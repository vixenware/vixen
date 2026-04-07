//! Demand analysis for VIR.
//!
//! Builds the call graph, selects entry points, propagates demand observations,
//! and computes reachability. Produces a [`VirDemandResult`] consumed by later
//! passes (lazy field seeding, lowering).
//!
//! Entry point: [`analyze_demand`].

use std::collections::BTreeSet;

use crate::body::BodyOwner;
use crate::build::VirProgram;
use crate::expr::ExprKind;
use crate::item::ItemKind;
use crate::scope::DefTarget;
use crate::stmt::StmtKind;
use crate::{BodyId, ExprId, ItemId, StmtId};

/// Demand observation for a body's result.
///
/// Mirrors `vx_analysis::Observation` but lives in vx-vir to avoid
/// a dependency on the analysis crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// Not demanded — dead code.
    NoObs,
    /// Value is needed (eager evaluation required).
    NeedValue,
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Result of demand analysis on a VIR program.
#[derive(Debug, Clone)]
pub struct VirDemandResult {
    /// Per-body analysis results.
    pub bodies: Vec<VirAnalyzedBody>,
    /// Which bodies are reachable from entry points.
    pub reachable: BTreeSet<BodyId>,
    /// Entry point body IDs.
    pub entry_points: Vec<BodyId>,
}

/// Per-body demand analysis result.
#[derive(Debug, Clone)]
pub struct VirAnalyzedBody {
    pub body_id: BodyId,
    pub result_observation: Observation,
    pub call_edges: Vec<VirCallEdge>,
}

/// A call edge in the call graph.
#[derive(Debug, Clone)]
pub struct VirCallEdge {
    pub call_expr: ExprId,
    pub callee: VirCallTarget,
}

/// The resolved target of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirCallTarget {
    /// Direct call to a named function's body.
    Body(BodyId),
    /// Call through a closure binding — we know the closure's body.
    Closure(BodyId),
    /// Call through a binding we can't statically resolve to a body.
    Unknown,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run demand analysis on a fully built VIR program.
pub fn analyze_demand(program: &VirProgram) -> VirDemandResult {
    let arena = &program.arena;

    // Step 1: Build call graph for every body.
    let mut bodies: Vec<VirAnalyzedBody> = arena
        .bodies
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let body_id = BodyId(i as u32);
            let mut edges = Vec::new();
            let root_expr = arena[body_id].root_expr;
            collect_call_edges(arena, body_id, root_expr, &mut edges);
            VirAnalyzedBody {
                body_id,
                result_observation: Observation::NoObs,
                call_edges: edges,
            }
        })
        .collect();

    // Step 2: Select entry points.
    let entry_points = select_entry_points(program, &bodies);

    // Step 3+4: Demand propagation + reachability via BFS.
    let mut reachable = BTreeSet::new();
    let mut queue: Vec<BodyId> = entry_points.clone();
    for &entry in &entry_points {
        reachable.insert(entry);
    }

    while let Some(caller_id) = queue.pop() {
        let caller = &bodies[caller_id.0 as usize];
        for edge in &caller.call_edges {
            let callee_body = match edge.callee {
                VirCallTarget::Body(b) | VirCallTarget::Closure(b) => b,
                VirCallTarget::Unknown => continue,
            };
            if reachable.insert(callee_body) {
                queue.push(callee_body);
            }
        }
    }

    // Mark reachable bodies as NeedValue.
    for body in &mut bodies {
        if reachable.contains(&body.body_id) {
            body.result_observation = Observation::NeedValue;
        }
    }

    VirDemandResult {
        bodies,
        reachable,
        entry_points,
    }
}

// ---------------------------------------------------------------------------
// Call graph extraction
// ---------------------------------------------------------------------------

