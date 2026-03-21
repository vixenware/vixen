use std::{collections::BTreeSet, sync::Arc};

use facet_value::{VObject, Value as FacetValue};
use vx_resolve::TypeCollection;
use vx_types::{
    AliasTy, DeserContainerAttrs, DeserFieldAttrs, DeserRenameRule, EnumDef, EnumTy, NewtypeTy,
    OpaqueTy, Scalar, StructDef, StructTy, Ty, TypeDef, TypeId,
};

use crate::{
    CrossingPolicy, EvalError, EvalErrorKind, ProjectionStep, Provenance, ReuseClass, Value,
    ValueMeta, ValueRep,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StructuredFormat {
    Json,
    Toml,
}

impl StructuredFormat {
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Json => "Json.parse",
            Self::Toml => "Toml.parse",
        }
    }
}

pub(crate) fn parse_blob_as_value(
    format: StructuredFormat,
    blob: &Value,
    target: &Ty,
    types: &TypeCollection,
) -> Result<Value, EvalError> {
    let text = blob_text(blob, format)?;
    let parsed = match format {
        StructuredFormat::Json => facet_json::from_str::<FacetValue>(&text),
        StructuredFormat::Toml => facet_toml::from_str::<FacetValue>(&text),
    }
    .map_err(|error| {
        EvalError::new(
            EvalErrorKind::UserFail,
            format!("{} failed: {error}", format.display_name()),
        )
    })?;
    let context = DecodeContext {
        format,
        base_meta: &blob.meta,
        types,
    };
    context.decode_value(&parsed, target, blob.meta.provenance.clone())
}

struct DecodeContext<'a> {
    format: StructuredFormat,
    base_meta: &'a ValueMeta,
    types: &'a TypeCollection,
}

