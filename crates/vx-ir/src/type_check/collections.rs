//! Array, tuple, record, map, and struct literal inference.

use vx_types::{Scalar, TypeId};

use crate::expr::{
    ArrayElement, ArrayExpr, MapElement, MapExpr, RecordExpr, StructLiteralExpr, TupleExpr,
};

use super::TypeChecker;

pub(crate) fn infer_array(tc: &mut TypeChecker, arr: &ArrayExpr) -> TypeId {
    let mut element_ty = None;
    for elem in &arr.elements {
        let id = match elem {
            ArrayElement::Expr(id) | ArrayElement::Spread(id) => *id,
        };
        let ty = tc.infer_expr(id);
        if !tc.types.env.get_ty(ty).is_poison() && element_ty.is_none() {
            element_ty = Some(ty);
        }
    }
    let inner = element_ty.unwrap_or_else(|| tc.poison());
    tc.types.env.intern_array(inner)
}

pub(crate) fn infer_tuple(tc: &mut TypeChecker, t: &TupleExpr) -> TypeId {
    let elements: Vec<TypeId> = t.elements.iter().map(|&id| tc.infer_expr(id)).collect();
    tc.types.env.intern_tuple(elements)
}

pub(crate) fn infer_record(tc: &mut TypeChecker, r: &RecordExpr) -> TypeId {
    for field in &r.fields {
        // Skip closures in record fields (they get typed via callback param binding).
        let kind = &tc.arena[field.value].kind.clone();
        if matches!(kind, crate::expr::ExprKind::Closure(_)) {
            continue;
        }
        tc.infer_expr(field.value);
    }
    tc.poison()
}

pub(crate) fn infer_map(tc: &mut TypeChecker, m: &MapExpr) -> TypeId {
    let mut key_ty: Option<TypeId> = None;
    let mut val_ty: Option<TypeId> = None;
    for elem in &m.entries {
        match elem {
            MapElement::Entry(entry) => {
                match &entry.key {
                    crate::expr::MapKey::Expr(k) => {
                        let ty = tc.infer_expr(*k);
                        if !tc.types.env.get_ty(ty).is_poison() && key_ty.is_none() {
                            key_ty = Some(ty);
                        }
                    }
                    crate::expr::MapKey::Name { .. } => {
                        if key_ty.is_none() {
                            key_ty = Some(tc.types.env.intern_scalar(Scalar::String));
                        }
                    }
                }
                let ty = tc.infer_expr(entry.value);
                if !tc.types.env.get_ty(ty).is_poison() && val_ty.is_none() {
                    val_ty = Some(ty);
                }
            }
            MapElement::Spread(id) => {
                tc.infer_expr(*id);
            }
        }
    }
    let poison = tc.poison();
    let k = key_ty.unwrap_or(poison);
    let v = val_ty.unwrap_or(poison);
    tc.types.env.intern_map(k, v)
}

pub(crate) fn infer_struct_literal(tc: &mut TypeChecker, sl: &StructLiteralExpr) -> TypeId {
    // Infer field value expressions.
    for field in &sl.fields {
        tc.infer_expr(field.value);
    }

    // Enum-variant construction `Enum.Variant { … }`: the value has the enum
    // type. `resolved` is the enum item, `resolved_variant` the chosen variant.
    if let Some(item_id) = sl.resolved
        && sl.resolved_variant.is_some()
        && let crate::item::ItemKind::Enum(ref e) = tc.arena[item_id].kind
    {
        let enum_name_span = e.name_span;
        if let Some(&def_id) = tc.types.def_to_type.get(&enum_name_span)
            && matches!(tc.types.env[def_id], vx_types::TypeDef::Enum(_))
        {
            return tc.types.env.intern_enum(def_id, vec![]);
        }
    }

    // Use the resolved struct item to find the TypeDefId.
    if let Some(item_id) = sl.resolved
        && let crate::item::ItemKind::Struct(ref s) = tc.arena[item_id].kind
    {
        let name_span = s.name_span;
        if let Some(&def_id) = tc.types.def_to_type.get(&name_span)
            && matches!(
                tc.types.env[def_id],
                vx_types::TypeDef::Struct(ref sdef) if sdef.parent_enum.is_none()
            )
        {
            return tc.types.env.intern_struct(def_id, vec![]);
        }
    }

    // Fallback: look up by name.
    let type_ids: Vec<vx_types::TypeDefId> = tc.types.env.lookup_def(&sl.name).to_vec();
    for id in type_ids {
        if let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[id]
            && sdef.parent_enum.is_none()
        {
            return tc.types.env.intern_struct(id, vec![]);
        }
    }
    tc.poison()
}
