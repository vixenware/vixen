//! Array, tuple, record, map, and struct literal inference.

use vx_types::{Scalar, TupleTy, Ty};

use crate::expr::{
    ArrayElement, ArrayExpr, MapElement, MapExpr, RecordExpr, StructLiteralExpr, TupleExpr,
};

use super::TypeChecker;

pub(crate) fn infer_array(tc: &mut TypeChecker, arr: &ArrayExpr) -> Ty {
    let mut element_ty = None;
    for elem in &arr.elements {
        let id = match elem {
            ArrayElement::Expr(id) | ArrayElement::Spread(id) => *id,
        };
        let ty = tc.infer_expr(id);
        if !ty.is_poison() && element_ty.is_none() {
            element_ty = Some(ty);
        }
    }
    Ty::Array(Box::new(element_ty.unwrap_or(Ty::Poison)))
}

pub(crate) fn infer_tuple(tc: &mut TypeChecker, t: &TupleExpr) -> Ty {
    let elements: Vec<Ty> = t.elements.iter().map(|&id| tc.infer_expr(id)).collect();
    Ty::Tuple(TupleTy { elements })
}

pub(crate) fn infer_record(tc: &mut TypeChecker, r: &RecordExpr) -> Ty {
    for field in &r.fields {
        // Skip closures in record fields (they get typed via callback param binding).
        let kind = &tc.arena[field.value].kind.clone();
        if matches!(kind, crate::expr::ExprKind::Closure(_)) {
            continue;
        }
        tc.infer_expr(field.value);
    }
    Ty::Poison
}

pub(crate) fn infer_map(tc: &mut TypeChecker, m: &MapExpr) -> Ty {
    let mut key_ty = None;
    let mut val_ty = None;
    for elem in &m.entries {
        match elem {
            MapElement::Entry(entry) => {
                match &entry.key {
                    crate::expr::MapKey::Expr(k) => {
                        let ty = tc.infer_expr(*k);
                        if !ty.is_poison() && key_ty.is_none() {
                            key_ty = Some(ty);
                        }
                    }
                    crate::expr::MapKey::Name { .. } => {
                        if key_ty.is_none() {
                            key_ty = Some(Ty::Scalar(Scalar::String));
                        }
                    }
                }
                let ty = tc.infer_expr(entry.value);
                if !ty.is_poison() && val_ty.is_none() {
                    val_ty = Some(ty);
                }
            }
            MapElement::Spread(id) => {
                tc.infer_expr(*id);
            }
        }
    }
    Ty::Map(
        Box::new(key_ty.unwrap_or(Ty::Poison)),
        Box::new(val_ty.unwrap_or(Ty::Poison)),
    )
}

pub(crate) fn infer_struct_literal(tc: &mut TypeChecker, sl: &StructLiteralExpr) -> Ty {
    // Infer field value expressions.
    for field in &sl.fields {
        tc.infer_expr(field.value);
    }

    // Use the resolved struct item to find the TypeId.
    if let Some(item_id) = sl.resolved {
        if let crate::item::ItemKind::Struct(ref s) = tc.arena[item_id].kind {
            if let Some(&type_id) = tc.types.def_to_type.get(&s.name_span) {
                if matches!(tc.types.env[type_id], vx_types::TypeDef::Struct(ref sdef) if sdef.parent_enum.is_none())
                {
                    return Ty::Struct(vx_types::StructTy {
                        id: type_id,
                        type_args: vec![],
                    });
                }
            }
        }
    }

    // Fallback: look up by name.
    let type_ids = tc.types.env.lookup(&sl.name);
    for &id in type_ids {
        if let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[id] {
            if sdef.parent_enum.is_none() {
                return Ty::Struct(vx_types::StructTy {
                    id,
                    type_args: vec![],
                });
            }
        }
    }
    Ty::Poison
}
