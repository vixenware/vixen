//! Name reference inference and definition type lookup.

use vx_types::{FnSig, ParamDef, TypeId};

use crate::binding::BindingKind;
use crate::expr::NameRefExpr;
use crate::item::ItemKind;
use crate::scope::DefTarget;
use crate::{BindingId, ExprId, ItemId};

use super::TypeChecker;

pub(crate) fn infer_name_ref(tc: &mut TypeChecker, _expr_id: ExprId, nr: &NameRefExpr) -> TypeId {
    let Some(target) = nr.resolved else {
        return tc.poison();
    };

    match target {
        DefTarget::Binding(binding_id) => type_of_binding(tc, binding_id),
        DefTarget::Item(item_id) => type_of_item(tc, item_id),
        DefTarget::Variant {
            enum_item,
            variant_index,
        } => type_of_variant(tc, enum_item, variant_index),
    }
}

/// Get the type of a binding.
pub(crate) fn type_of_binding(tc: &mut TypeChecker, binding_id: BindingId) -> TypeId {
    let binding = &tc.arena[binding_id];

    // If already typed (e.g. by let-stmt processing), return it.
    if let Some(ty) = binding.ty {
        return ty;
    }

    match binding.kind {
        BindingKind::Parameter => {
            // Look up type from the enclosing function's FnSig and persist it.
            if let Some(ty) = param_type_from_fn_sig(tc, binding_id) {
                tc.arena[binding_id].ty = Some(ty);
                return ty;
            }
            tc.poison()
        }
        BindingKind::Let
        | BindingKind::PatternMatch
        | BindingKind::AtPattern
        | BindingKind::ArrayRest => {
            // Should have been populated by type_pattern or let-stmt handling.
            tc.poison()
        }
    }
}

/// Look up a parameter's type from its enclosing function's FnSig.
fn param_type_from_fn_sig(tc: &TypeChecker, binding_id: BindingId) -> Option<TypeId> {
    let binding = &tc.arena[binding_id];
    let body_id = binding.body;
    let body = &tc.arena[body_id];

    // Find the FnSig for this body's owning function.
    let item_id = match body.owner {
        crate::body::BodyOwner::Function { item } => item,
        _ => return None,
    };

    let name_span = match &tc.arena[item_id].kind {
        ItemKind::Function(f) => f.name_span,
        _ => return None,
    };

    let def_site = name_span;
    let sig = tc.types.def_to_fn_sig.get(&def_site)?;

    // Match by name in positional params.
    for param in &sig.positional_params {
        if param.name == binding.name && !tc.types.env.get_ty(param.ty).is_poison() {
            return Some(param.ty);
        }
    }

    // Match by name in keyword params.
    for param in &sig.keyword_params {
        if param.name == binding.name && !tc.types.env.get_ty(param.ty).is_poison() {
            return Some(param.ty);
        }
    }

    None
}

/// Get the type of an item reference (function, struct constructor, etc.).
pub(crate) fn type_of_item(tc: &mut TypeChecker, item_id: ItemId) -> TypeId {
    let kind = tc.arena[item_id].kind.clone();

    match kind {
        ItemKind::Function(f) => {
            let def_site = f.name_span;
            if let Some(sig) = tc.types.def_to_fn_sig.get(&def_site).cloned() {
                let params: Vec<TypeId> = sig.positional_params.iter().map(|p| p.ty).collect();
                let ret = sig.ret;
                let caps = sig.capabilities;
                return tc.types.env.intern_function(params, ret, caps);
            }
            tc.poison()
        }
        ItemKind::Namespace(ns) => {
            // If namespace has exactly one constructor function, return its type.
            let mut sigs = Vec::new();
            for &child_id in &ns.items {
                if let ItemKind::Function(f) = &tc.arena[child_id].kind
                    && let Some(sig) = tc.types.def_to_fn_sig.get(&f.name_span)
                {
                    sigs.push(sig.clone());
                }
            }
            if sigs.len() == 1 {
                return tc.function_ty_from_sig(&sigs[0]);
            }
            tc.poison()
        }
        _ => tc.poison(),
    }
}

