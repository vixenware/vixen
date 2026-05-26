//! Type collection on VIR: register type declarations and build signatures.
//!
//! Reads from VIR items and produces a [`TypeCollection`] with TypeEnv,
//! def-to-type / def-to-fn-sig maps, and sum-alias alternatives.

use std::collections::HashMap;

use vx_parser::SourceId;
use vx_types::{
    AliasDef, DeserContainerAttrs, DeserFieldAttrs, DeserRenameRule, EnumDef, FieldDef,
    FieldDefaultValue, FnSig, KeywordParamDef, NewtypeDef, OpaqueDef, ParamDef, Scalar, StructDef,
    TypeDef, TypeDefId, TypeEnv, TypeId, TypeParam,
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
    let mut name_to_id: HashMap<String, (TypeDefId, TypeKind)> = HashMap::new();

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
    name_to_id: &mut HashMap<String, (TypeDefId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
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
                register_newtype(env, n, source_id, name_to_id, def_to_type);
            }
            ItemKind::TypeAlias(t) => {
                register_type_alias(env, t, source_id, name_to_id, def_to_type);
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
    name_to_id: &mut HashMap<String, (TypeDefId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
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
    name_to_id: &mut HashMap<String, (TypeDefId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
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
    env: &mut TypeEnv,
    n: &NewtypeItem,
    _source_id: SourceId,
    name_to_id: &mut HashMap<String, (TypeDefId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
) {
    let poison = env.intern_poison();
    let id = env.define(
        &n.name,
        TypeDef::Newtype(NewtypeDef {
            name: n.name.clone(),
            type_params: n.type_params.clone(),
            inner: poison, // filled in pass 2
        }),
    );
    name_to_id.insert(n.name.clone(), (id, TypeKind::Newtype));
    def_to_type.insert(n.name_span, id);
}

fn register_type_alias(
    env: &mut TypeEnv,
    t: &TypeAliasItem,
    _source_id: SourceId,
    name_to_id: &mut HashMap<String, (TypeDefId, TypeKind)>,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
) {
    let (def, kind) = if t.aliased_ty.is_some() {
        let poison = env.intern_poison();
        (
            TypeDef::Alias(AliasDef {
                name: t.name.clone(),
                type_params: t.type_params.clone(),
                target: poison, // filled in pass 2
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
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
    def_to_fn_sig: &mut HashMap<DefSite, FnSig>,
    alias_sum_alternatives: &mut HashMap<TypeDefId, Vec<TypeId>>,
    parent_type_params: &[String],
    parent_self_ty: Option<TypeId>,
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
                    env,
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
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
) {
    let (id, _) = name_to_id[&s.name];
    let fields = collect_field_defs(arena, &s.fields, name_to_id, &s.type_params, env);
    if let TypeDef::Struct(ref mut sd) = env[id] {
        sd.fields = fields;
    }
}

fn fill_enum(
    arena: &VirArena,
    e: &EnumItem,
    _source_id: SourceId,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
) {
    let (enum_id, _) = name_to_id[&e.name];
    let mut variant_ids = Vec::new();

    // Collect variants first (need &mut env each iteration).
    let variants_data: Vec<(
        String,
        Vec<FieldDef>,
        DeserContainerAttrs,
        vx_parser::SourceSpan,
    )> = e
        .variants
        .iter()
        .map(|variant| {
            let fields =
                collect_variant_field_defs(arena, variant, name_to_id, &e.type_params, env);
            let attrs = collect_container_attrs_vir(arena, &variant.attributes);
            (variant.name.clone(), fields, attrs, variant.name_span)
        })
        .collect();

    for (name, fields, attrs, name_span) in variants_data {
        let vid = env.define(
            &name,
            TypeDef::Struct(StructDef {
                name: name.clone(),
                parent_enum: Some(enum_id),
                type_params: e.type_params.clone(),
                attrs,
                fields,
            }),
        );
        variant_ids.push(vid);
        def_to_type.insert(name_span, vid);
    }

    if let TypeDef::Enum(ref mut ed) = env[enum_id] {
        ed.variants = variant_ids;
    }
}

fn fill_newtype(
    arena: &VirArena,
    n: &NewtypeItem,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
) {
    let (id, _) = name_to_id[&n.name];
    let inner = match n.inner_ty {
        Some(tr) => resolve_type_ref(arena, tr, name_to_id, &n.type_params, None, env),
        None => env.intern_poison(),
    };
    if let TypeDef::Newtype(ref mut nd) = env[id] {
        nd.inner = inner;
    }
}

fn fill_type_alias(
    arena: &VirArena,
    t: &TypeAliasItem,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
    alias_sum_alternatives: &mut HashMap<TypeDefId, Vec<TypeId>>,
) {
    let Some(aliased_tr) = t.aliased_ty else {
        return;
    };
    let (id, _) = name_to_id[&t.name];
    let target = resolve_type_ref(arena, aliased_tr, name_to_id, &t.type_params, None, env);

    // If the aliased type is a sum, record the alternatives.
    if let TypeRefKind::Sum(ref sum) = arena.type_refs[aliased_tr.0 as usize].kind {
        let alts_ids: Vec<TypeRefId> = sum.alternatives.clone();
        let alternatives: Vec<TypeId> = alts_ids
            .into_iter()
            .map(|alt| resolve_type_ref(arena, alt, name_to_id, &t.type_params, None, env))
            .collect();
        alias_sum_alternatives.insert(id, alternatives);
    }

    if let TypeDef::Alias(ref mut ad) = env[id] {
        ad.target = target;
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_function(
    arena: &VirArena,
    f: &FunctionItem,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
    def_to_fn_sig: &mut HashMap<DefSite, FnSig>,
    parent_type_params: &[String],
    self_ty: Option<TypeId>,
) {
    // Merge parent type params (from namespace) with function-level type params.
    let mut type_params: Vec<String> = parent_type_params.to_vec();
    type_params.extend(f.type_params.iter().cloned());

    let sig = collect_fn_sig_from_body(arena, f, name_to_id, &type_params, self_ty, env);
    def_to_fn_sig.insert(f.name_span, sig);
}

#[allow(clippy::too_many_arguments)]
fn fill_namespace(
    arena: &VirArena,
    ns: &NamespaceItem,
    source_id: SourceId,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
    def_to_type: &mut HashMap<DefSite, TypeDefId>,
    def_to_fn_sig: &mut HashMap<DefSite, FnSig>,
    alias_sum_alternatives: &mut HashMap<TypeDefId, Vec<TypeId>>,
) {
    let namespace_type_params = ns.type_params.clone();

    // Compute namespace self_ty: resolve the namespace name as a type with type params.
    let namespace_self_ty =
        compute_namespace_self_ty(&ns.name, &namespace_type_params, name_to_id, env);

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
        &namespace_type_params,
        namespace_self_ty,
    );
}

// ---------------------------------------------------------------------------
// FnSig collection
// ---------------------------------------------------------------------------

fn collect_fn_sig_from_body(
    arena: &VirArena,
    func: &FunctionItem,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    type_params: &[String],
    self_ty: Option<TypeId>,
    env: &mut TypeEnv,
) -> FnSig {
    let body = &arena[func.body];

    let mut positional_params: Vec<ParamDef> = Vec::new();
    let mut keyword_params: Vec<KeywordParamDef> = Vec::new();

    // Cache the params we need to walk; avoid holding &arena[body] across &mut env calls.
    struct ParamData {
        name: String,
        name_span: vx_parser::SourceSpan,
        ty_ref: Option<TypeRefId>,
        is_keyword: bool,
        has_default: bool,
        doc: Option<String>,
    }
    let params_data: Vec<ParamData> = body
        .params
        .iter()
        .map(|param| ParamData {
            name: param_name(arena, param),
            name_span: param_name_span(arena, param),
            ty_ref: param.ty,
            is_keyword: param.is_keyword,
            has_default: param.has_default,
            doc: param.doc.clone(),
        })
        .collect();

    for p in params_data {
        let ty = if let Some(tr) = p.ty_ref {
            resolve_type_ref(arena, tr, name_to_id, type_params, self_ty, env)
        } else if p.name == "self" {
            self_ty.unwrap_or_else(|| env.intern_poison())
        } else {
            env.intern_poison()
        };

        if p.is_keyword {
            keyword_params.push(KeywordParamDef {
                name: p.name,
                name_span: p.name_span,
                ty,
                has_default: p.has_default,
                doc: p.doc,
            });
        } else {
            positional_params.push(ParamDef {
                name: p.name,
                name_span: p.name_span,
                ty,
                doc: p.doc,
            });
        }
    }

    let ret = match body.return_ty {
        Some(tr) => resolve_type_ref(arena, tr, name_to_id, type_params, self_ty, env),
        None => env.intern_unit(),
    };

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

// ---------------------------------------------------------------------------
// Namespace self_ty
// ---------------------------------------------------------------------------

fn compute_namespace_self_ty(
    name: &str,
    type_params: &[String],
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
) -> Option<TypeId> {
    let type_args: Vec<TypeId> = type_params
        .iter()
        .enumerate()
        .map(|(index, name)| {
            env.intern_param(TypeParam {
                name: name.clone(),
                index: index as u32,
            })
        })
        .collect();
    resolve_named_type(name, type_args, name_to_id, env)
}

// ---------------------------------------------------------------------------
// Type ref resolution — exact clone of TypeResolver::resolve_type()
// ---------------------------------------------------------------------------

/// Resolve a VIR TypeRef to a [`TypeId`], interning constructed types into `env`.
///
/// This intentionally mirrors the old collector's `TypeResolver::resolve_type_in_context`
/// exactly, including limitations like ignoring multi-segment paths.
pub fn resolve_type_ref(
    arena: &VirArena,
    type_ref_id: TypeRefId,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    type_params: &[String],
    self_ty: Option<TypeId>,
    env: &mut TypeEnv,
) -> TypeId {
    let tr = &arena.type_refs[type_ref_id.0 as usize];
    match &tr.kind {
        TypeRefKind::Path(path) => {
            let segments = path.segments.clone();
            let type_args = path.type_args.clone();
            resolve_type_path(
                arena,
                &segments,
                &type_args,
                name_to_id,
                type_params,
                self_ty,
                env,
            )
        }
        TypeRefKind::Optional(opt) => {
            let inner_ref = opt.inner;
            let inner = resolve_type_ref(arena, inner_ref, name_to_id, type_params, self_ty, env);
            env.intern_optional(inner)
        }
        TypeRefKind::Tuple(tup) => {
            let elem_refs = tup.elements.clone();
            let elements: Vec<TypeId> = elem_refs
                .into_iter()
                .map(|e| resolve_type_ref(arena, e, name_to_id, type_params, self_ty, env))
                .collect();
            env.intern_tuple(elements)
        }
        TypeRefKind::Function(func) => {
            let param_refs = func.params.clone();
            let ret_ref = func.ret;
            let capabilities: Vec<vx_types::Capability> = func
                .capabilities
                .iter()
                .map(|tag| vx_types::Capability {
                    name: tag.name.clone(),
                    name_span: tag.name_span,
                })
                .collect();
            let params: Vec<TypeId> = param_refs
                .into_iter()
                .map(|p| resolve_type_ref(arena, p, name_to_id, type_params, self_ty, env))
                .collect();
            let ret = resolve_type_ref(arena, ret_ref, name_to_id, type_params, self_ty, env);
            env.intern_function(params, ret, capabilities)
        }
        TypeRefKind::Sum(sum) => {
            // Sum types: resolve each variant. Same behavior as old collector —
            // returns first alternative if multiple (sum types are stored as
            // AliasDef targets, not as a Ty variant).
            let alt_refs = sum.alternatives.clone();
            let alts: Vec<TypeId> = alt_refs
                .into_iter()
                .map(|a| resolve_type_ref(arena, a, name_to_id, type_params, self_ty, env))
                .collect();
            if alts.is_empty() {
                env.intern_poison()
            } else {
                alts[0]
            }
        }
        TypeRefKind::Error => env.intern_poison(),
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_type_path(
    arena: &VirArena,
    segments: &[String],
    type_arg_refs: &[TypeRefId],
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    type_params: &[String],
    self_ty: Option<TypeId>,
    env: &mut TypeEnv,
) -> TypeId {
    // Use first segment only — old collector ignores subsequent segments.
    let name = match segments.first() {
        Some(n) => n.clone(),
        None => return env.intern_poison(),
    };

    // Check Self.
    if name == "Self" {
        return self_ty.unwrap_or_else(|| env.intern_poison());
    }

    // Check type params by position index.
    if let Some(index) = type_params.iter().position(|p| *p == name) {
        return env.intern_param(TypeParam {
            name,
            index: index as u32,
        });
    }

    // Resolve type args.
    let type_args: Vec<TypeId> = type_arg_refs
        .iter()
        .map(|&tr| resolve_type_ref(arena, tr, name_to_id, type_params, self_ty, env))
        .collect();

    resolve_named_type(&name, type_args, name_to_id, env).unwrap_or_else(|| env.intern_poison())
}

fn resolve_named_type(
    name: &str,
    type_args: Vec<TypeId>,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    env: &mut TypeEnv,
) -> Option<TypeId> {
    // Builtins first — exact same order as old collector.
    match name {
        "U64" => return Some(env.intern_scalar(Scalar::U64)),
        "I64" => return Some(env.intern_scalar(Scalar::I64)),
        "F64" => return Some(env.intern_scalar(Scalar::F64)),
        "Bool" => return Some(env.intern_scalar(Scalar::Bool)),
        "String" => return Some(env.intern_scalar(Scalar::String)),
        "Unit" => return Some(env.intern_unit()),
        "Option" => {
            let inner = type_args
                .into_iter()
                .next()
                .unwrap_or_else(|| env.intern_poison());
            return Some(env.intern_optional(inner));
        }
        "Array" => {
            let inner = type_args
                .into_iter()
                .next()
                .unwrap_or_else(|| env.intern_poison());
            return Some(env.intern_array(inner));
        }
        "Map" => {
            let mut iter = type_args.into_iter();
            let key = iter.next().unwrap_or_else(|| env.intern_poison());
            let val = iter.next().unwrap_or_else(|| env.intern_poison());
            return Some(env.intern_map(key, val));
        }
        "Stream" => {
            let inner = type_args
                .into_iter()
                .next()
                .unwrap_or_else(|| env.intern_poison());
            return Some(env.intern_stream(inner));
        }
        _ => {}
    }

    let &(id, kind) = name_to_id.get(name)?;
    Some(match kind {
        TypeKind::Struct => env.intern_struct(id, type_args),
        TypeKind::Enum => env.intern_enum(id, type_args),
        TypeKind::Newtype => env.intern_newtype(id, type_args),
        TypeKind::Alias => env.intern_alias(id, type_args),
        TypeKind::Opaque => env.intern_opaque(id, type_args),
    })
}

// ---------------------------------------------------------------------------
// Field collection
// ---------------------------------------------------------------------------

fn collect_field_defs(
    arena: &VirArena,
    fields: &[FieldDecl],
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    type_params: &[String],
    env: &mut TypeEnv,
) -> Vec<FieldDef> {
    // Walk in an explicit loop because we need &mut env per field.
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        let ty = match f.ty {
            Some(tr) => resolve_type_ref(arena, tr, name_to_id, type_params, None, env),
            None => env.intern_poison(),
        };
        let default_value = f
            .default_value
            .and_then(|expr_id| extract_default_value(arena, expr_id));
        out.push(FieldDef {
            name: f.name.clone(),
            ty,
            attrs: collect_field_attrs_vir(arena, &f.attributes),
            has_default: f.has_default,
            default_value,
            name_span: f.name_span,
        });
    }
    out
}

fn collect_variant_field_defs(
    arena: &VirArena,
    variant: &VariantDecl,
    name_to_id: &HashMap<String, (TypeDefId, TypeKind)>,
    type_params: &[String],
    env: &mut TypeEnv,
) -> Vec<FieldDef> {
    collect_field_defs(arena, &variant.fields, name_to_id, type_params, env)
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
