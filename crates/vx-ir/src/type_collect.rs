//! Type collection on VIR: register type declarations and build signatures.
//!
//! Reads from VIR items and produces a [`TypeCollection`] with TypeEnv,
//! def-to-type / def-to-fn-sig maps, and sum-alias alternatives.

use std::collections::HashMap;

use vx_parser::SourceId;
use vx_types::{
    AliasDef, DeserContainerAttrs, DeserFieldAttrs, DeserRenameRule, EnumDef, FieldDef,
    FieldDefaultValue, FnSig, KeywordParamDef, NewtypeDef, OpaqueDef, ParamDef, Scalar, StructDef,
    Ty, TypeDef, TypeEnv, TypeId,
};

use crate::arena::VirArena;
use crate::body::Param;
use crate::def::DefSite;
use crate::expr::{ExprKind, LitKind};
use crate::item::{
    Attribute, EnumItem, FieldDecl, FunctionItem, ItemKind, NamespaceItem, NewtypeItem, StructItem,
    TypeAliasItem, VariantDecl,
};
use crate::pattern::PatternKind;
use crate::type_collection::{TypeCollection, TypeKind};
use crate::type_ref::TypeRefKind;
use crate::{ItemId, SourceUnitId, TypeRefId};

/// Collect all type declarations from VIR into a [`TypeCollection`].
pub fn collect_types_vir(arena: &mut VirArena) -> TypeCollection {
    let mut env = TypeEnv::new();
    let mut def_to_type = HashMap::new();
    let mut def_to_fn_sig = HashMap::new();
    let mut alias_sum_alternatives = HashMap::new();
    let mut name_to_id: HashMap<String, (TypeId, TypeKind)> = HashMap::new();

    // Clear all item type_ids before populating.
    for item in &mut arena.items {
        match &mut item.kind {
            ItemKind::Struct(s) => s.type_id = None,
            ItemKind::Enum(e) => e.type_id = None,
            ItemKind::Newtype(n) => n.type_id = None,
            ItemKind::TypeAlias(t) => t.type_id = None,
            ItemKind::Function(f) => f.type_id = None,
            ItemKind::Namespace(_) | ItemKind::Error => {}
        }
    }

    // Collect all source unit IDs for iteration.
    let su_ids: Vec<SourceUnitId> = (0..arena.source_units.len())
        .map(SourceUnitId::from_index)
        .collect();

    // -----------------------------------------------------------------------
    // Pass 1: Register type declarations with placeholders.
    // -----------------------------------------------------------------------
    for &su_id in &su_ids {
        let source_id = arena[su_id].source;
        let items: Vec<ItemId> = arena[su_id].items.clone();
        register_type_decls_recursive(
            arena,
            &items,
            source_id,
            &mut env,
            &mut name_to_id,
            &mut def_to_type,
        );
    }

    // -----------------------------------------------------------------------
    // Pass 2: Fill in field types, variant types, and function signatures.
    // -----------------------------------------------------------------------
    for &su_id in &su_ids {
        let source_id = arena[su_id].source;
        let items: Vec<ItemId> = arena[su_id].items.clone();
        fill_types_recursive(
            arena,
            &items,
            source_id,
            &name_to_id,
            &mut env,
            &mut def_to_type,
            &mut def_to_fn_sig,
            &mut alias_sum_alternatives,
            &[],  // no parent type params at top level
            None, // no self_ty at top level
        );
    }

    // Pass 3: Collect FnSigs for non-namespace top-level functions that weren't
    // already collected (block-local functions, etc.)
    // The old collector does a descendant walk for FnDecls not inside namespaces.
    // For VIR, we've already handled all items via recursive traversal, so this
    // should be covered. But for parity, we skip duplicates.

    // Write type_ids back onto items.
    for item in &mut arena.items {
        match &mut item.kind {
            ItemKind::Struct(s) => {
                if let Some(&id) = def_to_type.get(&s.name_span) {
                    s.type_id = Some(id);
                }
            }
            ItemKind::Enum(e) => {
                if let Some(&id) = def_to_type.get(&e.name_span) {
                    e.type_id = Some(id);
                }
            }
            ItemKind::Newtype(n) => {
                if let Some(&id) = def_to_type.get(&n.name_span) {
                    n.type_id = Some(id);
                }
            }
            ItemKind::TypeAlias(t) => {
                if let Some(&id) = def_to_type.get(&t.name_span) {
                    t.type_id = Some(id);
                }
            }
            _ => {}
        }
    }

    TypeCollection {
        env,
        def_to_type,
        def_to_fn_sig,
        name_to_id,
        alias_sum_alternatives,
    }
}

