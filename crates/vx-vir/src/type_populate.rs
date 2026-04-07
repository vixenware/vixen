//! Transfer expression types from the existing AST-based type checker onto VIR.
//!
//! This is a transitional compatibility step. It populates [`Expr::ty`] by
//! exact [`SourceSpan`] match against [`TypeCheckResult::expr_types`]. Unmatched,
//! synthetic, and error expressions are left (or cleared to) `None`.
//!
//! A future VIR-native type checker will replace this.

use crate::arena::VirArena;
use crate::expr::ExprKind;
use vx_resolve::TypeCheckResult;

/// Populate `Expr.ty` from the existing type checker's span-keyed results.
///
/// This function is safe to call multiple times: it unconditionally assigns
/// each expression's type (clearing stale values when the checker no longer
/// has an entry for that span).
///
/// Explicitly skips:
/// - `ExprKind::Error` — recovery nodes often reuse the parent span and would
///   accidentally pick up the parent's type.
/// - Synthetic blocks (`BlockExpr { synthetic: true }`) — file-spanning spans
///   that don't correspond to any real expression the checker typed.
pub fn populate_expr_types(arena: &mut VirArena, type_check: &TypeCheckResult) {
    for expr in &mut arena.exprs {
        if matches!(expr.kind, ExprKind::Error) {
            expr.ty = None;
            continue;
        }
        if let ExprKind::Block(ref b) = expr.kind {
            if b.synthetic {
                expr.ty = None;
                continue;
            }
        }
        expr.ty = type_check.expr_types.get(&expr.span).cloned();
    }
}
