//! Capture analysis for VIR.
//!
//! Computes which bindings from ancestor bodies are referenced inside each
//! nested body (closure or local function). Populates `Body.captures`.
//!
//! The pass is bottom-up: children are analyzed before parents, so a parent
//! body can propagate captures from its nested children.

use std::collections::HashSet;

use crate::arena::VirArena;
use crate::expr::{ExprKind, UpdateField};
use crate::item::ItemKind;
use crate::scope::DefTarget;
use crate::stmt::StmtKind;
use crate::{BindingId, BodyId, ExprId, ItemId, StmtId};

/// Run capture analysis on a fully name-resolved arena.
///
/// Clears all `Body.captures` first, then repopulates.
pub fn analyze_captures(arena: &mut VirArena) {
    // Clear all captures up front (stale-state safety for reruns).
    for body in &mut arena.bodies {
        body.captures.clear();
    }

    // Build body tree and process in postorder (children before parents).
    let body_count = arena.bodies.len();
    let mut children: Vec<Vec<BodyId>> = vec![vec![]; body_count];
    for i in 0..body_count {
        let body_id = BodyId(i as u32);
        if let Some(parent) = arena[body_id].parent {
            children[parent.0 as usize].push(body_id);
        }
    }

    // Postorder traversal via iterative DFS.
    let mut order = Vec::with_capacity(body_count);
    let stack: Vec<BodyId> = (0..body_count)
        .map(|i| BodyId(i as u32))
        .filter(|&id| arena[id].parent.is_none())
        .collect();
    // DFS: push roots, expand children, reverse for postorder.
    let mut visit_stack = stack.clone();
    while let Some(id) = visit_stack.pop() {
        order.push(id);
        for &child in &children[id.0 as usize] {
            visit_stack.push(child);
        }
    }
    order.reverse(); // Now children come before parents.

    // Analyze each body in postorder.
    for body_id in order {
        if arena[body_id].parent.is_none() {
            continue; // Top-level bodies and top-level functions never capture.
        }
        let captures = collect_captures(arena, body_id);
        arena[body_id].captures = captures;
    }
}

/// Collect captures for a single body.
fn collect_captures(arena: &VirArena, body_id: BodyId) -> Vec<BindingId> {
    let mut captured = HashSet::new();
    let root_expr = arena[body_id].root_expr;
    visit_expr(arena, body_id, root_expr, &mut captured);
    let mut result: Vec<BindingId> = captured.into_iter().collect();
    result.sort();
    result
}

fn visit_expr(
    arena: &VirArena,
    body_id: BodyId,
    expr_id: ExprId,
    captured: &mut HashSet<BindingId>,
) {
    let kind = arena[expr_id].kind.clone();
    match kind {
        ExprKind::Bomb => {}
        ExprKind::NameRef(nr) => {
            if let Some(DefTarget::Binding(bid)) = nr.resolved {
                let binding_body = arena[bid].body;
                if is_ancestor_body(arena, binding_body, body_id) {
                    captured.insert(bid);
                }
            }
        }
        ExprKind::Closure(c) => {
            // Stop: don't recurse into child body. Propagate its captures.
            propagate_child_captures(arena, body_id, c.body, captured);
        }
        ExprKind::Block(block) => {
            for &stmt_id in &block.stmts {
                visit_stmt(arena, body_id, stmt_id, captured);
            }
            if let Some(tail) = block.tail {
                visit_expr(arena, body_id, tail, captured);
            }
        }
        ExprKind::If(if_) => {
            visit_expr(arena, body_id, if_.condition, captured);
            visit_expr(arena, body_id, if_.then_branch, captured);
            if let Some(else_br) = if_.else_branch {
                visit_expr(arena, body_id, else_br, captured);
            }
        }
        ExprKind::Match(m) => {
            visit_expr(arena, body_id, m.scrutinee, captured);
            for arm in &m.arms {
                visit_expr(arena, body_id, arm.body, captured);
            }
        }
        ExprKind::Binary(bin) => {
            visit_expr(arena, body_id, bin.lhs, captured);
            visit_expr(arena, body_id, bin.rhs, captured);
        }
        ExprKind::Prefix(pre) => {
            visit_expr(arena, body_id, pre.operand, captured);
        }
        ExprKind::Call(call) => {
            visit_expr(arena, body_id, call.callee, captured);
            for arg in &call.args {
                visit_expr(arena, body_id, arg.expr, captured);
            }
        }
        ExprKind::Where(w) => {
            visit_expr(arena, body_id, w.callee, captured);
            visit_expr(arena, body_id, w.keywords, captured);
        }
        ExprKind::Invoke(inv) => {
            visit_expr(arena, body_id, inv.callee, captured);
        }
        ExprKind::Field(f) => {
            visit_expr(arena, body_id, f.receiver, captured);
        }
        ExprKind::Index(idx) => {
            visit_expr(arena, body_id, idx.receiver, captured);
            visit_expr(arena, body_id, idx.index, captured);
        }
        ExprKind::Array(arr) => {
            for elem in &arr.elements {
                match elem {
                    crate::expr::ArrayElement::Expr(id) | crate::expr::ArrayElement::Spread(id) => {
                        visit_expr(arena, body_id, *id, captured);
                    }
                }
            }
        }
        ExprKind::Tuple(t) => {
            for &elem in &t.elements {
                visit_expr(arena, body_id, elem, captured);
            }
        }
        ExprKind::Record(r) => {
            for field in &r.fields {
                visit_expr(arena, body_id, field.value, captured);
            }
        }
        ExprKind::Map(m) => {
            for elem in &m.entries {
                match elem {
                    crate::expr::MapElement::Entry(entry) => {
                        if let crate::expr::MapKey::Expr(k) = entry.key {
                            visit_expr(arena, body_id, k, captured);
                        }
                        visit_expr(arena, body_id, entry.value, captured);
                    }
                    crate::expr::MapElement::Spread(id) => {
                        visit_expr(arena, body_id, *id, captured);
                    }
                }
            }
        }
        ExprKind::StructLiteral(sl) => {
            for field in &sl.fields {
                visit_expr(arena, body_id, field.value, captured);
            }
        }
        ExprKind::With(w) => {
            visit_expr(arena, body_id, w.receiver, captured);
            visit_update_fields(arena, body_id, &w.updates, captured);
        }
        ExprKind::Fallible(f) => {
            visit_expr(arena, body_id, f.expr, captured);
        }
        ExprKind::Fail(f) => {
            if let Some(msg) = f.message {
                visit_expr(arena, body_id, msg, captured);
            }
        }
        ExprKind::TypeOp(top) => {
            visit_expr(arena, body_id, top.expr, captured);
        }
        ExprKind::Range(r) => {
            if let Some(s) = r.start {
                visit_expr(arena, body_id, s, captured);
            }
            if let Some(e) = r.end {
                visit_expr(arena, body_id, e, captured);
            }
        }
        ExprKind::InterpolatedString(is) => {
            for part in &is.parts {
                if let crate::expr::StringPart::Expr(id) = part {
                    visit_expr(arena, body_id, *id, captured);
                }
            }
        }
        ExprKind::CliLiteral(cli) => {
            for arg in &cli.args {
                for piece in &arg.pieces {
                    if let crate::expr::StringPart::Expr(id) = piece {
                        visit_expr(arena, body_id, *id, captured);
                    }
                }
            }
        }
        ExprKind::PartialApp(pa) => {
            visit_expr(arena, body_id, pa.callee, captured);
            for arg in &pa.args {
                if let crate::expr::PartialArg::Expr(id) = arg {
                    visit_expr(arena, body_id, *id, captured);
                }
            }
        }
        ExprKind::Literal(_) | ExprKind::PathLiteral(_) | ExprKind::Import(_) | ExprKind::Error => {
        }
    }
}

