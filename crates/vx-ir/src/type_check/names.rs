//! Name reference inference and definition type lookup.

use vx_types::{FnSig, InferVar, ParamDef, Ty};

use crate::binding::BindingKind;
use crate::expr::NameRefExpr;
use crate::item::ItemKind;
use crate::scope::DefTarget;
use crate::{BindingId, ExprId, ItemId};

use super::TypeChecker;

pub(crate) fn infer_name_ref(tc: &mut TypeChecker, _expr_id: ExprId, nr: &NameRefExpr) -> Ty {
    let Some(target) = nr.resolved else {
        return Ty::Poison;
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
pub(crate) fn type_of_binding(tc: &mut TypeChecker, binding_id: BindingId) -> Ty {
    let binding = &tc.arena[binding_id];

    // If already typed (e.g. by let-stmt processing), return it.
    if let Some(ref ty) = binding.ty {
        return ty.clone();
    }

    match binding.kind {
        BindingKind::Parameter => {
            // Look up type from the enclosing function's FnSig and persist it.
            if let Some(ty) = param_type_from_fn_sig(tc, binding_id) {
                tc.arena[binding_id].ty = Some(ty.clone());
                return ty;
            }
            Ty::Poison
        }
        BindingKind::Let
        | BindingKind::PatternMatch
        | BindingKind::AtPattern
        | BindingKind::ArrayRest => {
            // Should have been populated by type_pattern or let-stmt handling.
            Ty::Poison
        }
    }
}

/// Look up a parameter's type from its enclosing function's FnSig.
fn param_type_from_fn_sig(tc: &TypeChecker, binding_id: BindingId) -> Option<Ty> {
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
        if param.name == binding.name && !param.ty.is_poison() {
            return Some(param.ty.clone());
        }
    }

    // Match by name in keyword params.
    for param in &sig.keyword_params {
        if param.name == binding.name && !param.ty.is_poison() {
            return Some(param.ty.clone());
        }
    }

    None
}

/// Get the type of an item reference (function, struct constructor, etc.).
pub(crate) fn type_of_item(tc: &TypeChecker, item_id: ItemId) -> Ty {
    let item = &tc.arena[item_id];
    let kind = &item.kind;

    match kind {
        ItemKind::Function(f) => {
            let def_site = f.name_span;
            if let Some(sig) = tc.types.def_to_fn_sig.get(&def_site) {
                return Ty::Function(vx_types::FunctionTy {
                    params: sig.positional_params.iter().map(|p| p.ty.clone()).collect(),
                    ret: Box::new(sig.ret.clone()),
                    capabilities: sig.capabilities.clone(),
                });
            }
            Ty::Poison
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
            Ty::Poison
        }
        _ => Ty::Poison,
    }
}

/// Get the type of a variant reference.
pub(crate) fn type_of_variant(tc: &TypeChecker, enum_item: ItemId, variant_index: u32) -> Ty {
    let ItemKind::Enum(ref enum_item_data) = tc.arena[enum_item].kind else {
        return Ty::Poison;
    };

    let Some(_variant) = enum_item_data.variants.get(variant_index as usize) else {
        return Ty::Poison;
    };

    // Look up the variant's TypeId from the type collection.
    let enum_name_span = enum_item_data.name_span;
    let Some(&enum_type_id) = tc.types.def_to_type.get(&enum_name_span) else {
        return Ty::Poison;
    };

    let vx_types::TypeDef::Enum(ref enum_def) = tc.types.env[enum_type_id] else {
        return Ty::Poison;
    };

    let Some(&variant_type_id) = enum_def.variants.get(variant_index as usize) else {
        return Ty::Poison;
    };

    let vx_types::TypeDef::Struct(ref variant_struct) = tc.types.env[variant_type_id] else {
        return Ty::Poison;
    };

    // Unit variant (no fields) → value type.
    if variant_struct.fields.is_empty() {
        if enum_def.name == "Option" {
            return Ty::Optional(Box::new(Ty::Infer(InferVar(0))));
        }
        return Ty::Enum(vx_types::EnumTy {
            id: enum_type_id,
            type_args: vec![],
        });
    }

    // Variant with fields → constructor function type.
    let ret = if enum_def.name == "Option" {
        let inner = variant_struct
            .fields
            .iter()
            .find(|f| f.name == "value")
            .map(|f| f.ty.clone())
            .unwrap_or(Ty::Infer(InferVar(0)));
        Ty::Optional(Box::new(inner))
    } else {
        Ty::Enum(vx_types::EnumTy {
            id: enum_type_id,
            type_args: vec![],
        })
    };

    Ty::Function(vx_types::FunctionTy {
        params: variant_struct.fields.iter().map(|f| f.ty.clone()).collect(),
        ret: Box::new(ret),
        capabilities: vec![],
    })
}

/// Get the FnSig for a name ref, considering function/variant/namespace overloads.
pub(crate) fn sig_for_name_ref_target(tc: &TypeChecker, target: DefTarget) -> Option<FnSig> {
    match target {
        DefTarget::Item(item_id) => {
            let kind = &tc.arena[item_id].kind;
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
    tc: &TypeChecker,
    enum_item: ItemId,
    variant_index: u32,
) -> Option<FnSig> {
    let ItemKind::Enum(ref enum_item_data) = tc.arena[enum_item].kind else {
        return None;
    };

    let variant = enum_item_data.variants.get(variant_index as usize)?;
    let enum_name_span = enum_item_data.name_span;
    let &enum_type_id = tc.types.def_to_type.get(&enum_name_span)?;
    let vx_types::TypeDef::Enum(ref enum_def) = tc.types.env[enum_type_id] else {
        return None;
    };
    let &variant_type_id = enum_def.variants.get(variant_index as usize)?;
    let vx_types::TypeDef::Struct(ref variant_struct) = tc.types.env[variant_type_id] else {
        return None;
    };

    let ret = if enum_def.name == "Option" {
        let inner = variant_struct
            .fields
            .iter()
            .find(|f| f.name == "value")
            .map(|f| f.ty.clone())
            .unwrap_or(Ty::Infer(InferVar(0)));
        Ty::Optional(Box::new(inner))
    } else {
        Ty::Enum(vx_types::EnumTy {
            id: enum_type_id,
            type_args: vec![],
        })
    };

    Some(FnSig {
        name: variant.name.clone(),
        name_span: variant.name_span,
        type_params: enum_def.type_params.clone(),
        positional_params: variant_struct
            .fields
            .iter()
            .map(|f| ParamDef {
                name: f.name.clone(),
                name_span: f.name_span,
                ty: f.ty.clone(),
                doc: None,
            })
            .collect(),
        keyword_params: vec![],
        ret,
        capabilities: vec![],
    })
}
