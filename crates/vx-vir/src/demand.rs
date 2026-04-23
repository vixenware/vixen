//! Demand analysis for VIR.
//!
//! Builds the call graph, selects entry points, propagates demand observations,
//! and computes reachability. Produces a [`VirDemandResult`] consumed by later
//! passes (lazy field seeding, lowering).
//!
//! Entry point: [`analyze_demand`].

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::binding::BindingKind;
use crate::body::BodyOwner;
use crate::build::VirProgram;
use crate::expr::{ExprKind, FieldKey};
use crate::item::ItemKind;
use crate::scope::DefTarget;
use crate::stmt::StmtKind;
use crate::{BindingId, BodyId, ExprId, ItemId, StmtId};

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
    /// Product fields marked for lazy (thunked) evaluation.
    pub lazy_fields: Vec<LazyField>,
}

/// A product field marked for lazy (thunked) evaluation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LazyField {
    /// The body containing the product construction.
    pub body: BodyId,
    /// The product construction expression (Tuple, Record, or StructLiteral).
    pub product_expr: ExprId,
    /// The field to make lazy.
    pub field: FieldKey,
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

    // Step 5: Lazy field seeding.
    let lazy_fields = seed_lazy_fields(arena, &bodies, &reachable);

    VirDemandResult {
        bodies,
        reachable,
        entry_points,
        lazy_fields,
    }
}