fn collect_call_edges(
    arena: &crate::VirArena,
    body_id: BodyId,
    expr_id: ExprId,
    edges: &mut Vec<VirCallEdge>,
) {
    let kind = arena[expr_id].kind.clone();
    match kind {
        ExprKind::Call(call) => {
            // This Call owns its edge.
            let target = resolve_callee(arena, call.callee);
            edges.push(VirCallEdge {
                call_expr: expr_id,
                callee: target,
            });
            // Recurse into callee subtree (may contain nested calls).
            collect_call_edges(arena, body_id, call.callee, edges);
            // Recurse into args.
            for arg in &call.args {
                collect_call_edges(arena, body_id, arg.expr, edges);
            }
        }
        ExprKind::Invoke(inv) => {
            // Invoke owns its edge.
            let target = resolve_callee(arena, inv.callee);
            edges.push(VirCallEdge {
                call_expr: expr_id,
                callee: target,
            });
            // Recurse into callee subtree.
            collect_call_edges(arena, body_id, inv.callee, edges);
        }
        ExprKind::Where(w) => {
            // Check if callee is a Call or Invoke.
            let callee_is_call_or_invoke = matches!(
                arena[w.callee].kind,
                ExprKind::Call(_) | ExprKind::Invoke(_)
            );
            if callee_is_call_or_invoke {
                // Inner Call/Invoke will create the edge. Just recurse.
                collect_call_edges(arena, body_id, w.callee, edges);
            } else {
                // Bare callee (e.g. `f where { x: 1 }`) — Where owns the edge.
                let target = resolve_callee(arena, w.callee);
                edges.push(VirCallEdge {
                    call_expr: expr_id,
                    callee: target,
                });
                // Callee was consumed by resolution; don't recurse into it
                // (it's a bare NameRef, no nested calls).
            }
            // Always recurse into keywords.
            collect_call_edges(arena, body_id, w.keywords, edges);
        }
        ExprKind::Closure(_) => {
            // Stop: child body is analyzed separately.
        }
        ExprKind::Block(block) => {
            for &stmt_id in &block.stmts {
                visit_stmt_for_edges(arena, body_id, stmt_id, edges);
            }
            if let Some(tail) = block.tail {
                collect_call_edges(arena, body_id, tail, edges);
            }
        }
        ExprKind::If(if_) => {
            collect_call_edges(arena, body_id, if_.condition, edges);
            collect_call_edges(arena, body_id, if_.then_branch, edges);
            if let Some(else_br) = if_.else_branch {
                collect_call_edges(arena, body_id, else_br, edges);
            }
        }
        ExprKind::Match(m) => {
            collect_call_edges(arena, body_id, m.scrutinee, edges);
            for arm in &m.arms {
                collect_call_edges(arena, body_id, arm.body, edges);
            }
        }
        ExprKind::Binary(bin) => {
            collect_call_edges(arena, body_id, bin.lhs, edges);
            collect_call_edges(arena, body_id, bin.rhs, edges);
        }
        ExprKind::Prefix(pre) => {
            collect_call_edges(arena, body_id, pre.operand, edges);
        }
        ExprKind::Field(f) => {
            collect_call_edges(arena, body_id, f.receiver, edges);
        }
        ExprKind::Index(idx) => {
            collect_call_edges(arena, body_id, idx.receiver, edges);
            collect_call_edges(arena, body_id, idx.index, edges);
        }
        ExprKind::Array(arr) => {
            for elem in &arr.elements {
                match elem {
                    crate::expr::ArrayElement::Expr(id) | crate::expr::ArrayElement::Spread(id) => {
                        collect_call_edges(arena, body_id, *id, edges);
                    }
                }
            }
        }
        ExprKind::Tuple(t) => {
            for &elem in &t.elements {
                collect_call_edges(arena, body_id, elem, edges);
            }
        }
        ExprKind::Record(r) => {
            for field in &r.fields {
                collect_call_edges(arena, body_id, field.value, edges);
            }
        }
        ExprKind::Map(m) => {
            for elem in &m.entries {
                match elem {
                    crate::expr::MapElement::Entry(entry) => {
                        if let crate::expr::MapKey::Expr(k) = entry.key {
                            collect_call_edges(arena, body_id, k, edges);
                        }
                        collect_call_edges(arena, body_id, entry.value, edges);
                    }
                    crate::expr::MapElement::Spread(id) => {
                        collect_call_edges(arena, body_id, *id, edges);
                    }
                }
            }
        }
        ExprKind::StructLiteral(sl) => {
            for field in &sl.fields {
                collect_call_edges(arena, body_id, field.value, edges);
            }
        }
        ExprKind::With(w) => {
            collect_call_edges(arena, body_id, w.receiver, edges);
            visit_update_fields_for_edges(arena, body_id, &w.updates, edges);
        }
        ExprKind::Fallible(f) => {
            collect_call_edges(arena, body_id, f.expr, edges);
        }
        ExprKind::Fail(f) => {
            if let Some(msg) = f.message {
                collect_call_edges(arena, body_id, msg, edges);
            }
        }
        ExprKind::TypeOp(top) => {
            collect_call_edges(arena, body_id, top.expr, edges);
        }
        ExprKind::Range(r) => {
            if let Some(s) = r.start {
                collect_call_edges(arena, body_id, s, edges);
            }
            if let Some(e) = r.end {
                collect_call_edges(arena, body_id, e, edges);
            }
        }
        ExprKind::InterpolatedString(is) => {
            for part in &is.parts {
                if let crate::expr::StringPart::Expr(id) = part {
                    collect_call_edges(arena, body_id, *id, edges);
                }
            }
        }
        ExprKind::PartialApp(pa) => {
            collect_call_edges(arena, body_id, pa.callee, edges);
            for arg in &pa.args {
                if let crate::expr::PartialArg::Expr(id) = arg {
                    collect_call_edges(arena, body_id, *id, edges);
                }
            }
        }
        ExprKind::NameRef(_)
        | ExprKind::Literal(_)
        | ExprKind::PathLiteral(_)
        | ExprKind::Import(_)
        | ExprKind::Error => {}
    }
}