fn visit_stmt(
    arena: &VirArena,
    body_id: BodyId,
    stmt_id: StmtId,
    captured: &mut HashSet<BindingId>,
) {
    let kind = arena[stmt_id].kind.clone();
    match kind {
        StmtKind::Let(let_) => {
            visit_expr(arena, body_id, let_.init, captured);
        }
        StmtKind::Expr(expr_id) => {
            visit_expr(arena, body_id, expr_id, captured);
        }
        StmtKind::Return(ret) => {
            if let Some(val) = ret.value {
                visit_expr(arena, body_id, val, captured);
            }
        }
        StmtKind::Assign(assign) => {
            visit_expr(arena, body_id, assign.value, captured);
        }
        StmtKind::CompoundUpdate(cu) => {
            visit_update_fields(arena, body_id, &cu.updates, captured);
        }
        StmtKind::FieldAssign(fa) => {
            visit_expr(arena, body_id, fa.value, captured);
        }
        StmtKind::Item(item_id) => {
            visit_item(arena, body_id, item_id, captured);
        }
        StmtKind::Error => {}
    }
}

fn visit_item(
    arena: &VirArena,
    body_id: BodyId,
    item_id: ItemId,
    captured: &mut HashSet<BindingId>,
) {
    let kind = arena[item_id].kind.clone();
    match kind {
        ItemKind::Function(f) => {
            // Stop: don't recurse into function body. Propagate its captures.
            propagate_child_captures(arena, body_id, f.body, captured);
        }
        ItemKind::Namespace(ns) => {
            // Namespaces can appear in executable code. Recurse into child items
            // to propagate captures from any functions inside.
            for &child in &ns.items {
                visit_item(arena, body_id, child, captured);
            }
        }
        _ => {}
    }
}

fn visit_update_fields(
    arena: &VirArena,
    body_id: BodyId,
    updates: &[UpdateField],
    captured: &mut HashSet<BindingId>,
) {
    for field in updates {
        match field {
            UpdateField::Set { value, .. } => visit_expr(arena, body_id, *value, captured),
            UpdateField::Nested { updates, .. } => {
                visit_update_fields(arena, body_id, updates, captured)
            }
        }
    }
}

/// Propagate captures from a child body into the current body.
/// A child's capture belongs to the current body's captures if the binding
/// is owned by an ancestor of the current body (not the current body itself).
fn propagate_child_captures(
    arena: &VirArena,
    body_id: BodyId,
    child_body_id: BodyId,
    captured: &mut HashSet<BindingId>,
) {
    let child_captures = arena[child_body_id].captures.clone();
    for bid in child_captures {
        let binding_body = arena[bid].body;
        if is_ancestor_body(arena, binding_body, body_id) {
            captured.insert(bid);
        }
    }
}

/// Check if `candidate` is an ancestor of `current` (walking up the parent chain).
fn is_ancestor_body(arena: &VirArena, candidate: BodyId, current: BodyId) -> bool {
    let mut walk = arena[current].parent;
    while let Some(ancestor) = walk {
        if ancestor == candidate {
            return true;
        }
        walk = arena[ancestor].parent;
    }
    false
}