// ---------------------------------------------------------------------------
// Pass 1 helpers
// ---------------------------------------------------------------------------

fn register_type_decls_recursive(
    arena: &VirArena,
    items: &[ItemId],
    source_id: SourceId,
    env: &mut TypeEnv,
    name_to_id: &mut HashMap<String, (TypeId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeId>,
) {
    for &item_id in items {
        let item = &arena[item_id];
        match &item.kind {
            ItemKind::Struct(s) => {
                register_struct(arena, s, source_id, env, name_to_id, def_to_type);
            }
            ItemKind::Enum(e) => {
                register_enum(arena, e, source_id, env, name_to_id, def_to_type);
            }
            ItemKind::Newtype(n) => {
                register_newtype(n, source_id, env, name_to_id, def_to_type);
            }
            ItemKind::TypeAlias(t) => {
                register_type_alias(t, source_id, env, name_to_id, def_to_type);
            }
            ItemKind::Namespace(ns) => {
                let child_items: Vec<ItemId> = ns.items.clone();
                register_type_decls_recursive(
                    arena,
                    &child_items,
                    source_id,
                    env,
                    name_to_id,
                    def_to_type,
                );
            }
            ItemKind::Function(_) | ItemKind::Error => {}
        }
    }
}

fn register_struct(
    arena: &VirArena,
    s: &StructItem,
    _source_id: SourceId,
    env: &mut TypeEnv,
    name_to_id: &mut HashMap<String, (TypeId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeId>,
) {
    let id = env.define(
        &s.name,
        TypeDef::Struct(StructDef {
            name: s.name.clone(),
            parent_enum: None,
            type_params: s.type_params.clone(),
            attrs: collect_container_attrs_vir(arena, &s.attributes),
            fields: Vec::new(), // filled in pass 2
        }),
    );
    name_to_id.insert(s.name.clone(), (id, TypeKind::Struct));
    def_to_type.insert(s.name_span, id);
}

fn register_enum(
    arena: &VirArena,
    e: &EnumItem,
    _source_id: SourceId,
    env: &mut TypeEnv,
    name_to_id: &mut HashMap<String, (TypeId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeId>,
) {
    let id = env.define(
        &e.name,
        TypeDef::Enum(EnumDef {
            name: e.name.clone(),
            type_params: e.type_params.clone(),
            attrs: collect_container_attrs_vir(arena, &e.attributes),
            variants: Vec::new(), // filled in pass 2
        }),
    );
    name_to_id.insert(e.name.clone(), (id, TypeKind::Enum));
    def_to_type.insert(e.name_span, id);
}

fn register_newtype(
    n: &NewtypeItem,
    _source_id: SourceId,
    env: &mut TypeEnv,
    name_to_id: &mut HashMap<String, (TypeId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeId>,
) {
    let id = env.define(
        &n.name,
        TypeDef::Newtype(NewtypeDef {
            name: n.name.clone(),
            type_params: n.type_params.clone(),
            inner: Ty::Poison, // filled in pass 2
        }),
    );
    name_to_id.insert(n.name.clone(), (id, TypeKind::Newtype));
    def_to_type.insert(n.name_span, id);
}

fn register_type_alias(
    t: &TypeAliasItem,
    _source_id: SourceId,
    env: &mut TypeEnv,
    name_to_id: &mut HashMap<String, (TypeId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeId>,
) {
    let (def, kind) = if t.aliased_ty.is_some() {
        (
            TypeDef::Alias(AliasDef {
                name: t.name.clone(),
                type_params: t.type_params.clone(),
                target: Ty::Poison, // filled in pass 2
            }),
            TypeKind::Alias,
        )
    } else {
        (
            TypeDef::Opaque(OpaqueDef {
                name: t.name.clone(),
                type_params: t.type_params.clone(),
            }),
            TypeKind::Opaque,
        )
    };
    let id = env.define(&t.name, def);
    name_to_id.insert(t.name.clone(), (id, kind));
    def_to_type.insert(t.name_span, id);
}

// ---------------------------------------------------------------------------
// Pass 2 helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn fill_types_recursive(
    arena: &VirArena,
    items: &[ItemId],
    source_id: SourceId,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    env: &mut TypeEnv,
    def_to_type: &mut HashMap<DefSite, TypeId>,
    def_to_fn_sig: &mut HashMap<DefSite, FnSig>,
    alias_sum_alternatives: &mut HashMap<TypeId, Vec<Ty>>,
    parent_type_params: &[String],
    parent_self_ty: Option<&Ty>,
) {
    for &item_id in items {
        let item = &arena[item_id];
        match &item.kind {
            ItemKind::Struct(s) => {
                fill_struct(arena, s, name_to_id, env);
            }
            ItemKind::Enum(e) => {
                fill_enum(arena, e, source_id, name_to_id, env, def_to_type);
            }
            ItemKind::Newtype(n) => {
                fill_newtype(arena, n, name_to_id, env);
            }
            ItemKind::TypeAlias(t) => {
                fill_type_alias(arena, t, name_to_id, env, alias_sum_alternatives);
            }
            ItemKind::Function(f) => {
                fill_function(
                    arena,
                    f,
                    name_to_id,
                    def_to_fn_sig,
                    parent_type_params,
                    parent_self_ty,
                );
            }
            ItemKind::Namespace(ns) => {
                fill_namespace(
                    arena,
                    ns,
                    source_id,
                    name_to_id,
                    env,
                    def_to_type,
                    def_to_fn_sig,
                    alias_sum_alternatives,
                );
            }
            ItemKind::Error => {}
        }
    }
}

fn fill_struct(
    arena: &VirArena,
    s: &StructItem,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    env: &mut TypeEnv,
) {
    let (id, _) = name_to_id[&s.name];
    let fields = collect_field_defs(arena, &s.fields, name_to_id, &s.type_params);
    if let TypeDef::Struct(ref mut sd) = env[id] {
        sd.fields = fields;
    }
}

fn fill_enum(
    arena: &VirArena,
    e: &EnumItem,
    _source_id: SourceId,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    env: &mut TypeEnv,
    def_to_type: &mut HashMap<DefSite, TypeId>,
) {
    let (enum_id, _) = name_to_id[&e.name];
    let mut variant_ids = Vec::new();

    for variant in &e.variants {
        let fields = collect_variant_field_defs(arena, variant, name_to_id, &e.type_params);
        let vid = env.define(
            &variant.name,
            TypeDef::Struct(StructDef {
                name: variant.name.clone(),
                parent_enum: Some(enum_id),
                type_params: e.type_params.clone(),
                attrs: collect_container_attrs_vir(arena, &variant.attributes),
                fields,
            }),
        );
        variant_ids.push(vid);
        def_to_type.insert(variant.name_span, vid);
    }

    if let TypeDef::Enum(ref mut ed) = env[enum_id] {
        ed.variants = variant_ids;
    }
}

fn fill_newtype(
    arena: &VirArena,
    n: &NewtypeItem,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    env: &mut TypeEnv,
) {
    let (id, _) = name_to_id[&n.name];
    let inner = n
        .inner_ty
        .map(|tr| resolve_type_ref(arena, tr, name_to_id, &n.type_params, None))
        .unwrap_or(Ty::Poison);
    if let TypeDef::Newtype(ref mut nd) = env[id] {
        nd.inner = inner;
    }
}

fn fill_type_alias(
    arena: &VirArena,
    t: &TypeAliasItem,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    env: &mut TypeEnv,
    alias_sum_alternatives: &mut HashMap<TypeId, Vec<Ty>>,
) {
    let Some(aliased_tr) = t.aliased_ty else {
        return;
    };
    let (id, _) = name_to_id[&t.name];
    let target = resolve_type_ref(arena, aliased_tr, name_to_id, &t.type_params, None);

    // If the aliased type is a sum, record the alternatives.
    if let TypeRefKind::Sum(ref sum) = arena.type_refs[aliased_tr.0 as usize].kind {
        let alternatives = sum
            .alternatives
            .iter()
            .map(|&alt| resolve_type_ref(arena, alt, name_to_id, &t.type_params, None))
            .collect();
        alias_sum_alternatives.insert(id, alternatives);
    }

    if let TypeDef::Alias(ref mut ad) = env[id] {
        ad.target = target;
    }
}

fn fill_function(
    arena: &VirArena,
    f: &FunctionItem,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    def_to_fn_sig: &mut HashMap<DefSite, FnSig>,
    parent_type_params: &[String],
    self_ty: Option<&Ty>,
) {
    // Merge parent type params (from namespace) with function-level type params.
    let mut type_params: Vec<String> = parent_type_params.to_vec();
    type_params.extend(f.type_params.iter().cloned());

    let sig = collect_fn_sig_from_body(arena, f, name_to_id, &type_params, self_ty);
    def_to_fn_sig.insert(f.name_span, sig);
}

#[allow(clippy::too_many_arguments)]
fn fill_namespace(
    arena: &VirArena,
    ns: &NamespaceItem,
    source_id: SourceId,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    env: &mut TypeEnv,
    def_to_type: &mut HashMap<DefSite, TypeId>,
    def_to_fn_sig: &mut HashMap<DefSite, FnSig>,
    alias_sum_alternatives: &mut HashMap<TypeId, Vec<Ty>>,
) {
    let namespace_type_params = &ns.type_params;

    // Compute namespace self_ty: resolve the namespace name as a type with type params.
    let namespace_self_ty = compute_namespace_self_ty(&ns.name, namespace_type_params, name_to_id);

    let child_items: Vec<ItemId> = ns.items.clone();
    fill_types_recursive(
        arena,
        &child_items,
        source_id,
        name_to_id,
        env,
        def_to_type,
        def_to_fn_sig,
        alias_sum_alternatives,
        namespace_type_params,
        namespace_self_ty.as_ref(),
    );
}

// ---------------------------------------------------------------------------
// FnSig collection
// ---------------------------------------------------------------------------

fn collect_fn_sig_from_body(
    arena: &VirArena,
    func: &FunctionItem,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    type_params: &[String],
    self_ty: Option<&Ty>,
) -> FnSig {
    let body = &arena[func.body];

    let mut positional_params: Vec<ParamDef> = Vec::new();
    let mut keyword_params: Vec<KeywordParamDef> = Vec::new();

    for param in &body.params {
        let pname = param_name(arena, param);
        let pname_span = param_name_span(arena, param);
        let ty = param
            .ty
            .map(|tr| resolve_type_ref(arena, tr, name_to_id, type_params, self_ty))
            .or_else(|| (pname == "self").then(|| self_ty.cloned()).flatten())
            .unwrap_or(Ty::Poison);

        if param.is_keyword {
            // Keyword param: check if it has a default value.
            let has_default = param_has_default(arena, param);
            keyword_params.push(KeywordParamDef {
                name: pname,
                name_span: pname_span,
                ty,
                has_default,
                doc: param.doc.clone(),
            });
        } else {
            positional_params.push(ParamDef {
                name: pname,
                name_span: pname_span,
                ty,
                doc: param.doc.clone(),
            });
        }
    }

    let ret = body
        .return_ty
        .map(|tr| resolve_type_ref(arena, tr, name_to_id, type_params, self_ty))
        .unwrap_or(Ty::Unit);

    let capabilities = body
        .capabilities
        .iter()
        .map(|tag| vx_types::Capability {
            name: tag.name.clone(),
            name_span: tag.name_span,
        })
        .collect();

    FnSig {
        name: func.name.clone(),
        name_span: func.name_span,
        type_params: type_params.to_vec(),
        positional_params,
        keyword_params,
        ret,
        capabilities,
    }
}

/// Extract the name from a parameter's pattern.
fn param_name(arena: &VirArena, param: &Param) -> String {
    let pat = &arena.patterns[param.pattern.0 as usize];
    match &pat.kind {
        PatternKind::Binding(b) => arena.bindings[b.binding.0 as usize].name.clone(),
        PatternKind::Wildcard => "_".to_string(),
        _ => String::new(),
    }
}

/// Extract the name span from a parameter's pattern.
fn param_name_span(arena: &VirArena, param: &Param) -> vx_parser::SourceSpan {
    let pat = &arena.patterns[param.pattern.0 as usize];
    match &pat.kind {
        PatternKind::Binding(b) => arena.bindings[b.binding.0 as usize].name_span,
        _ => pat.span,
    }
}

/// Check if a keyword parameter has a default value.
fn param_has_default(_arena: &VirArena, param: &Param) -> bool {
    param.has_default
}

// ---------------------------------------------------------------------------
// Namespace self_ty
// ---------------------------------------------------------------------------

fn compute_namespace_self_ty(
    name: &str,
    type_params: &[String],
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
) -> Option<Ty> {
    let type_args: Vec<Ty> = type_params
        .iter()
        .enumerate()
        .map(|(index, name)| {
            Ty::Param(vx_types::TypeParam {
                name: name.clone(),
                index: index as u32,
            })
        })
        .collect();
    resolve_named_type(name, type_args, name_to_id)
}

// ---------------------------------------------------------------------------
// Type ref resolution — exact clone of TypeResolver::resolve_type()
// ---------------------------------------------------------------------------

/// Resolve a VIR TypeRef to a Ty.
///
/// This intentionally mirrors the old collector's `TypeResolver::resolve_type_in_context`
/// exactly, including limitations like ignoring multi-segment paths.
pub fn resolve_type_ref(
    arena: &VirArena,
    type_ref_id: TypeRefId,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    type_params: &[String],
    self_ty: Option<&Ty>,
) -> Ty {
    let tr = &arena.type_refs[type_ref_id.0 as usize];
    match &tr.kind {
        TypeRefKind::Path(path) => resolve_type_path(
            arena,
            &path.segments,
            &path.type_args,
            name_to_id,
            type_params,
            self_ty,
        ),
        TypeRefKind::Optional(opt) => {
            let inner = resolve_type_ref(arena, opt.inner, name_to_id, type_params, self_ty);
            Ty::Optional(Box::new(inner))
        }
        TypeRefKind::Tuple(tup) => {
            let elements: Vec<Ty> = tup
                .elements
                .iter()
                .map(|&e| resolve_type_ref(arena, e, name_to_id, type_params, self_ty))
                .collect();
            Ty::Tuple(vx_types::TupleTy { elements })
        }
        TypeRefKind::Function(func) => {
            let params: Vec<Ty> = func
                .params
                .iter()
                .map(|&p| resolve_type_ref(arena, p, name_to_id, type_params, self_ty))
                .collect();
            let ret = resolve_type_ref(arena, func.ret, name_to_id, type_params, self_ty);
            let capabilities = func
                .capabilities
                .iter()
                .map(|tag| vx_types::Capability {
                    name: tag.name.clone(),
                    name_span: tag.name_span,
                })
                .collect();
            Ty::Function(vx_types::FunctionTy {
                params,
                ret: Box::new(ret),
                capabilities,
            })
        }
        TypeRefKind::Sum(sum) => {
            // Sum types: resolve each variant. Same behavior as old collector —
            // returns first alternative if multiple (sum types are stored as
            // AliasDef targets, not as a Ty variant).
            let alts: Vec<Ty> = sum
                .alternatives
                .iter()
                .map(|&a| resolve_type_ref(arena, a, name_to_id, type_params, self_ty))
                .collect();
            if alts.len() == 1 {
                alts.into_iter().next().unwrap()
            } else {
                alts.into_iter().next().unwrap_or(Ty::Poison)
            }
        }
        TypeRefKind::Error => Ty::Poison,
    }
}

fn resolve_type_path(
    arena: &VirArena,
    segments: &[String],
    type_arg_refs: &[TypeRefId],
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    type_params: &[String],
    self_ty: Option<&Ty>,
) -> Ty {
    // Use first segment only — old collector ignores subsequent segments.
    let name = match segments.first() {
        Some(n) => n,
        None => return Ty::Poison,
    };

    // Check Self.
    if name == "Self" {
        return self_ty.cloned().unwrap_or(Ty::Poison);
    }

    // Check type params by position index.
    if let Some(index) = type_params.iter().position(|p| p == name) {
        return Ty::Param(vx_types::TypeParam {
            name: name.clone(),
            index: index as u32,
        });
    }

    // Resolve type args.
    let type_args: Vec<Ty> = type_arg_refs
        .iter()
        .map(|&tr| resolve_type_ref(arena, tr, name_to_id, type_params, self_ty))
        .collect();

    resolve_named_type(name, type_args, name_to_id).unwrap_or(Ty::Poison)
}

fn resolve_named_type(
    name: &str,
    type_args: Vec<Ty>,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
) -> Option<Ty> {
    // Builtins first — exact same order as old collector.
    match name {
        "U64" => return Some(Ty::Scalar(Scalar::U64)),
        "I64" => return Some(Ty::Scalar(Scalar::I64)),
        "F64" => return Some(Ty::Scalar(Scalar::F64)),
        "Bool" => return Some(Ty::Scalar(Scalar::Bool)),
        "String" => return Some(Ty::Scalar(Scalar::String)),
        "Unit" => return Some(Ty::Unit),
        "Option" => {
            let inner = type_args.into_iter().next().unwrap_or(Ty::Poison);
            return Some(Ty::Optional(Box::new(inner)));
        }
        "Array" => {
            let inner = type_args.into_iter().next().unwrap_or(Ty::Poison);
            return Some(Ty::Array(Box::new(inner)));
        }
        "Map" => {
            let mut iter = type_args.into_iter();
            let key = iter.next().unwrap_or(Ty::Poison);
            let val = iter.next().unwrap_or(Ty::Poison);
            return Some(Ty::Map(Box::new(key), Box::new(val)));
        }
        "Stream" => {
            let inner = type_args.into_iter().next().unwrap_or(Ty::Poison);
            return Some(Ty::Stream(Box::new(inner)));
        }
        _ => {}
    }

    let &(id, kind) = name_to_id.get(name)?;
    Some(match kind {
        TypeKind::Struct => Ty::Struct(vx_types::StructTy { id, type_args }),
        TypeKind::Enum => Ty::Enum(vx_types::EnumTy { id, type_args }),
        TypeKind::Newtype => Ty::Newtype(vx_types::NewtypeTy { id, type_args }),
        TypeKind::Alias => Ty::Alias(vx_types::AliasTy { id, type_args }),
        TypeKind::Opaque => Ty::Opaque(vx_types::OpaqueTy { id, type_args }),
    })
}

// ---------------------------------------------------------------------------
// Field collection
// ---------------------------------------------------------------------------

fn collect_field_defs(
    arena: &VirArena,
    fields: &[FieldDecl],
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    type_params: &[String],
) -> Vec<FieldDef> {
    fields
        .iter()
        .map(|f| {
            let ty =
                f.ty.map(|tr| resolve_type_ref(arena, tr, name_to_id, type_params, None))
                    .unwrap_or(Ty::Poison);
            let default_value = f
                .default_value
                .and_then(|expr_id| extract_default_value(arena, expr_id));
            FieldDef {
                name: f.name.clone(),
                ty,
                attrs: collect_field_attrs_vir(arena, &f.attributes),
                has_default: f.has_default,
                default_value,
                name_span: f.name_span,
            }
        })
        .collect()
}

fn collect_variant_field_defs(
    arena: &VirArena,
    variant: &VariantDecl,
    name_to_id: &HashMap<String, (TypeId, TypeKind)>,
    type_params: &[String],
) -> Vec<FieldDef> {
    collect_field_defs(arena, &variant.fields, name_to_id, type_params)
}

/// Extract a FieldDefaultValue from a VIR expression.
/// Mirrors the old collector's `field_default_value` / `default_value_from_expr`.
fn extract_default_value(arena: &VirArena, expr_id: crate::ExprId) -> Option<FieldDefaultValue> {
    let expr = &arena[expr_id];
    match &expr.kind {
        ExprKind::Literal(lit) => match &lit.value {
            LitKind::Bool(b) => Some(FieldDefaultValue::Bool(*b)),
            LitKind::Int { raw } => raw.parse::<u64>().ok().map(FieldDefaultValue::U64),
            LitKind::String { raw } => Some(FieldDefaultValue::String(raw.clone())),
            LitKind::Float { .. } => None,
        },
        ExprKind::Array(arr) => {
            if arr.elements.is_empty() {
                Some(FieldDefaultValue::EmptyArray)
            } else {
                None
            }
        }
        ExprKind::Map(map) => {
            if map.entries.is_empty() {
                Some(FieldDefaultValue::EmptyMap)
            } else {
                None
            }
        }
        ExprKind::NameRef(name_ref) => {
            if name_ref.name == "None" {
                Some(FieldDefaultValue::None)
            } else {
                None
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Attribute collection
// ---------------------------------------------------------------------------

fn collect_container_attrs_vir(arena: &VirArena, attributes: &[Attribute]) -> DeserContainerAttrs {
    let mut attrs = DeserContainerAttrs::default();
    for attr in attributes {
        for entry in &attr.entries {
            match entry.key.as_str() {
                "rename" => {
                    attrs.rename = entry_string_value(arena, entry);
                }
                "rename_all" => {
                    attrs.rename_all =
                        entry_string_value(arena, entry).and_then(|value| match value.as_str() {
                            "snake_case" => Some(DeserRenameRule::SnakeCase),
                            "camelCase" => Some(DeserRenameRule::CamelCase),
                            "PascalCase" => Some(DeserRenameRule::PascalCase),
                            "kebab-case" => Some(DeserRenameRule::KebabCase),
                            "SCREAMING_SNAKE_CASE" => Some(DeserRenameRule::ScreamingSnakeCase),
                            _ => None,
                        })
                }
                "untagged" => attrs.untagged = true,
                _ => {}
            }
        }
    }
    attrs
}

fn collect_field_attrs_vir(arena: &VirArena, attributes: &[Attribute]) -> DeserFieldAttrs {
    let mut attrs = DeserFieldAttrs::default();
    for attr in attributes {
        for entry in &attr.entries {
            match entry.key.as_str() {
                "rename" => {
                    attrs.rename = entry_string_value(arena, entry);
                }
                "flatten" => attrs.flatten = true,
                _ => {}
            }
        }
    }
    attrs
}

/// Extract a string value from an attribute entry's value expression.
/// The entry's value is an ExprId pointing to a literal string.
fn entry_string_value(arena: &VirArena, entry: &crate::item::AttributeEntry) -> Option<String> {
    let expr_id = entry.value?;
    let expr = &arena[expr_id];
    if let ExprKind::Literal(lit) = &expr.kind
        && let LitKind::String { raw } = &lit.value
    {
        return Some(raw.clone());
    }
    None
}