impl DecodeContext<'_> {
    fn decode_value(
        &self,
        value: &FacetValue,
        target: &Ty,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        match target {
            Ty::Unit => {
                if !value.is_null() {
                    return Err(type_mismatch("unit", value));
                }
                Ok(Value::unit().with_meta(self.leaf_meta(provenance)))
            }
            Ty::Scalar(scalar) => self.decode_scalar(value, *scalar, provenance),
            Ty::Optional(inner) => self.decode_optional(value, inner, provenance),
            Ty::Array(inner) => self.decode_array(value, inner, provenance),
            Ty::Map(key, value_ty) => self.decode_map(value, key, value_ty, provenance),
            Ty::Struct(struct_ty) => self.decode_struct(value, struct_ty, provenance),
            Ty::Enum(enum_ty) => self.decode_enum(value, enum_ty, provenance),
            Ty::Alias(alias_ty) => self.decode_alias(value, alias_ty, provenance),
            Ty::Newtype(newtype_ty) => self.decode_newtype(value, newtype_ty, provenance),
            Ty::Opaque(opaque_ty) => self.decode_opaque(value, opaque_ty),
            Ty::Tuple(_)
            | Ty::Function(_)
            | Ty::Stream(_)
            | Ty::Param(_)
            | Ty::Infer(_)
            | Ty::Poison => Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!(
                    "{} does not support target type {target:?} yet",
                    self.format.display_name()
                ),
            )),
        }
    }

    fn decode_scalar(
        &self,
        value: &FacetValue,
        scalar: Scalar,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let value = match scalar {
            Scalar::Bool => Value::bool(
                value
                    .as_bool()
                    .ok_or_else(|| type_mismatch("Bool", value))?,
            ),
            Scalar::String => Value::string(
                value
                    .as_string()
                    .ok_or_else(|| type_mismatch("String", value))?
                    .as_str(),
            ),
            Scalar::U64 => Value::u64(
                value
                    .as_number()
                    .and_then(|number| number.to_u64())
                    .ok_or_else(|| type_mismatch("U64", value))?,
            ),
            Scalar::I64 => {
                return Err(EvalError::new(
                    EvalErrorKind::UserFail,
                    "eval2 deserialization does not support I64 yet",
                ));
            }
            Scalar::F64 => {
                return Err(EvalError::new(
                    EvalErrorKind::UserFail,
                    "eval2 deserialization does not support F64 yet",
                ));
            }
        };
        Ok(value.with_meta(self.leaf_meta(provenance)))
    }

    fn decode_optional(
        &self,
        value: &FacetValue,
        inner: &Ty,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        if value.is_null() {
            return Ok(Value::structure("None", Vec::new()).with_meta(self.leaf_meta(provenance)));
        }
        let inner = self.decode_value(
            value,
            inner,
            provenance.project(ProjectionStep::Field("value".to_string())),
        )?;
        let meta = self.composite_meta(provenance, [&inner]);
        Ok(Value::structure("Some", vec![(Arc::from("value"), inner)]).with_meta(meta))
    }

    fn decode_array(
        &self,
        value: &FacetValue,
        inner: &Ty,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let array = value
            .as_array()
            .ok_or_else(|| type_mismatch("Array", value))?;
        let mut items = Vec::with_capacity(array.len());
        for (index, item) in array.iter().enumerate() {
            items.push(self.decode_value(
                item,
                inner,
                provenance.project(ProjectionStep::Index(index as u64)),
            )?);
        }
        let meta = self.composite_meta(provenance, items.iter());
        Ok(Value::array(items).with_meta(meta))
    }

    fn decode_map(
        &self,
        value: &FacetValue,
        key_ty: &Ty,
        value_ty: &Ty,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let object = value
            .as_object()
            .ok_or_else(|| type_mismatch("Map", value))?;
        let mut entries = Vec::with_capacity(object.len());
        for (key, entry_value) in object.iter() {
            let key_text = key.as_str();
            let key_provenance = provenance.project(ProjectionStep::MapKey(key_text.to_string()));
            let decoded_key = self.decode_object_key(key_text, key_ty, key_provenance.clone())?;
            let decoded_value = self.decode_value(entry_value, value_ty, key_provenance)?;
            entries.push((decoded_key, decoded_value));
        }
        let meta = self.map_meta(provenance, entries.iter());
        Ok(Value::map(entries).with_meta(meta))
    }

    fn decode_struct(
        &self,
        value: &FacetValue,
        struct_ty: &StructTy,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let TypeDef::Struct(def) = &self.types.env[struct_ty.id] else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "struct type id did not resolve to a struct definition",
            ));
        };
        let object = value
            .as_object()
            .ok_or_else(|| type_mismatch(def.name.as_str(), value))?;
        self.decode_struct_from_object(object, struct_ty, def, provenance)
    }

    fn decode_struct_from_object(
        &self,
        object: &VObject,
        struct_ty: &StructTy,
        def: &StructDef,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let mut fields = Vec::with_capacity(def.fields.len());
        let mut claimed_keys = def
            .fields
            .iter()
            .filter(|field| !field.attrs.flatten)
            .map(|field| decoded_field_name(def, field))
            .collect::<BTreeSet<_>>();
        for field in &def.fields {
            let field_ty = substitute_type_params(&field.ty, &struct_ty.type_args);
            let decoded_name = decoded_field_name(def, field);
            let field_provenance = provenance.project(ProjectionStep::Field(decoded_name.clone()));
            let decoded = if field.attrs.flatten {
                let decoded = self.decode_flattened_field(
                    object,
                    &field_ty,
                    field,
                    field_provenance,
                    &claimed_keys,
                )?;
                claimed_keys.extend(flatten_claimed_keys(self.types, &field_ty, field, def));
                decoded
            } else {
                match object.get(&decoded_name) {
                    Some(field_value) => {
                        self.decode_value(field_value, &field_ty, field_provenance)?
                    }
                    None if field.has_default => {
                        self.default_value(&field_ty, field_provenance.clone())?
                    }
                    None if matches!(field_ty, Ty::Optional(_)) => {
                        Value::structure("None", Vec::new())
                            .with_meta(self.leaf_meta(field_provenance))
                    }
                    None => {
                        return Err(EvalError::new(
                            EvalErrorKind::UserFail,
                            format!("missing field {} for {}", field.name, def.name),
                        ));
                    }
                }
            };
            fields.push((Arc::from(field.name.as_str()), decoded));
        }
        let meta = self.composite_meta(provenance, fields.iter().map(|(_, value)| value));
        Ok(Value::structure(Arc::from(def.name.as_str()), fields).with_meta(meta))
    }

    fn decode_enum(
        &self,
        value: &FacetValue,
        enum_ty: &EnumTy,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let TypeDef::Enum(def) = &self.types.env[enum_ty.id] else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "enum type id did not resolve to an enum definition",
            ));
        };
        if def.attrs.untagged {
            return self.decode_untagged_enum(value, enum_ty, def, provenance);
        }
        self.decode_externally_tagged_enum(value, enum_ty, def, provenance)
    }

    fn decode_externally_tagged_enum(
        &self,
        value: &FacetValue,
        enum_ty: &EnumTy,
        def: &EnumDef,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        if let Some(variant_name) = value.as_string().map(|name| name.as_str()) {
            let (_, variant) = find_variant_by_decoded_name(self.types, def, variant_name)?;
            if !variant.fields.is_empty() {
                return Err(EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("variant {variant_name} requires a payload"),
                ));
            }
            return Ok(
                Value::structure(Arc::from(variant.name.as_str()), Vec::new())
                    .with_meta(self.leaf_meta(provenance)),
            );
        }

        let object = value
            .as_object()
            .ok_or_else(|| type_mismatch(def.name.as_str(), value))?;
        if object.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!("enum {} must decode from a single-entry object", def.name),
            ));
        }
        let (variant_name, variant_value) = object
            .iter()
            .next()
            .expect("single-entry object should yield one item");
        let (variant_id, variant) =
            find_variant_by_decoded_name(self.types, def, variant_name.as_str())?;
        let variant_ty = StructTy {
            id: variant_id,
            type_args: enum_ty.type_args.clone(),
        };
        self.decode_variant_payload(
            variant_value,
            &variant_ty,
            variant,
            provenance.project(ProjectionStep::Field(variant_name.to_string())),
            true,
        )
    }

    fn decode_untagged_enum(
        &self,
        value: &FacetValue,
        enum_ty: &EnumTy,
        def: &EnumDef,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let mut last_error = None;
        for variant_id in &def.variants {
            let TypeDef::Struct(variant) = &self.types.env[*variant_id] else {
                continue;
            };
            let variant_ty = StructTy {
                id: *variant_id,
                type_args: enum_ty.type_args.clone(),
            };
            match self.decode_variant_payload(
                value,
                &variant_ty,
                variant,
                provenance.project(ProjectionStep::Field(decoded_variant_name(def, variant))),
                true,
            ) {
                Ok(decoded) => return Ok(decoded),
                Err(error) if error.kind == EvalErrorKind::UserFail => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("untagged enum {} did not match any variant", def.name),
            )
        }))
    }

    fn decode_variant_payload(
        &self,
        value: &FacetValue,
        variant_ty: &StructTy,
        variant: &StructDef,
        provenance: Provenance,
        allow_direct_single_field: bool,
    ) -> Result<Value, EvalError> {
        match variant.fields.as_slice() {
            [] => {
                if value.is_null() {
                    Ok(
                        Value::structure(Arc::from(variant.name.as_str()), Vec::new())
                            .with_meta(self.leaf_meta(provenance)),
                    )
                } else {
                    Err(EvalError::new(
                        EvalErrorKind::UserFail,
                        format!("variant {} requires no payload", variant.name),
                    ))
                }
            }
            [field] if allow_direct_single_field => {
                if value.as_object().is_some() {
                    return self.decode_struct(value, variant_ty, provenance);
                }
                let field_ty = substitute_type_params(&field.ty, &variant_ty.type_args);
                let decoded = self.decode_value(
                    value,
                    &field_ty,
                    provenance.project(ProjectionStep::Field(field.name.clone())),
                )?;
                let meta = self.composite_meta(provenance, [&decoded]);
                Ok(Value::structure(
                    Arc::from(variant.name.as_str()),
                    vec![(Arc::from(field.name.as_str()), decoded)],
                )
                .with_meta(meta))
            }
            _ => self.decode_struct(value, variant_ty, provenance),
        }
    }

    fn decode_alias(
        &self,
        value: &FacetValue,
        alias_ty: &AliasTy,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let TypeDef::Alias(def) = &self.types.env[alias_ty.id] else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "alias type id did not resolve to an alias definition",
            ));
        };
        let target = substitute_type_params(&def.target, &alias_ty.type_args);
        self.decode_value(value, &target, provenance)
    }

    fn decode_newtype(
        &self,
        value: &FacetValue,
        newtype_ty: &NewtypeTy,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        let TypeDef::Newtype(def) = &self.types.env[newtype_ty.id] else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "newtype id did not resolve to a newtype definition",
            ));
        };
        let target = substitute_type_params(&def.inner, &newtype_ty.type_args);
        self.decode_value(value, &target, provenance)
    }

    fn decode_opaque(&self, _value: &FacetValue, opaque_ty: &OpaqueTy) -> Result<Value, EvalError> {
        let TypeDef::Opaque(def) = &self.types.env[opaque_ty.id] else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "opaque type id did not resolve to an opaque definition",
            ));
        };
        Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!(
                "{} does not support opaque target type {}",
                self.format.display_name(),
                def.name
            ),
        ))
    }

    fn decode_object_key(
        &self,
        key: &str,
        target: &Ty,
        provenance: Provenance,
    ) -> Result<Value, EvalError> {
        match target {
            Ty::Scalar(Scalar::String) => {
                Ok(Value::string(key).with_meta(self.leaf_meta(provenance)))
            }
            Ty::Scalar(Scalar::U64) => Ok(Value::u64(key.parse::<u64>().map_err(|_| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("map key {key:?} is not a valid U64"),
                )
            })?)
            .with_meta(self.leaf_meta(provenance))),
            _ => Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!("JSON/TOML object keys cannot decode into {target:?}"),
            )),
        }
    }

    fn leaf_meta(&self, provenance: Provenance) -> ValueMeta {
        ValueMeta::new(
            provenance,
            self.base_meta.reuse.clone(),
            self.base_meta.crossing.clone(),
        )
        .with_claims(self.base_meta.claims.iter().cloned())
    }

    fn composite_meta<'a>(
        &self,
        provenance: Provenance,
        children: impl IntoIterator<Item = &'a Value>,
    ) -> ValueMeta {
        aggregate_meta(provenance, children, self.base_meta)
    }

    fn map_meta<'a>(
        &self,
        provenance: Provenance,
        entries: impl IntoIterator<Item = &'a (Value, Value)>,
    ) -> ValueMeta {
        let mut meta = self.leaf_meta(provenance);
        let mut saw_blocked = meta.crossing == CrossingPolicy::Blocked;
        for (key, value) in entries {
            meta.reuse = join_reuse(meta.reuse.clone(), key.meta.reuse.clone());
            meta.reuse = join_reuse(meta.reuse.clone(), value.meta.reuse.clone());
            meta = meta.with_claims(key.meta.claims.iter().cloned());
            meta = meta.with_claims(value.meta.claims.iter().cloned());
            saw_blocked |= key.meta.crossing == CrossingPolicy::Blocked;
            saw_blocked |= value.meta.crossing == CrossingPolicy::Blocked;
        }
        if saw_blocked {
            meta.crossing = CrossingPolicy::Blocked;
        }
        meta
    }

    fn decode_flattened_field(
        &self,
        object: &VObject,
        target: &Ty,
        field: &vx_types::FieldDef,
        provenance: Provenance,
        claimed_keys: &BTreeSet<String>,
    ) -> Result<Value, EvalError> {
        match target {
            Ty::Struct(struct_ty) => self.decode_struct_from_object(
                object,
                struct_ty,
                struct_def(self.types, struct_ty.id)?,
                provenance,
            ),
            Ty::Map(key_ty, value_ty) if matches!(key_ty.as_ref(), Ty::Scalar(Scalar::String)) => {
                let mut entries = Vec::new();
                for (key, entry_value) in object.iter() {
                    if claimed_keys.contains(key.as_str()) {
                        continue;
                    }
                    let key_provenance =
                        provenance.project(ProjectionStep::MapKey(key.as_str().to_string()));
                    let decoded_key =
                        self.decode_object_key(key.as_str(), key_ty, key_provenance.clone())?;
                    let decoded_value = self.decode_value(entry_value, value_ty, key_provenance)?;
                    entries.push((decoded_key, decoded_value));
                }
                let meta = self.map_meta(provenance, entries.iter());
                Ok(Value::map(entries).with_meta(meta))
            }
            _ => Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!(
                    "flattened field {} must be a struct or Map[String, _]",
                    field.name
                ),
            )),
        }
    }

    fn default_value(&self, target: &Ty, provenance: Provenance) -> Result<Value, EvalError> {
        match target {
            Ty::Scalar(Scalar::Bool) => {
                Ok(Value::bool(false).with_meta(self.leaf_meta(provenance)))
            }
            Ty::Scalar(Scalar::String) => {
                Ok(Value::string("").with_meta(self.leaf_meta(provenance)))
            }
            Ty::Scalar(Scalar::U64) => Ok(Value::u64(0).with_meta(self.leaf_meta(provenance))),
            Ty::Optional(_) => {
                Ok(Value::structure("None", Vec::new()).with_meta(self.leaf_meta(provenance)))
            }
            Ty::Array(_) => Ok(Value::array(Vec::new()).with_meta(self.leaf_meta(provenance))),
            Ty::Map(_, _) => Ok(Value::map(Vec::new()).with_meta(self.leaf_meta(provenance))),
            Ty::Alias(alias_ty) => {
                let TypeDef::Alias(def) = &self.types.env[alias_ty.id] else {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        "alias type id did not resolve to an alias definition",
                    ));
                };
                let target = substitute_type_params(&def.target, &alias_ty.type_args);
                self.default_value(&target, provenance)
            }
            Ty::Newtype(newtype_ty) => {
                let TypeDef::Newtype(def) = &self.types.env[newtype_ty.id] else {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        "newtype id did not resolve to a newtype definition",
                    ));
                };
                let target = substitute_type_params(&def.inner, &newtype_ty.type_args);
                self.default_value(&target, provenance)
            }
            _ => Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!("missing field default for unsupported target type {target:?}"),
            )),
        }
    }
}