fn visit_stmt_for_edges(
    arena: &crate::VirArena,
    body_id: BodyId,
    stmt_id: StmtId,
    edges: &mut Vec<VirCallEdge>,
) {
    let kind = arena[stmt_id].kind.clone();
    match kind {
        StmtKind::Let(let_) => {
            collect_call_edges(arena, body_id, let_.init, edges);
        }
        StmtKind::Expr(expr_id) => {
            collect_call_edges(arena, body_id, expr_id, edges);
        }
        StmtKind::Return(ret) => {
            if let Some(val) = ret.value {
                collect_call_edges(arena, body_id, val, edges);
            }
        }
        StmtKind::Assign(assign) => {
            collect_call_edges(arena, body_id, assign.value, edges);
        }
        StmtKind::CompoundUpdate(cu) => {
            visit_update_fields_for_edges(arena, body_id, &cu.updates, edges);
        }
        StmtKind::FieldAssign(fa) => {
            collect_call_edges(arena, body_id, fa.value, edges);
        }
        StmtKind::Item(_) => {
            // Stop: all items (functions, namespaces) are body boundaries.
            // Their bodies are analyzed separately.
        }
        StmtKind::Error => {}
    }
}

fn visit_update_fields_for_edges(
    arena: &crate::VirArena,
    body_id: BodyId,
    updates: &[crate::expr::UpdateField],
    edges: &mut Vec<VirCallEdge>,
) {
    for field in updates {
        match field {
            crate::expr::UpdateField::Set { value, .. } => {
                collect_call_edges(arena, body_id, *value, edges);
            }
            crate::expr::UpdateField::Nested { updates, .. } => {
                visit_update_fields_for_edges(arena, body_id, updates, edges);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Callee resolution
// ---------------------------------------------------------------------------

fn resolve_callee(arena: &crate::VirArena, callee_expr: ExprId) -> VirCallTarget {
    match &arena[callee_expr].kind {
        ExprKind::NameRef(nr) => match nr.resolved {
            Some(DefTarget::Item(item_id)) => match &arena[item_id].kind {
                ItemKind::Function(f) => VirCallTarget::Body(f.body),
                _ => VirCallTarget::Unknown,
            },
            Some(DefTarget::Binding(bid)) => {
                if let Some(init_id) = arena[bid].init_expr {
                    if let ExprKind::Closure(c) = &arena[init_id].kind {
                        return VirCallTarget::Closure(c.body);
                    }
                }
                VirCallTarget::Unknown
            }
            _ => VirCallTarget::Unknown,
        },
        _ => VirCallTarget::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Entry point selection
// ---------------------------------------------------------------------------

fn select_entry_points(program: &VirProgram, bodies: &[VirAnalyzedBody]) -> Vec<BodyId> {
    let arena = &program.arena;

    // Collect top-level function bodies (parent: None, BodyOwner::Function).
    // Group by source unit for "primary source unit" filtering.
    let primary_su = program.source_units.first().copied();

    // Helper: get ItemId → function name and body for top-level functions.
    struct TopLevelFn {
        body_id: BodyId,
        item_id: ItemId,
        name: String,
    }

    let mut top_level_fns: Vec<TopLevelFn> = Vec::new();
    for (i, body) in arena.bodies.iter().enumerate() {
        let body_id = BodyId(i as u32);
        if body.parent.is_some() {
            continue; // Not top-level.
        }
        if let BodyOwner::Function { item } = body.owner {
            if let ItemKind::Function(f) = &arena[item].kind {
                top_level_fns.push(TopLevelFn {
                    body_id,
                    item_id: item,
                    name: f.name.clone(),
                });
            }
        }
    }

    // Items belonging to the primary source unit.
    let primary_items: BTreeSet<ItemId> = primary_su
        .map(|su| arena[su].items.iter().copied().collect())
        .unwrap_or_default();

    // 1. main from primary source unit.
    let mut entries: Vec<BodyId> = top_level_fns
        .iter()
        .filter(|f| f.name == "main" && primary_items.contains(&f.item_id))
        .map(|f| f.body_id)
        .collect();

    // 2. main from any source unit.
    if entries.is_empty() {
        entries = top_level_fns
            .iter()
            .filter(|f| f.name == "main")
            .map(|f| f.body_id)
            .collect();
    }

    // 3. build from primary source unit.
    if entries.is_empty() {
        entries = top_level_fns
            .iter()
            .filter(|f| f.name == "build" && primary_items.contains(&f.item_id))
            .map(|f| f.body_id)
            .collect();
    }

    // 4. build from any source unit.
    if entries.is_empty() {
        entries = top_level_fns
            .iter()
            .filter(|f| f.name == "build")
            .map(|f| f.body_id)
            .collect();
    }

    // 5. Root bodies: top-level functions not called by anyone.
    if entries.is_empty() {
        let called: BTreeSet<BodyId> = bodies
            .iter()
            .flat_map(|b| b.call_edges.iter())
            .filter_map(|edge| match edge.callee {
                VirCallTarget::Body(b) | VirCallTarget::Closure(b) => Some(b),
                VirCallTarget::Unknown => None,
            })
            .collect();

        let mut roots: Vec<BodyId> = top_level_fns
            .iter()
            .filter(|f| !called.contains(&f.body_id))
            .map(|f| f.body_id)
            .collect();
        roots.sort();
        if let Some(&first) = roots.first() {
            entries.push(first);
        }
    }

    // 6. First top-level function body.
    if entries.is_empty() {
        if let Some(f) = top_level_fns.first() {
            entries.push(f.body_id);
        }
    }

    entries.sort();
    entries.dedup();
    entries
}
