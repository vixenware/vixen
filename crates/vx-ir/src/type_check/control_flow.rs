//! If, match, block, and statement checking.

use vx_types::Ty;

use crate::StmtId;
use crate::expr::{BlockExpr, IfExpr, MatchExpr};
use crate::stmt::StmtKind;

use super::TypeChecker;
use super::patterns::type_pattern;
use crate::diagnostic::IrDiagnostic;

pub(crate) fn infer_block(tc: &mut TypeChecker, block: &BlockExpr) -> Ty {
    let stmts = block.stmts.clone();
    let tail = block.tail;
    let is_synthetic = block.synthetic;

    let stmt_count = stmts.len();
    for (i, &stmt_id) in stmts.iter().enumerate() {
        let is_last = i + 1 == stmt_count && tail.is_none();
        check_stmt(tc, stmt_id, is_last && !is_synthetic);
    }

    if let Some(tail_id) = tail {
        tc.infer_expr(tail_id)
    } else {
        Ty::Unit
    }
}

fn check_stmt(tc: &mut TypeChecker, stmt_id: StmtId, is_tail: bool) {
    let kind = tc.arena[stmt_id].kind.clone();

    match kind {
        StmtKind::Let(let_) => {
            // Resolve annotation type.
            let annotation_ty = let_
                .ty
                .map(|tr_id| tc.resolve_type_ref_id(tr_id))
                .filter(|t| !t.is_poison());

            let inferred_ty = tc.infer_expr(let_.init);
            // Annotation wins, inferred is fallback.
            let ty = annotation_ty.unwrap_or(inferred_ty);
            if !ty.is_poison() {
                type_pattern(tc, let_.pattern, &ty);
            }
        }
        StmtKind::Assign(assign) => {
            tc.infer_expr(assign.value);
        }
        StmtKind::CompoundUpdate(cu) => {
            // Infer update field values.
            infer_update_fields(tc, &cu.updates);
        }
        StmtKind::FieldAssign(fa) => {
            tc.infer_expr(fa.value);
        }
        StmtKind::Return(ret) => {
            if let Some(val) = ret.value {
                tc.infer_expr(val);
            }
        }
        StmtKind::Item(item_id) => {
            tc.check_item(item_id);
        }
        StmtKind::Expr(expr_id) => {
            let ty = tc.infer_expr(expr_id);
            if is_tail {
                // Tail expression of a block — its type is the block's type.
                // This case is handled by the caller.
            } else {
                let span = tc.arena[expr_id].span;
                tc.push_diagnostic(IrDiagnostic::UnusedExprStatement { expr: span });
            }
            let _ = ty;
        }
        StmtKind::Error => {}
    }
}

fn infer_update_fields(tc: &mut TypeChecker, updates: &[crate::expr::UpdateField]) {
    for field in updates {
        match field {
            crate::expr::UpdateField::Set { value, .. } => {
                tc.infer_expr(*value);
            }
            crate::expr::UpdateField::Nested { updates, .. } => {
                infer_update_fields(tc, updates);
            }
        }
    }
}

pub(crate) fn infer_if(tc: &mut TypeChecker, if_: &IfExpr) -> Ty {
    tc.infer_expr(if_.condition);
    let then_ty = tc.infer_expr(if_.then_branch);
    if let Some(else_br) = if_.else_branch {
        tc.infer_expr(else_br);
    }
    // Old checker returns then-branch type only (no unification).
    then_ty
}

pub(crate) fn infer_match(tc: &mut TypeChecker, m: &MatchExpr) -> Ty {
    let scrutinee_ty = tc.infer_expr(m.scrutinee);

    let mut result_ty = Ty::Poison;
    let arms = m.arms.clone();
    for arm in &arms {
        type_pattern(tc, arm.pattern, &scrutinee_ty);
        let arm_ty = tc.infer_expr(arm.body);
        if result_ty.is_poison() && !arm_ty.is_poison() {
            result_ty = arm_ty;
        }
    }
    result_ty
}