fn decoded_field_name(container: &StructDef, field: &vx_types::FieldDef) -> String {
    field
        .attrs
        .rename
        .clone()
        .unwrap_or_else(|| renamed_identifier(&field.name, &container.attrs, &field.attrs))
}

fn decoded_variant_name(container: &EnumDef, variant: &StructDef) -> String {
    variant.attrs.rename.clone().unwrap_or_else(|| {
        renamed_identifier(&variant.name, &container.attrs, &DeserFieldAttrs::default())
    })
}

fn renamed_identifier(
    fallback: &str,
    container_attrs: &DeserContainerAttrs,
    _field_attrs: &DeserFieldAttrs,
) -> String {
    container_attrs
        .rename_all
        .map(|rule| apply_rename_rule(fallback, rule))
        .unwrap_or_else(|| fallback.to_string())
}

fn apply_rename_rule(name: &str, rule: DeserRenameRule) -> String {
    let words = split_identifier_words(name);
    match rule {
        DeserRenameRule::SnakeCase => words.join("_"),
        DeserRenameRule::KebabCase => words.join("-"),
        DeserRenameRule::ScreamingSnakeCase => words
            .iter()
            .map(|word| word.to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join("_"),
        DeserRenameRule::CamelCase => {
            let mut iter = words.into_iter();
            let Some(first) = iter.next() else {
                return String::new();
            };
            let mut output = first;
            for word in iter {
                output.push_str(&uppercase_first(&word));
            }
            output
        }
        DeserRenameRule::PascalCase => words
            .into_iter()
            .map(|word| uppercase_first(&word))
            .collect::<String>(),
    }
}

fn split_identifier_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let chars = name.chars().collect::<Vec<_>>();
    for (index, ch) in chars.iter().copied().enumerate() {
        if matches!(ch, '_' | '-' | ' ') {
            if !current.is_empty() {
                words.push(current);
                current = String::new();
            }
            continue;
        }
        let next = chars.get(index + 1).copied();
        let is_upper = ch.is_ascii_uppercase();
        let previous_is_word = current
            .chars()
            .last()
            .map(|prev| prev.is_ascii_lowercase() || prev.is_ascii_digit())
            .unwrap_or(false);
        let next_is_lower = next
            .map(|value| value.is_ascii_lowercase())
            .unwrap_or(false);
        if is_upper && !current.is_empty() && (previous_is_word || next_is_lower) {
            words.push(current);
            current = String::new();
        }
        current.push(ch.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn uppercase_first(word: &str) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut output = String::new();
    output.push(first.to_ascii_uppercase());
    output.extend(chars);
    output
}

fn blob_text(blob: &Value, format: StructuredFormat) -> Result<String, EvalError> {
    match &blob.rep {
        ValueRep::External(external) if external.kind.as_ref() == "Blob" => {
            Ok(external.descriptor.to_string())
        }
        ValueRep::String(text) => Ok(text.to_string()),
        _ => Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "{} requires a Blob or String argument",
                format.display_name()
            ),
        )),
    }
}