/// Get the type of a variant reference.
pub(crate) fn type_of_variant(
    tc: &mut TypeChecker,
    enum_item: ItemId,
    variant_index: u32,
) -> TypeId {
    let ItemKind::Enum(ref enum_item_data) = tc.arena[enum_item].kind else {
        return tc.poison();
    };

    let Some(_variant) = enum_item_data.variants.get(variant_index as usize) else {
        return tc.poison();
    };

    // Look up the variant's TypeDefId from the type collection.
    let enum_name_span = enum_item_data.name_span;
    let Some(&enum_def_id) = tc.types.def_to_type.get(&enum_name_span) else {
        return tc.poison();
    };

    let variant_def_id = {
        let vx_types::TypeDef::Enum(ref enum_def) = tc.types.env[enum_def_id] else {
            return tc.poison();
        };
        let Some(&v) = enum_def.variants.get(variant_index as usize) else {
            return tc.poison();
        };
        v
    };

    // Snapshot variant struct fields.
    let fields_snapshot: Vec<(String, TypeId)> = {
        let vx_types::TypeDef::Struct(ref variant_struct) = tc.types.env[variant_def_id] else {
            return tc.poison();
        };
        variant_struct
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect()
    };

    // The variant's enum type, generic over the enum's own type params (e.g.
    // `Option[T]`, `Result[T, E]`). A leftover `Ty::Param` is wildcard-compatible
    // (unifies with anything), so a unit variant used as a value (`None`) just
    // works; a `Variant(args)` *call* resolves the params via `instantiate_call_sig`.
    let ret = enum_self_type(tc, enum_def_id);

    // Unit variant (no fields) → the enum value itself.
    if fields_snapshot.is_empty() {
        return ret;
    }
    // Variant with fields → its constructor function type.
    let params: Vec<TypeId> = fields_snapshot.iter().map(|(_, t)| *t).collect();
    tc.types.env.intern_function(params, ret, vec![])
}

/// The enum's own generic type — `intern_enum(def, [Param(0), Param(1), …])` — used
/// as a variant value/constructor's result. The params stay abstract (wildcards)
/// until a call site or a usage unifies them.
fn enum_self_type(tc: &mut TypeChecker, enum_def_id: vx_types::TypeDefId) -> TypeId {
    let type_params: Vec<String> = match &tc.types.env[enum_def_id] {
        vx_types::TypeDef::Enum(e) => e.type_params.clone(),
        _ => Vec::new(),
    };
    let args: Vec<TypeId> = type_params
        .iter()
        .enumerate()
        .map(|(i, name)| {
            tc.types.env.intern_param(vx_types::TypeParam {
                name: name.clone(),
                index: i as u32,
            })
        })
        .collect();
    tc.types.env.intern_enum(enum_def_id, args)
}

/// Get the FnSig for a name ref, considering function/variant/namespace overloads.
pub(crate) fn sig_for_name_ref_target(tc: &mut TypeChecker, target: DefTarget) -> Option<FnSig> {
    match target {
        DefTarget::Item(item_id) => {
            let kind = tc.arena[item_id].kind.clone();
            match kind {
                ItemKind::Function(f) => tc.types.def_to_fn_sig.get(&f.name_span).cloned(),
                ItemKind::Namespace(ns) => {
                    let mut sigs = Vec::new();
                    for &child_id in &ns.items {
                        if let ItemKind::Function(f) = &tc.arena[child_id].kind
                            && let Some(sig) = tc.types.def_to_fn_sig.get(&f.name_span)
                        {
                            sigs.push(sig.clone());
                        }
                    }
                    (sigs.len() == 1).then(|| sigs.into_iter().next().unwrap())
                }
                _ => None,
            }
        }
        DefTarget::Variant {
            enum_item,
            variant_index,
        } => variant_sig(tc, enum_item, variant_index),
        DefTarget::Binding(_) => None,
    }
}

/// Build a FnSig for a variant constructor.
pub(crate) fn variant_sig(
    tc: &mut TypeChecker,
    enum_item: ItemId,
    variant_index: u32,
) -> Option<FnSig> {
    let ItemKind::Enum(ref enum_item_data) = tc.arena[enum_item].kind else {
        return None;
    };

    let variant = enum_item_data.variants.get(variant_index as usize)?.clone();
    let enum_name_span = enum_item_data.name_span;
    let &enum_def_id = tc.types.def_to_type.get(&enum_name_span)?;
    let (variant_def_id, type_params) = {
        let vx_types::TypeDef::Enum(ref enum_def) = tc.types.env[enum_def_id] else {
            return None;
        };
        let &v = enum_def.variants.get(variant_index as usize)?;
        (v, enum_def.type_params.clone())
    };
    let fields_snapshot: Vec<vx_types::FieldDef> = {
        let vx_types::TypeDef::Struct(ref variant_struct) = tc.types.env[variant_def_id] else {
            return None;
        };
        variant_struct.fields.clone()
    };

    // The constructor returns the enum generic over its own params; the call site
    // resolves them from the args (`instantiate_call_sig`).
    let ret = enum_self_type(tc, enum_def_id);

    Some(FnSig {
        name: variant.name.clone(),
        name_span: variant.name_span,
        type_params,
        positional_params: fields_snapshot
            .into_iter()
            .map(|f| ParamDef {
                name: f.name,
                name_span: f.name_span,
                ty: f.ty,
                doc: None,
            })
            .collect(),
        keyword_params: vec![],
        ret,
        capabilities: vec![],
    })
}