/// Run demand analysis with an explicit entry point (no auto-selection).
///
/// Used by vx-rt where the caller already knows which function to run.
pub fn analyze_demand_for_entry(program: &VirProgram, entry: BodyId) -> VirDemandResult {
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

    let entry_points = vec![entry];

    // BFS reachability from the single entry.
    let mut reachable = BTreeSet::new();
    let mut queue: Vec<BodyId> = entry_points.clone();
    reachable.insert(entry);

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

    for body in &mut bodies {
        if reachable.contains(&body.body_id) {
            body.result_observation = Observation::NeedValue;
        }
    }

    let lazy_fields = seed_lazy_fields(arena, &bodies, &reachable);

    VirDemandResult {
        bodies,
        reachable,
        entry_points,
        lazy_fields,
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
            // Only recurse into non-trivial callees. A direct name-ref callee is
            // already accounted for by the call edge above; recursing into it
            // would incorrectly treat ordinary direct calls as function-as-value
            // reachability and pull declaration-only stdlib bodies into the
            // reachable set.
            if !matches!(arena[call.callee].kind, ExprKind::NameRef(_)) {
                collect_call_edges(arena, body_id, call.callee, edges);
            }
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
            if !matches!(arena[inv.callee].kind, ExprKind::NameRef(_)) {
                collect_call_edges(arena, body_id, inv.callee, edges);
            }
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
        ExprKind::NameRef(ref nr) => {
            // Function-as-value: if a NameRef resolves to a function item,
            // that function's body is reachable (it may be called indirectly).
            if let Some(DefTarget::Item(item_id)) = nr.resolved {
                if let ItemKind::Function(ref f) = arena[item_id].kind
                    && !matches!(
                        arena[arena[f.body].root_expr].kind,
                        crate::expr::ExprKind::Error
                    )
                {
                    edges.push(VirCallEdge {
                        call_expr: expr_id,
                        callee: VirCallTarget::Body(f.body),
                    });
                }
            }
        }
        ExprKind::Literal(_) | ExprKind::PathLiteral(_) | ExprKind::Import(_) | ExprKind::Error => {
        }
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
                ItemKind::Function(f) => {
                    if matches!(
                        arena[arena[f.body].root_expr].kind,
                        crate::expr::ExprKind::Error
                    ) {
                        VirCallTarget::Unknown
                    } else {
                        VirCallTarget::Body(f.body)
                    }
                }
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

// ---------------------------------------------------------------------------
// Lazy field seeding
// ---------------------------------------------------------------------------

/// Seed lazy fields across all reachable bodies.
///
/// Aggregates projected fields from both local projections and cross-function
/// parameter projections, then computes the complement for each product.
fn seed_lazy_fields(
    arena: &crate::VirArena,
    bodies: &[VirAnalyzedBody],
    reachable: &BTreeSet<BodyId>,
) -> Vec<LazyField> {
    // Map: (body, product_expr) → projected fields. Aggregated across all uses.
    let mut projected: HashMap<(BodyId, ExprId), BTreeSet<FieldKey>> = HashMap::new();

    // Step 1: Collect locally projected fields.
    for &body_id in reachable {
        let root_expr = arena[body_id].root_expr;
        collect_local_projections(arena, body_id, root_expr, &mut projected);
    }

    // Step 2: Collect cross-function projected fields.
    // Cache callee parameter projections to avoid re-walking.
    let mut param_proj_cache: HashMap<BodyId, HashMap<BindingId, BTreeSet<FieldKey>>> =
        HashMap::new();

    for analyzed in bodies {
        if !reachable.contains(&analyzed.body_id) {
            continue;
        }
        for edge in &analyzed.call_edges {
            let callee_body_id = match edge.callee {
                VirCallTarget::Body(b) | VirCallTarget::Closure(b) => b,
                VirCallTarget::Unknown => continue,
            };

            // Get the Call's args from the expression.
            let call_args = match &arena[edge.call_expr].kind {
                ExprKind::Call(call) => &call.args,
                _ => continue, // Invoke/Where-bare have no positional args.
            };

            // Get callee's positional params.
            let positional_params: Vec<_> = arena[callee_body_id]
                .params
                .iter()
                .filter(|p| !p.is_keyword)
                .collect();

            if call_args.len() != positional_params.len() {
                continue;
            }

            // Get or compute callee's parameter projections.
            let callee_projs = param_proj_cache
                .entry(callee_body_id)
                .or_insert_with(|| collect_param_projections(arena, callee_body_id));

            for (arg, param) in call_args.iter().zip(positional_params.iter()) {
                // Get the BindingId for this param's pattern.
                let param_binding = match &arena[param.pattern].kind {
                    crate::pattern::PatternKind::Binding(b) => b.binding,
                    _ => continue, // Destructuring param — skip.
                };

                let Some(param_fields) = callee_projs.get(&param_binding) else {
                    continue; // Param not projected at all — all fields would be lazy.
                    // But we still need to resolve the product to seed.
                    // Actually: if param isn't projected, there's nothing to add
                    // to the projected set. The local phase may still seed laziness.
                    // Fall through below handles this correctly.
                };

                // Resolve caller-side argument to a product.
                let mut visited = HashSet::new();
                let Some((product_expr, _)) = resolve_to_product(arena, arg.expr, &mut visited)
                else {
                    continue;
                };
                let product_body = arena[product_expr].body;

                // Add callee's projected fields to the product's projected set.
                projected
                    .entry((product_body, product_expr))
                    .or_default()
                    .extend(param_fields.iter().cloned());
            }
        }
    }

    // Also handle the case where callee projects NO fields from a param —
    // we need to add the product to the map with empty projected set so
    // all fields become lazy. Re-walk call edges for this case.
    for analyzed in bodies {
        if !reachable.contains(&analyzed.body_id) {
            continue;
        }
        for edge in &analyzed.call_edges {
            let callee_body_id = match edge.callee {
                VirCallTarget::Body(b) | VirCallTarget::Closure(b) => b,
                VirCallTarget::Unknown => continue,
            };
            let call_args = match &arena[edge.call_expr].kind {
                ExprKind::Call(call) => &call.args,
                _ => continue,
            };
            let positional_params: Vec<_> = arena[callee_body_id]
                .params
                .iter()
                .filter(|p| !p.is_keyword)
                .collect();
            if call_args.len() != positional_params.len() {
                continue;
            }
            let callee_projs = param_proj_cache.get(&callee_body_id);
            for (arg, param) in call_args.iter().zip(positional_params.iter()) {
                let param_binding = match &arena[param.pattern].kind {
                    crate::pattern::PatternKind::Binding(b) => b.binding,
                    _ => continue,
                };
                // If callee doesn't project this param at all, ensure product
                // is in the map (with no additional projections).
                let has_projections = callee_projs
                    .and_then(|cp| cp.get(&param_binding))
                    .is_some_and(|s| !s.is_empty());
                if !has_projections {
                    let mut visited = HashSet::new();
                    if let Some((product_expr, _)) =
                        resolve_to_product(arena, arg.expr, &mut visited)
                    {
                        let product_body = arena[product_expr].body;
                        projected.entry((product_body, product_expr)).or_default();
                    }
                }
            }
        }
    }

    // Step 3: Compute complement — lazy fields are all_fields - projected.
    let mut lazy_fields = Vec::new();
    for ((body, product_expr), proj_set) in &projected {
        let all_fields = product_field_keys(arena, *product_expr);
        for field in all_fields {
            if !proj_set.contains(&field) {
                lazy_fields.push(LazyField {
                    body: *body,
                    product_expr: *product_expr,
                    field,
                });
            }
        }
    }
    lazy_fields.sort();
    lazy_fields.dedup();
    lazy_fields
}

/// Get all field keys for a product expression.
fn product_field_keys(arena: &crate::VirArena, expr: ExprId) -> Vec<FieldKey> {
    match &arena[expr].kind {
        ExprKind::Tuple(t) => (0..t.elements.len())
            .map(|i| FieldKey::Index(i as u32))
            .collect(),
        ExprKind::Record(r) => r
            .fields
            .iter()
            .map(|f| FieldKey::Name(f.name.clone()))
            .collect(),
        ExprKind::StructLiteral(sl) => sl
            .fields
            .iter()
            .map(|f| FieldKey::Name(f.name.clone()))
            .collect(),
        _ => vec![],
    }
}

/// Collect local projections within a body (stopping at nested body boundaries).
fn collect_local_projections(
    arena: &crate::VirArena,
    body_id: BodyId,
    expr_id: ExprId,
    projected: &mut HashMap<(BodyId, ExprId), BTreeSet<FieldKey>>,
) {
    let kind = arena[expr_id].kind.clone();
    match kind {
        ExprKind::Field(f) => {
            // Try to resolve this projection to a product.
            let mut visited = HashSet::new();
            if let Some((product_expr, _)) = resolve_to_product(arena, f.receiver, &mut visited) {
                let product_body = arena[product_expr].body;
                projected
                    .entry((product_body, product_expr))
                    .or_default()
                    .insert(f.field.clone());
            }
            // Also recurse into receiver (there may be deeper projections).
            collect_local_projections(arena, body_id, f.receiver, projected);
        }
        ExprKind::Closure(_) => {
            // Stop: child body analyzed separately.
        }
        ExprKind::Block(block) => {
            for &stmt_id in &block.stmts {
                visit_stmt_for_projections(arena, body_id, stmt_id, projected);
            }
            if let Some(tail) = block.tail {
                collect_local_projections(arena, body_id, tail, projected);
            }
        }
        ExprKind::If(if_) => {
            collect_local_projections(arena, body_id, if_.condition, projected);
            collect_local_projections(arena, body_id, if_.then_branch, projected);
            if let Some(else_br) = if_.else_branch {
                collect_local_projections(arena, body_id, else_br, projected);
            }
        }
        ExprKind::Match(m) => {
            collect_local_projections(arena, body_id, m.scrutinee, projected);
            for arm in &m.arms {
                collect_local_projections(arena, body_id, arm.body, projected);
            }
        }
        ExprKind::Binary(bin) => {
            collect_local_projections(arena, body_id, bin.lhs, projected);
            collect_local_projections(arena, body_id, bin.rhs, projected);
        }
        ExprKind::Prefix(pre) => {
            collect_local_projections(arena, body_id, pre.operand, projected);
        }
        ExprKind::Call(call) => {
            collect_local_projections(arena, body_id, call.callee, projected);
            for arg in &call.args {
                collect_local_projections(arena, body_id, arg.expr, projected);
            }
        }
        ExprKind::Where(w) => {
            collect_local_projections(arena, body_id, w.callee, projected);
            collect_local_projections(arena, body_id, w.keywords, projected);
        }
        ExprKind::Invoke(inv) => {
            collect_local_projections(arena, body_id, inv.callee, projected);
        }
        ExprKind::Index(idx) => {
            collect_local_projections(arena, body_id, idx.receiver, projected);
            collect_local_projections(arena, body_id, idx.index, projected);
        }
        ExprKind::Array(arr) => {
            for elem in &arr.elements {
                match elem {
                    crate::expr::ArrayElement::Expr(id) | crate::expr::ArrayElement::Spread(id) => {
                        collect_local_projections(arena, body_id, *id, projected);
                    }
                }
            }
        }
        ExprKind::Tuple(t) => {
            for &elem in &t.elements {
                collect_local_projections(arena, body_id, elem, projected);
            }
        }
        ExprKind::Record(r) => {
            for field in &r.fields {
                collect_local_projections(arena, body_id, field.value, projected);
            }
        }
        ExprKind::Map(m) => {
            for elem in &m.entries {
                match elem {
                    crate::expr::MapElement::Entry(entry) => {
                        if let crate::expr::MapKey::Expr(k) = entry.key {
                            collect_local_projections(arena, body_id, k, projected);
                        }
                        collect_local_projections(arena, body_id, entry.value, projected);
                    }
                    crate::expr::MapElement::Spread(id) => {
                        collect_local_projections(arena, body_id, *id, projected);
                    }
                }
            }
        }
        ExprKind::StructLiteral(sl) => {
            for field in &sl.fields {
                collect_local_projections(arena, body_id, field.value, projected);
            }
        }
        ExprKind::With(w) => {
            collect_local_projections(arena, body_id, w.receiver, projected);
            for uf in &w.updates {
                visit_update_field_for_projections(arena, body_id, uf, projected);
            }
        }
        ExprKind::Fallible(f) => {
            collect_local_projections(arena, body_id, f.expr, projected);
        }
        ExprKind::Fail(f) => {
            if let Some(msg) = f.message {
                collect_local_projections(arena, body_id, msg, projected);
            }
        }
        ExprKind::TypeOp(top) => {
            collect_local_projections(arena, body_id, top.expr, projected);
        }
        ExprKind::Range(r) => {
            if let Some(s) = r.start {
                collect_local_projections(arena, body_id, s, projected);
            }
            if let Some(e) = r.end {
                collect_local_projections(arena, body_id, e, projected);
            }
        }
        ExprKind::InterpolatedString(is) => {
            for part in &is.parts {
                if let crate::expr::StringPart::Expr(id) = part {
                    collect_local_projections(arena, body_id, *id, projected);
                }
            }
        }
        ExprKind::PartialApp(pa) => {
            collect_local_projections(arena, body_id, pa.callee, projected);
            for arg in &pa.args {
                if let crate::expr::PartialArg::Expr(id) = arg {
                    collect_local_projections(arena, body_id, *id, projected);
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

fn visit_stmt_for_projections(
    arena: &crate::VirArena,
    body_id: BodyId,
    stmt_id: StmtId,
    projected: &mut HashMap<(BodyId, ExprId), BTreeSet<FieldKey>>,
) {
    let kind = arena[stmt_id].kind.clone();
    match kind {
        StmtKind::Let(let_) => {
            collect_local_projections(arena, body_id, let_.init, projected);
        }
        StmtKind::Expr(expr_id) => {
            collect_local_projections(arena, body_id, expr_id, projected);
        }
        StmtKind::Return(ret) => {
            if let Some(val) = ret.value {
                collect_local_projections(arena, body_id, val, projected);
            }
        }
        StmtKind::Assign(assign) => {
            collect_local_projections(arena, body_id, assign.value, projected);
        }
        StmtKind::CompoundUpdate(cu) => {
            for uf in &cu.updates {
                visit_update_field_for_projections(arena, body_id, uf, projected);
            }
        }
        StmtKind::FieldAssign(fa) => {
            collect_local_projections(arena, body_id, fa.value, projected);
        }
        StmtKind::Item(_) => {
            // Stop: all items are body boundaries.
        }
        StmtKind::Error => {}
    }
}

fn visit_update_field_for_projections(
    arena: &crate::VirArena,
    body_id: BodyId,
    uf: &crate::expr::UpdateField,
    projected: &mut HashMap<(BodyId, ExprId), BTreeSet<FieldKey>>,
) {
    match uf {
        crate::expr::UpdateField::Set { value, .. } => {
            collect_local_projections(arena, body_id, *value, projected);
        }
        crate::expr::UpdateField::Nested { updates, .. } => {
            for u in updates {
                visit_update_field_for_projections(arena, body_id, u, projected);
            }
        }
    }
}

/// Collect which fields each parameter has projected from it (direct only).
fn collect_param_projections(
    arena: &crate::VirArena,
    body_id: BodyId,
) -> HashMap<BindingId, BTreeSet<FieldKey>> {
    let mut result: HashMap<BindingId, BTreeSet<FieldKey>> = HashMap::new();
    let root_expr = arena[body_id].root_expr;
    collect_param_projs_expr(arena, body_id, root_expr, &mut result);
    result
}

fn collect_param_projs_expr(
    arena: &crate::VirArena,
    body_id: BodyId,
    expr_id: ExprId,
    result: &mut HashMap<BindingId, BTreeSet<FieldKey>>,
) {
    let kind = arena[expr_id].kind.clone();
    match kind {
        ExprKind::Field(f) => {
            // Check if receiver is a direct NameRef to a parameter.
            if let ExprKind::NameRef(nr) = &arena[f.receiver].kind {
                if let Some(DefTarget::Binding(bid)) = nr.resolved {
                    if arena[bid].kind == BindingKind::Parameter && arena[bid].body == body_id {
                        result.entry(bid).or_default().insert(f.field.clone());
                    }
                }
            }
            // Recurse into receiver for nested projections.
            collect_param_projs_expr(arena, body_id, f.receiver, result);
        }
        ExprKind::Closure(_) => {}
        ExprKind::Block(block) => {
            for &stmt_id in &block.stmts {
                visit_stmt_for_param_projs(arena, body_id, stmt_id, result);
            }
            if let Some(tail) = block.tail {
                collect_param_projs_expr(arena, body_id, tail, result);
            }
        }
        // Recurse into all child expressions (same pattern as other walks).
        _ => {
            visit_expr_children_for_param_projs(arena, body_id, expr_id, &kind, result);
        }
    }
}

fn visit_expr_children_for_param_projs(
    arena: &crate::VirArena,
    body_id: BodyId,
    _expr_id: ExprId,
    kind: &ExprKind,
    result: &mut HashMap<BindingId, BTreeSet<FieldKey>>,
) {
    match kind {
        ExprKind::If(if_) => {
            collect_param_projs_expr(arena, body_id, if_.condition, result);
            collect_param_projs_expr(arena, body_id, if_.then_branch, result);
            if let Some(else_br) = if_.else_branch {
                collect_param_projs_expr(arena, body_id, else_br, result);
            }
        }
        ExprKind::Match(m) => {
            collect_param_projs_expr(arena, body_id, m.scrutinee, result);
            for arm in &m.arms {
                collect_param_projs_expr(arena, body_id, arm.body, result);
            }
        }
        ExprKind::Binary(bin) => {
            collect_param_projs_expr(arena, body_id, bin.lhs, result);
            collect_param_projs_expr(arena, body_id, bin.rhs, result);
        }
        ExprKind::Prefix(pre) => {
            collect_param_projs_expr(arena, body_id, pre.operand, result);
        }
        ExprKind::Call(call) => {
            collect_param_projs_expr(arena, body_id, call.callee, result);
            for arg in &call.args {
                collect_param_projs_expr(arena, body_id, arg.expr, result);
            }
        }
        ExprKind::Where(w) => {
            collect_param_projs_expr(arena, body_id, w.callee, result);
            collect_param_projs_expr(arena, body_id, w.keywords, result);
        }
        ExprKind::Invoke(inv) => {
            collect_param_projs_expr(arena, body_id, inv.callee, result);
        }
        ExprKind::Index(idx) => {
            collect_param_projs_expr(arena, body_id, idx.receiver, result);
            collect_param_projs_expr(arena, body_id, idx.index, result);
        }
        ExprKind::Tuple(t) => {
            for &elem in &t.elements {
                collect_param_projs_expr(arena, body_id, elem, result);
            }
        }
        ExprKind::Record(r) => {
            for field in &r.fields {
                collect_param_projs_expr(arena, body_id, field.value, result);
            }
        }
        ExprKind::Fallible(f) => {
            collect_param_projs_expr(arena, body_id, f.expr, result);
        }
        ExprKind::TypeOp(top) => {
            collect_param_projs_expr(arena, body_id, top.expr, result);
        }
        _ => {} // Leaf nodes, closures already handled.
    }
}

fn visit_stmt_for_param_projs(
    arena: &crate::VirArena,
    body_id: BodyId,
    stmt_id: StmtId,
    result: &mut HashMap<BindingId, BTreeSet<FieldKey>>,
) {
    let kind = arena[stmt_id].kind.clone();
    match kind {
        StmtKind::Let(let_) => {
            collect_param_projs_expr(arena, body_id, let_.init, result);
        }
        StmtKind::Expr(expr_id) => {
            collect_param_projs_expr(arena, body_id, expr_id, result);
        }
        StmtKind::Return(ret) => {
            if let Some(val) = ret.value {
                collect_param_projs_expr(arena, body_id, val, result);
            }
        }
        StmtKind::Assign(assign) => {
            collect_param_projs_expr(arena, body_id, assign.value, result);
        }
        StmtKind::Item(_) => {}
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Product resolution
// ---------------------------------------------------------------------------

/// Resolve an expression to a product construction, tracing through bindings
/// and field projections.
///
/// Returns `(product_expr_id, field_keys)` or `None`.
fn resolve_to_product(
    arena: &crate::VirArena,
    expr: ExprId,
    visited: &mut HashSet<ExprId>,
) -> Option<(ExprId, Vec<FieldKey>)> {
    if !visited.insert(expr) {
        return None;
    }

    match &arena[expr].kind {
        // Direct products.
        ExprKind::Tuple(t) => {
            let keys = (0..t.elements.len())
                .map(|i| FieldKey::Index(i as u32))
                .collect();
            Some((expr, keys))
        }
        ExprKind::Record(r) => {
            let keys = r
                .fields
                .iter()
                .map(|f| FieldKey::Name(f.name.clone()))
                .collect();
            Some((expr, keys))
        }
        ExprKind::StructLiteral(sl) => {
            let keys = sl
                .fields
                .iter()
                .map(|f| FieldKey::Name(f.name.clone()))
                .collect();
            Some((expr, keys))
        }
        // Binding → follow init_expr.
        ExprKind::NameRef(nr) => {
            if let Some(DefTarget::Binding(bid)) = nr.resolved {
                if let Some(init) = arena[bid].init_expr {
                    return resolve_to_product(arena, init, visited);
                }
            }
            None
        }
        // Field projection → resolve base, then follow into sub-expression.
        ExprKind::Field(f) => {
            let (product_expr, _) = resolve_to_product(arena, f.receiver, visited)?;
            // Get the sub-expression at the projected field.
            let sub_expr = get_product_element(arena, product_expr, &f.field)?;
            resolve_to_product(arena, sub_expr, visited)
        }
        _ => None,
    }
}

/// Get the sub-expression at a specific field of a product.
fn get_product_element(
    arena: &crate::VirArena,
    product_expr: ExprId,
    field: &FieldKey,
) -> Option<ExprId> {
    match &arena[product_expr].kind {
        ExprKind::Tuple(t) => {
            if let FieldKey::Index(i) = field {
                t.elements.get(*i as usize).copied()
            } else {
                None
            }
        }
        ExprKind::Record(r) => {
            if let FieldKey::Name(name) = field {
                r.fields.iter().find(|f| f.name == *name).map(|f| f.value)
            } else {
                None
            }
        }
        ExprKind::StructLiteral(sl) => {
            if let FieldKey::Name(name) = field {
                sl.fields.iter().find(|f| f.name == *name).map(|f| f.value)
            } else {
                None
            }
        }
        _ => None,
    }
}