fn flatten_claimed_keys(
    types: &TypeCollection,
    target: &Ty,
    field: &vx_types::FieldDef,
    parent: &StructDef,
) -> BTreeSet<String> {
    match target {
        Ty::Struct(struct_ty) => collect_struct_flatten_keys(types, struct_ty),
        Ty::Map(key_ty, _) if matches!(key_ty.as_ref(), Ty::Scalar(Scalar::String)) => {
            BTreeSet::new()
        }
        _ => {
            let mut fallback = BTreeSet::new();
            fallback.insert(decoded_field_name(parent, field));
            fallback
        }
    }
}

fn collect_struct_flatten_keys(types: &TypeCollection, struct_ty: &StructTy) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let Ok(def) = struct_def(types, struct_ty.id) else {
        return keys;
    };
    for field in &def.fields {
        let field_ty = substitute_type_params(&field.ty, &struct_ty.type_args);
        if field.attrs.flatten {
            keys.extend(flatten_claimed_keys(types, &field_ty, field, def));
        } else {
            keys.insert(decoded_field_name(def, field));
        }
    }
    keys
}

fn struct_def<'a>(types: &'a TypeCollection, id: TypeId) -> Result<&'a StructDef, EvalError> {
    let TypeDef::Struct(def) = &types.env[id] else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "struct type id did not resolve to a struct definition",
        ));
    };
    Ok(def)
}

