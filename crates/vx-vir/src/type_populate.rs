//! Transfer types from the existing AST-based type checker onto VIR.
//!
//! This is a transitional compatibility step. Expression types are populated by
//! exact [`SourceSpan`] match against [`TypeCheckResult::expr_types`]. Binding
//! types are derived from initializer expressions (for let bindings) or from
//! [`TypeCheckResult::binding_types`] (for parameters).
//!
//! Unmatched, synthetic, and error expressions are left (or cleared to) `None`.
//! A future VIR-native type checker will replace this.

use crate::BindingId;
use crate::arena::VirArena;
use crate::expr::ExprKind;
use crate::pattern::PatternKind;
use crate::stmt::StmtKind;
use vx_resolve::TypeCheckResult;
use vx_types::Ty;

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
        if let ExprKind::Block(ref b) = expr.kind
            && b.synthetic
        {
            expr.ty = None;
            continue;
        }
        expr.ty = type_check.expr_types.get(&expr.span).cloned();
    }
}

/// Populate `Binding.ty` from expression types and the type checker's binding types.
///
/// This function is safe to call multiple times: it clears all binding types
/// first, then repopulates.
///
/// Strategy by binding kind:
/// - **`Parameter`**: from `type_check.binding_types` by `name_span` match.
/// - **`Let`**: from the initializer expression's type (`arena[init].ty`).
///   Only for `PatternKind::Binding` patterns — destructuring patterns are
///   left as `None` (type decomposition is out of scope).
/// - **`PatternMatch` / `AtPattern` / `ArrayRest`**: left as `None`.
///   `AtPattern` is mechanically derivable in some cases (it binds the whole
///   matched value), but deferred for now.
pub fn populate_binding_types(arena: &mut VirArena, type_check: &TypeCheckResult) {
    // Clear all binding types first — prevents stale types on rerun.
    for binding in &mut arena.bindings {
        binding.ty = None;
    }

    // Pass 1: All bindings from type_check.binding_types by name_span match.
    // This covers let bindings, pattern match bindings, and at-pattern bindings
    // — everything the type checker's `type_pattern` inserts.
    // Parameter bindings are NOT in binding_types (they come from FnSig
    // internally in the checker), so they stay None for now.
    for binding in &mut arena.bindings {
        binding.ty = type_check.binding_types.get(&binding.name_span).cloned();
    }

    // Pass 2: Let bindings with simple PatternKind::Binding — derive type
    // from initializer expression when binding_types didn't have an entry.
    // This is a fallback for cases where the checker's binding_types missed
    // a binding but the init expr got typed.
    // Collect assignments to avoid borrow conflict.
    let assignments: Vec<(BindingId, Option<Ty>)> = arena
        .stmts
        .iter()
        .filter_map(|stmt| {
            if let StmtKind::Let(ref let_stmt) = stmt.kind {
                let pat = &arena.patterns[let_stmt.pattern.0 as usize];
                if let PatternKind::Binding(ref pb) = pat.kind {
                    // Only fill in if not already populated by pass 1.
                    if arena.bindings[pb.binding.0 as usize].ty.is_none() {
                        let init_ty = arena.exprs[let_stmt.init.0 as usize].ty.clone();
                        return Some((pb.binding, init_ty));
                    }
                }
            }
            None
        })
        .collect();

    for (binding_id, ty) in assignments {
        arena.bindings[binding_id.0 as usize].ty = ty;
    }
}

/// Get the result type of a body.
///
/// For block root expressions, this returns the tail expression's type (if any).
/// For non-block root expressions, returns the root expression's type directly.
/// No separate field is needed on `Body`.
pub fn body_result_ty(arena: &VirArena, body_id: crate::BodyId) -> Option<&Ty> {
    let root_expr = arena[body_id].root_expr;
    // If the root is a block with a tail, the result type is the tail's type.
    if let ExprKind::Block(ref b) = arena[root_expr].kind
        && let Some(tail) = b.tail
    {
        return arena[tail].ty.as_ref();
    }
    arena[root_expr].ty.as_ref()
}
