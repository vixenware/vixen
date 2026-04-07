//! With, fallible, fail, type_op, range, and import inference.

use vx_types::Ty;

use crate::ExprId;
use crate::expr::{
    ExprKind, FailExpr, FallibleExpr, TypeOpExpr, TypeOpKind, UpdateField, WithExpr,
};

use super::TypeChecker;

pub(crate) fn infer_with(tc: &mut TypeChecker, w: &WithExpr) -> Ty {
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

pub(crate) fn infer_fallible(tc: &mut TypeChecker, f: &FallibleExpr) -> Ty {
    let inner_ty = tc.infer_expr(f.expr);
    if inner_ty.is_poison() {
        return Ty::Poison;
    }
    match inner_ty {
        Ty::Function(fn_ty) => Ty::Function(vx_types::FunctionTy {
            params: fn_ty.params,
            ret: Box::new(Ty::Optional(Box::new((*fn_ty.ret).clone()))),
            capabilities: fn_ty.capabilities,
        }),
        other => Ty::Optional(Box::new(other)),
    }
}

pub(crate) fn infer_fail(tc: &mut TypeChecker, f: &FailExpr) -> Ty {
    if let Some(msg) = f.message {
        tc.infer_expr(msg);
    }
    Ty::Poison
}

pub(crate) fn infer_range(tc: &mut TypeChecker, expr_id: ExprId) -> Ty {
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
    Ty::Poison
}

pub(crate) fn infer_type_op(tc: &mut TypeChecker, top: &TypeOpExpr) -> Ty {
    let _expr_ty = tc.infer_expr(top.expr);
    match top.op {
        TypeOpKind::As => {
            // `expr as T` → T.
            tc.resolve_type_ref_id(top.ty)
        }
        TypeOpKind::Is => {
            // `expr is T` → Bool.
            Ty::Scalar(vx_types::Scalar::Bool)
        }
        TypeOpKind::Ascription => {
            // `expr :: T` → T (type annotation, like `as` but documentation-only).
            tc.resolve_type_ref_id(top.ty)
        }
    }
}