fn type_mismatch(expected: &str, actual: &FacetValue) -> EvalError {
    EvalError::new(
        EvalErrorKind::UserFail,
        format!("expected {expected}, got {:?}", actual.value_type()),
    )
}

fn aggregate_meta<'a>(
    provenance: Provenance,
    children: impl IntoIterator<Item = &'a Value>,
    base_meta: &ValueMeta,
) -> ValueMeta {
    let mut meta = ValueMeta::new(
        provenance,
        base_meta.reuse.clone(),
        base_meta.crossing.clone(),
    )
    .with_claims(base_meta.claims.iter().cloned());
    let mut saw_blocked = meta.crossing == CrossingPolicy::Blocked;

    for child in children {
        meta.reuse = join_reuse(meta.reuse.clone(), child.meta.reuse.clone());
        meta = meta.with_claims(child.meta.claims.iter().cloned());
        saw_blocked |= child.meta.crossing == CrossingPolicy::Blocked;
    }

    if saw_blocked {
        meta.crossing = CrossingPolicy::Blocked;
    }

    meta
}

fn join_reuse(left: ReuseClass, right: ReuseClass) -> ReuseClass {
    match (left, right) {
        (ReuseClass::Live, _) | (_, ReuseClass::Live) => ReuseClass::Live,
        (ReuseClass::Observed, _) | (_, ReuseClass::Observed) => ReuseClass::Observed,
        (ReuseClass::Stable, ReuseClass::Stable) => ReuseClass::Stable,
    }
}

