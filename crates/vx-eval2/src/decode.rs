use std::sync::Arc;

use facet_value::Value as FacetValue;
use vx_resolve::TypeCollection;
use vx_types::{
    AliasTy, EnumDef, EnumTy, NewtypeTy, OpaqueTy, Scalar, StructDef, StructTy, Ty, TypeDef, TypeId,
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
        let mut fields = Vec::with_capacity(def.fields.len());
        for field in &def.fields {
            let field_ty = substitute_type_params(&field.ty, &struct_ty.type_args);
            let field_provenance = provenance.project(ProjectionStep::Field(field.name.clone()));
            let decoded = match object.get(&field.name) {
                Some(field_value) => self.decode_value(field_value, &field_ty, field_provenance)?,
                None if matches!(field_ty, Ty::Optional(_)) => {
                    Value::structure("None", Vec::new()).with_meta(self.leaf_meta(field_provenance))
                }
                None => {
                    return Err(EvalError::new(
                        EvalErrorKind::UserFail,
                        format!("missing field {} for {}", field.name, def.name),
                    ));
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
        if let Some(variant_name) = value.as_string().map(|name| name.as_str()) {
            let (_, variant) = find_variant(self.types, def, variant_name)?;
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
        let (variant_id, variant) = find_variant(self.types, def, variant_name.as_str())?;
        let variant_ty = StructTy {
            id: variant_id,
            type_args: enum_ty.type_args.clone(),
        };
        self.decode_struct(
            variant_value,
            &variant_ty,
            provenance.project(ProjectionStep::Field(variant.name.clone())),
        )
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
}

fn blob_text(blob: &Value, format: StructuredFormat) -> Result<String, EvalError> {
    match &blob.rep {
        ValueRep::External(external) if external.kind.as_ref() == "Blob" => {
            Ok(external.descriptor.to_string())
        }
        _ => Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{} requires a Blob argument", format.display_name()),
        )),
    }
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
