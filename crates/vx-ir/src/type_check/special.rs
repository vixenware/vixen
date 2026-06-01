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
    // `expr? : Result[T, E]` — the failure model's catch (was the old `Option[T]`).
    // E is the inferred union of error types reachable at this `?`; until the
    // call-graph union inference lands it is `String`, the type of a bare
    // `fail "…"` (the common case).
    // TODO(union-inference): replace `String` with the inferred error union.
    let err_ty = tc.types.env.intern_scalar(vx_types::Scalar::String);
    let inner_clone = tc.types.env.get_ty(inner_ty).clone();
    match inner_clone {
        Ty::Function(fn_ty) => {
            let res_ret = intern_result(tc, fn_ty.ret, err_ty);
            tc.types
                .env
                .intern_function(fn_ty.params, res_ret, fn_ty.capabilities)
        }
        _ => intern_result(tc, inner_ty, err_ty),
    }
}

/// Intern `Result[ok, err]` from the stdlib `Result` enum. Poisons if `Result`
/// isn't in scope (it is — it's declared in `core.vx`).
fn intern_result(tc: &mut TypeChecker, ok: TypeId, err: TypeId) -> TypeId {
    let result_def = tc
        .types
        .env
        .lookup_def("Result")
        .iter()
        .copied()
        .find(|&id| matches!(tc.types.env.get_def(id), vx_types::TypeDef::Enum(_)));
    match result_def {
        Some(def_id) => tc.types.env.intern_enum(def_id, vec![ok, err]),
        None => tc.poison(),
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