fn substitute_type_params(ty: &Ty, type_args: &[Ty]) -> Ty {
    match ty {
        Ty::Param(param) => type_args
            .get(param.index as usize)
            .cloned()
            .unwrap_or(Ty::Poison),
        Ty::Optional(inner) => Ty::Optional(Box::new(substitute_type_params(inner, type_args))),
        Ty::Array(inner) => Ty::Array(Box::new(substitute_type_params(inner, type_args))),
        Ty::Map(key, value) => Ty::Map(
            Box::new(substitute_type_params(key, type_args)),
            Box::new(substitute_type_params(value, type_args)),
        ),
        Ty::Tuple(tuple) => Ty::Tuple(vx_types::TupleTy {
            elements: tuple
                .elements
                .iter()
                .map(|element| substitute_type_params(element, type_args))
                .collect(),
        }),
        Ty::Struct(struct_ty) => Ty::Struct(StructTy {
            id: struct_ty.id,
            type_args: struct_ty
                .type_args
                .iter()
                .map(|arg| substitute_type_params(arg, type_args))
                .collect(),
        }),
        Ty::Enum(enum_ty) => Ty::Enum(EnumTy {
            id: enum_ty.id,
            type_args: enum_ty
                .type_args
                .iter()
                .map(|arg| substitute_type_params(arg, type_args))
                .collect(),
        }),
        Ty::Newtype(newtype_ty) => Ty::Newtype(vx_types::NewtypeTy {
            id: newtype_ty.id,
            type_args: newtype_ty
                .type_args
                .iter()
                .map(|arg| substitute_type_params(arg, type_args))
                .collect(),
        }),
        Ty::Alias(alias_ty) => Ty::Alias(vx_types::AliasTy {
            id: alias_ty.id,
            type_args: alias_ty
                .type_args
                .iter()
                .map(|arg| substitute_type_params(arg, type_args))
                .collect(),
        }),
        Ty::Opaque(opaque_ty) => Ty::Opaque(vx_types::OpaqueTy {
            id: opaque_ty.id,
            type_args: opaque_ty
                .type_args
                .iter()
                .map(|arg| substitute_type_params(arg, type_args))
                .collect(),
        }),
        other => other.clone(),
    }
}

fn find_variant<'a>(
    types: &'a TypeCollection,
    def: &'a EnumDef,
    variant_name: &str,
) -> Result<(TypeId, &'a StructDef), EvalError> {
    for variant_id in &def.variants {
        let TypeDef::Struct(variant) = &types.env[*variant_id] else {
            continue;
        };
        if variant.name == variant_name {
            return Ok((*variant_id, variant));
        }
    }
    Err(EvalError::new(
        EvalErrorKind::UserFail,
        format!("variant {variant_name} was not found in {}", def.name),
    ))
}

fn find_variant_by_decoded_name<'a>(
    types: &'a TypeCollection,
    def: &'a EnumDef,
    variant_name: &str,
) -> Result<(TypeId, &'a StructDef), EvalError> {
    for variant_id in &def.variants {
        let TypeDef::Struct(variant) = &types.env[*variant_id] else {
            continue;
        };
        if decoded_variant_name(def, variant) == variant_name {
            return Ok((*variant_id, variant));
        }
    }
    find_variant(types, def, variant_name)
}
