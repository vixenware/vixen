//! With, fallible, fail, type_op, range, and import inference.

use vx_types::{Ty, TypeId};

use crate::ExprId;
use crate::expr::{
    ExprKind, FailExpr, FallibleExpr, TypeOpExpr, TypeOpKind, UpdateField, WithExpr,
};

use super::TypeChecker;

pub(crate) fn infer_with(tc: &mut TypeChecker, w: &WithExpr) -> TypeId {
    let receiver_ty = tc.infer_expr(w.receiver);
    infer_update_field_values(tc, &w.updates);
    receiver_ty
}

fn infer_update_field_values(tc: &mut TypeChecker, updates: &[UpdateField]) {
    for field in updates {
        match field {
            UpdateField::Set { value, .. } => {
                tc.infer_expr(*value);
            }
            UpdateField::Nested { updates, .. } => {
                infer_update_field_values(tc, updates);
            }
        }
    }
}

pub(crate) fn infer_fallible(tc: &mut TypeChecker, f: &FallibleExpr) -> TypeId {
    let inner_ty = tc.infer_expr(f.expr);
    if tc.types.env.get_ty(inner_ty).is_poison() {
        return tc.poison();
    }
    let inner_clone = tc.types.env.get_ty(inner_ty).clone();
    match inner_clone {
        Ty::Function(fn_ty) => {
            let opt_ret = tc.types.env.intern_optional(fn_ty.ret);
            tc.types
                .env
                .intern_function(fn_ty.params, opt_ret, fn_ty.capabilities)
        }
        _ => tc.types.env.intern_optional(inner_ty),
    }
}

pub(crate) fn infer_fail(tc: &mut TypeChecker, f: &FailExpr) -> TypeId {
    if let Some(msg) = f.message {
        tc.infer_expr(msg);
    }
    tc.poison()
}

pub(crate) fn infer_range(tc: &mut TypeChecker, expr_id: ExprId) -> TypeId {
    let kind = tc.arena[expr_id].kind.clone();
    if let ExprKind::Range(ref range) = kind {
        if let Some(s) = range.start {
            tc.infer_expr(s);
        }
        if let Some(e) = range.end {
            tc.infer_expr(e);
        }
    }
    // Range type is not tracked in the old checker — returns Poison.
    tc.poison()
}

pub(crate) fn infer_type_op(tc: &mut TypeChecker, top: &TypeOpExpr) -> TypeId {
    let _expr_ty = tc.infer_expr(top.expr);
    match top.op {
        TypeOpKind::As => {
            // `expr as T` → T.
            tc.resolve_type_ref_id(top.ty)
        }
        TypeOpKind::Is => {
            // `expr is T` → Bool.
            tc.types.env.intern_scalar(vx_types::Scalar::Bool)
        }
        TypeOpKind::Ascription => {
            // `expr :: T` → T (type annotation, like `as` but documentation-only).
            tc.resolve_type_ref_id(top.ty)
        }
    }
}
