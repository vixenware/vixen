//! Name resolution pass for VIR.
//!
//! Walks all VIR nodes and populates `resolved` fields on:
//! - [`NameRefExpr`](crate::expr::NameRefExpr) — any name reference
//! - [`AssignStmt`](crate::stmt::AssignStmt) — assignment target
//! - [`CompoundUpdateStmt`](crate::stmt::CompoundUpdateStmt) — update target
//! - [`FieldAssignStmt`](crate::stmt::FieldAssignStmt) — field assign base
//! - [`StructLiteralExpr`](crate::expr::StructLiteralExpr) — struct name
//! - [`VariantPattern`](crate::pattern::VariantPattern) — variant name
//!
//! Uses [`scope::lookup_at`] for all lookups. The walk computes
//! `reference_order` contextually (not stored on nodes).

use crate::arena::VirArena;
use crate::diagnostic::IrDiagnostic;
use crate::expr::{
    ArrayElement, ExprKind, MapElement, MapKey, PartialArg, StringPart, UpdateField,
};
use crate::item::ItemKind;
use crate::pattern::PatternKind;
use crate::scope::{self, DefTarget, ScopeEntryTarget};
use crate::stmt::StmtKind;
use crate::{BodyId, ExprId, ItemId, PatternId, ScopeId, SourceUnitId, StmtId};
use vx_parser::SourceSpan;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Resolve all name references in a VIR arena.
///
/// Populates `resolved` fields on NameRefExpr, AssignStmt, CompoundUpdateStmt,
/// FieldAssignStmt, StructLiteralExpr, and VariantPattern. Returns diagnostics
/// for unresolved names and invalid target kinds.
pub fn resolve_names(arena: &mut VirArena) -> Vec<IrDiagnostic> {
    // Reset all resolved fields to None before walking.
    reset_resolved(arena);

    let mut errors = Vec::new();

    // Collect source unit IDs first to avoid borrow issues.
    let su_ids: Vec<SourceUnitId> = (0..arena.source_units.len())
        .map(SourceUnitId::from_index)
        .collect();

    for su_id in su_ids {
        resolve_source_unit(arena, su_id, &mut errors);
    }

    errors
}

// ---------------------------------------------------------------------------
// Reset pass
// ---------------------------------------------------------------------------

fn reset_resolved(arena: &mut VirArena) {
    for expr in &mut arena.exprs {
        match &mut expr.kind {
            ExprKind::NameRef(nr) => nr.resolved = None,
            ExprKind::StructLiteral(sl) => sl.resolved = None,
            _ => {}
        }
    }
    for stmt in &mut arena.stmts {
        match &mut stmt.kind {
            StmtKind::Assign(a) => a.resolved = None,
            StmtKind::CompoundUpdate(cu) => cu.resolved = None,
            StmtKind::FieldAssign(fa) => fa.resolved = None,
            _ => {}
        }
    }
    for pat in &mut arena.patterns {
        if let PatternKind::Variant(vp) = &mut pat.kind {
            vp.resolved = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Walk functions
// ---------------------------------------------------------------------------

/// Entry point: resolve a single source unit by walking its top-level body.
/// The synthetic block already contains all top-level items as StmtKind::Item,
/// so there is no separate su.items loop (that would double-resolve).
fn resolve_source_unit(arena: &mut VirArena, su_id: SourceUnitId, errors: &mut Vec<IrDiagnostic>) {
    let body_id = arena[su_id].top_level_body;
    resolve_body(arena, body_id, errors);
}

/// Resolve a function/closure/top-level body.
fn resolve_body(arena: &mut VirArena, body_id: BodyId, errors: &mut Vec<IrDiagnostic>) {
    let root_expr = arena[body_id].root_expr;
    // All params are visible in the body → u32::MAX.
    resolve_expr(arena, root_expr, u32::MAX, errors);
}

/// Resolve an expression. `ref_order` is the reference_order for lookups
/// in the expression's scope.
fn resolve_expr(
    arena: &mut VirArena,
    expr_id: ExprId,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    // Read the kind by cloning the minimal data needed to avoid holding
    // an immutable borrow on arena while we mutate.
    let kind = arena[expr_id].kind.clone();
    let scope = arena[expr_id].scope;

    match kind {
        ExprKind::NameRef(nr) => {
            resolve_name_ref(arena, expr_id, &nr.name, scope, ref_order, errors);
        }
        ExprKind::Block(block) => {
            let stmt_count = block.stmts.len() as u32;
            for (i, &stmt_id) in block.stmts.iter().enumerate() {
                resolve_stmt(arena, stmt_id, i as u32, errors);
            }
            if let Some(tail) = block.tail {
                resolve_expr(arena, tail, stmt_count, errors);
            }
        }
        ExprKind::Closure(closure) => {
            resolve_body(arena, closure.body, errors);
        }
        ExprKind::StructLiteral(sl) => {
            resolve_struct_literal(arena, expr_id, &sl.name, scope, ref_order, errors);
            for field in &sl.fields {
                resolve_expr(arena, field.value, ref_order, errors);
            }
        }
        ExprKind::Match(m) => {
            resolve_expr(arena, m.scrutinee, ref_order, errors);
            for arm in &m.arms {
                resolve_pattern(arena, arm.pattern, ref_order, errors);
                // All arm pattern bindings are visible in the arm body.
                resolve_expr(arena, arm.body, u32::MAX, errors);
            }
        }
        // All other expression kinds: recurse into children with same ref_order.
        ExprKind::Literal(_) | ExprKind::Import(_) | ExprKind::Error => {}
        ExprKind::PathLiteral(pl) => {
            for part in &pl.parts {
                if let StringPart::Expr(id) = part {
                    resolve_expr(arena, *id, ref_order, errors);
                }
            }
        }
        ExprKind::Tuple(t) => {
            for &elem in &t.elements {
                resolve_expr(arena, elem, ref_order, errors);
            }
        }
        ExprKind::Array(arr) => {
            for elem in &arr.elements {
                match elem {
                    ArrayElement::Expr(id) | ArrayElement::Spread(id) => {
                        resolve_expr(arena, *id, ref_order, errors);
                    }
                }
            }
        }
        ExprKind::Record(rec) => {
            for field in &rec.fields {
                resolve_expr(arena, field.value, ref_order, errors);
            }
        }
        ExprKind::Map(map) => {
            for elem in &map.entries {
                match elem {
                    MapElement::Entry(entry) => {
                        if let MapKey::Expr(k) = &entry.key {
                            resolve_expr(arena, *k, ref_order, errors);
                        }
                        resolve_expr(arena, entry.value, ref_order, errors);
                    }
                    MapElement::Spread(id) => {
                        resolve_expr(arena, *id, ref_order, errors);
                    }
                }
            }
        }
        ExprKind::Call(call) => {
            resolve_expr(arena, call.callee, ref_order, errors);
            for arg in &call.args {
                resolve_expr(arena, arg.expr, ref_order, errors);
            }
        }
        ExprKind::Where(w) => {
            resolve_expr(arena, w.callee, ref_order, errors);
            resolve_expr(arena, w.keywords, ref_order, errors);
        }
        ExprKind::Invoke(inv) => {
            resolve_expr(arena, inv.callee, ref_order, errors);
        }
        ExprKind::PartialApp(pa) => {
            resolve_expr(arena, pa.callee, ref_order, errors);
            for arg in &pa.args {
                if let PartialArg::Expr(id) = arg {
                    resolve_expr(arena, *id, ref_order, errors);
                }
            }
        }
        ExprKind::Index(idx) => {
            resolve_expr(arena, idx.receiver, ref_order, errors);
            resolve_expr(arena, idx.index, ref_order, errors);
        }
        ExprKind::Field(f) => {
            resolve_expr(arena, f.receiver, ref_order, errors);
        }
        ExprKind::If(if_) => {
            resolve_expr(arena, if_.condition, ref_order, errors);
            resolve_expr(arena, if_.then_branch, ref_order, errors);
            if let Some(else_) = if_.else_branch {
                resolve_expr(arena, else_, ref_order, errors);
            }
        }
        ExprKind::Binary(bin) => {
            resolve_expr(arena, bin.lhs, ref_order, errors);
            resolve_expr(arena, bin.rhs, ref_order, errors);
        }
        ExprKind::Prefix(pre) => {
            resolve_expr(arena, pre.operand, ref_order, errors);
        }
        ExprKind::Range(range) => {
            if let Some(s) = range.start {
                resolve_expr(arena, s, ref_order, errors);
            }
            if let Some(e) = range.end {
                resolve_expr(arena, e, ref_order, errors);
            }
        }
        ExprKind::TypeOp(top) => {
            resolve_expr(arena, top.expr, ref_order, errors);
        }
        ExprKind::With(w) => {
            resolve_expr(arena, w.receiver, ref_order, errors);
            resolve_update_fields(arena, &w.updates, ref_order, errors);
        }
        ExprKind::Fallible(f) => {
            resolve_expr(arena, f.expr, ref_order, errors);
        }
        ExprKind::InterpolatedString(is) => {
            for part in &is.parts {
                if let StringPart::Expr(id) = part {
                    resolve_expr(arena, *id, ref_order, errors);
                }
            }
        }
        ExprKind::Fail(f) => {
            if let Some(msg) = f.message {
                resolve_expr(arena, msg, ref_order, errors);
            }
        }
    }
}

fn resolve_update_fields(
    arena: &mut VirArena,
    fields: &[UpdateField],
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    for field in fields {
        match field {
            UpdateField::Set { value, .. } => {
                resolve_expr(arena, *value, ref_order, errors);
            }
            UpdateField::Nested { updates, .. } => {
                resolve_update_fields(arena, updates, ref_order, errors);
            }
        }
    }
}

/// Resolve a statement. `stmt_order` is this statement's position in its
/// parent block — used as the reference_order for lookups within the stmt.
fn resolve_stmt(
    arena: &mut VirArena,
    stmt_id: StmtId,
    stmt_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    let kind = arena[stmt_id].kind.clone();

    match kind {
        StmtKind::Let(let_) => {
            // Init expression uses stmt_order as reference_order so the
            // binding being defined is invisible in its own initializer.
            resolve_expr(arena, let_.init, stmt_order, errors);
            // Pattern doesn't need resolution — bindings already allocated.
        }
        StmtKind::Assign(assign) => {
            resolve_assign_target(
                arena,
                stmt_id,
                &assign.name,
                assign.name_span,
                stmt_order,
                errors,
            );
            resolve_expr(arena, assign.value, stmt_order, errors);
        }
        StmtKind::CompoundUpdate(cu) => {
            resolve_compound_update_target(
                arena,
                stmt_id,
                &cu.name,
                cu.name_span,
                stmt_order,
                errors,
            );
            resolve_update_fields(arena, &cu.updates, stmt_order, errors);
        }
        StmtKind::FieldAssign(fa) => {
            resolve_field_assign_target(arena, stmt_id, &fa.base, fa.base_span, stmt_order, errors);
            resolve_expr(arena, fa.value, stmt_order, errors);
        }
        StmtKind::Return(ret) => {
            if let Some(val) = ret.value {
                resolve_expr(arena, val, stmt_order, errors);
            }
        }
        StmtKind::Item(item_id) => {
            resolve_item(arena, item_id, errors);
        }
        StmtKind::Expr(expr_id) => {
            resolve_expr(arena, expr_id, stmt_order, errors);
        }
        StmtKind::Error => {}
    }
}

/// Resolve an item. Recurses into function bodies, namespace children,
/// and struct/enum field default-value expressions.
fn resolve_item(arena: &mut VirArena, item_id: ItemId, errors: &mut Vec<IrDiagnostic>) {
    let kind = arena[item_id].kind.clone();

    match kind {
        ItemKind::Function(f) => {
            resolve_body(arena, f.body, errors);
        }
        ItemKind::Namespace(ns) => {
            for &child_id in &ns.items {
                resolve_item(arena, child_id, errors);
            }
        }
        ItemKind::Struct(s) => {
            let ref_order = item_decl_order(arena, item_id);
            for field in &s.fields {
                if let Some(default) = field.default_value {
                    resolve_expr(arena, default, ref_order, errors);
                }
            }
        }
        ItemKind::Enum(e) => {
            let ref_order = item_decl_order(arena, item_id);
            for variant in &e.variants {
                for field in &variant.fields {
                    if let Some(default) = field.default_value {
                        resolve_expr(arena, default, ref_order, errors);
                    }
                }
            }
        }
        ItemKind::Newtype(_) | ItemKind::TypeAlias(_) | ItemKind::Error => {}
    }
}

/// Resolve a pattern. Only VariantPattern needs name resolution.
fn resolve_pattern(
    arena: &mut VirArena,
    pat_id: PatternId,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    let kind = arena[pat_id].kind.clone();
    let scope = arena[pat_id].scope;

    match kind {
        PatternKind::Variant(vp) => {
            resolve_variant_pattern(arena, pat_id, &vp.name, scope, ref_order, errors);
            if let Some(fields) = &vp.fields {
                for field in fields {
                    resolve_pattern(arena, field.pattern, ref_order, errors);
                }
            }
        }
        PatternKind::Tuple(t) => {
            for &elem in &t.elements {
                resolve_pattern(arena, elem, ref_order, errors);
            }
        }
        PatternKind::Struct(s) => {
            for field in &s.fields {
                resolve_pattern(arena, field.pattern, ref_order, errors);
            }
        }
        PatternKind::Array(a) => {
            for &elem in &a.elements {
                resolve_pattern(arena, elem, ref_order, errors);
            }
        }
        PatternKind::Or(or) => {
            for &alt in &or.alternatives {
                resolve_pattern(arena, alt, ref_order, errors);
            }
        }
        PatternKind::At(at) => {
            resolve_pattern(arena, at.inner, ref_order, errors);
        }
        PatternKind::Binding(_)
        | PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::Unit
        | PatternKind::ArrayRest(_)
        | PatternKind::Error => {}
    }
}

// ---------------------------------------------------------------------------
// Resolution helpers
// ---------------------------------------------------------------------------

/// Should this name be skipped (no resolution, no error)?
fn should_skip(name: &str) -> bool {
    name == "_" || name.starts_with('$')
}

/// Resolve a NameRefExpr. No target-kind constraint.
fn resolve_name_ref(
    arena: &mut VirArena,
    expr_id: ExprId,
    name: &str,
    scope: ScopeId,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    if should_skip(name) {
        return;
    }

    let result = scope::lookup_at(arena, name, scope, ref_order);
    match result {
        Some(target) => {
            if let ExprKind::NameRef(ref mut nr) = arena[expr_id].kind {
                nr.resolved = Some(target);
            }
        }
        None => {
            // Use the NameRef's name token span (no leading trivia) — the
            // expression span itself can include preceding whitespace.
            let span = match &arena[expr_id].kind {
                ExprKind::NameRef(nr) => nr.name_span,
                _ => arena[expr_id].span,
            };
            errors.push(IrDiagnostic::Unresolved { name: span });
        }
    }
}

/// Resolve assignment target. Must be a Binding.
fn resolve_assign_target(
    arena: &mut VirArena,
    stmt_id: StmtId,
    name: &str,
    name_span: SourceSpan,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    if should_skip(name) {
        return;
    }

    let scope = arena[stmt_id].scope;
    let result = scope::lookup_at(arena, name, scope, ref_order);

    match result {
        Some(DefTarget::Binding(binding_id)) => {
            if let StmtKind::Assign(ref mut a) = arena[stmt_id].kind {
                a.resolved = Some(binding_id);
            }
        }
        Some(found) => {
            errors.push(IrDiagnostic::InvalidTarget {
                name: name_span,
                found,
            });
        }
        None => {
            errors.push(IrDiagnostic::Unresolved { name: name_span });
        }
    }
}

/// Resolve compound update target. Must be a Binding.
fn resolve_compound_update_target(
    arena: &mut VirArena,
    stmt_id: StmtId,
    name: &str,
    name_span: SourceSpan,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    if should_skip(name) {
        return;
    }

    let scope = arena[stmt_id].scope;
    let result = scope::lookup_at(arena, name, scope, ref_order);

    match result {
        Some(DefTarget::Binding(binding_id)) => {
            if let StmtKind::CompoundUpdate(ref mut cu) = arena[stmt_id].kind {
                cu.resolved = Some(binding_id);
            }
        }
        Some(found) => {
            errors.push(IrDiagnostic::InvalidTarget {
                name: name_span,
                found,
            });
        }
        None => {
            errors.push(IrDiagnostic::Unresolved { name: name_span });
        }
    }
}

/// Resolve field-assign base. Must be a Binding.
fn resolve_field_assign_target(
    arena: &mut VirArena,
    stmt_id: StmtId,
    name: &str,
    name_span: SourceSpan,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    if should_skip(name) {
        return;
    }

    let scope = arena[stmt_id].scope;
    let result = scope::lookup_at(arena, name, scope, ref_order);

    match result {
        Some(DefTarget::Binding(binding_id)) => {
            if let StmtKind::FieldAssign(ref mut fa) = arena[stmt_id].kind {
                fa.resolved = Some(binding_id);
            }
        }
        Some(found) => {
            errors.push(IrDiagnostic::InvalidTarget {
                name: name_span,
                found,
            });
        }
        None => {
            errors.push(IrDiagnostic::Unresolved { name: name_span });
        }
    }
}

/// Resolve a struct literal name. Must resolve to an Item with ItemKind::Struct.
fn resolve_struct_literal(
    arena: &mut VirArena,
    expr_id: ExprId,
    name: &str,
    scope: ScopeId,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    if should_skip(name) {
        return;
    }

    let result = scope::lookup_at(arena, name, scope, ref_order);
    // Use the struct name token span rather than the full literal span
    // (which includes the `{ ... }` braces and preceding whitespace).
    let span = match &arena[expr_id].kind {
        ExprKind::StructLiteral(sl) => sl.name_span,
        _ => arena[expr_id].span,
    };

    match result {
        Some(DefTarget::Item(item_id)) => {
            if matches!(arena[item_id].kind, ItemKind::Struct(_)) {
                if let ExprKind::StructLiteral(ref mut sl) = arena[expr_id].kind {
                    sl.resolved = Some(item_id);
                }
            } else {
                errors.push(IrDiagnostic::InvalidTarget {
                    name: span,
                    found: DefTarget::Item(item_id),
                });
            }
        }
        Some(found) => {
            errors.push(IrDiagnostic::InvalidTarget { name: span, found });
        }
        None => {
            errors.push(IrDiagnostic::Unresolved { name: span });
        }
    }
}

/// Resolve a variant pattern name. Must resolve to DefTarget::Variant or
/// DefTarget::Item with ItemKind::Enum. When storing on VariantPattern.resolved
/// (which is Option<ItemId>), DefTarget::Variant { enum_item, .. } stores
/// enum_item — the variant_index is NOT preserved; the pattern's .name field
/// plus the parent enum ItemId is sufficient to identify the specific variant.
fn resolve_variant_pattern(
    arena: &mut VirArena,
    pat_id: PatternId,
    name: &str,
    scope: ScopeId,
    ref_order: u32,
    errors: &mut Vec<IrDiagnostic>,
) {
    if should_skip(name) {
        return;
    }

    let result = scope::lookup_at(arena, name, scope, ref_order);
    // Use the variant name token span rather than the full pattern span
    // (which can include surrounding parens, fields, leading trivia).
    let span = match &arena[pat_id].kind {
        PatternKind::Variant(vp) => vp.name_span,
        _ => arena[pat_id].span,
    };

    match result {
        Some(DefTarget::Variant {
            enum_item,
            variant_index: _,
        }) => {
            if let PatternKind::Variant(ref mut vp) = arena[pat_id].kind {
                vp.resolved = Some(enum_item);
            }
        }
        Some(DefTarget::Item(item_id)) => {
            if matches!(arena[item_id].kind, ItemKind::Enum(_)) {
                if let PatternKind::Variant(ref mut vp) = arena[pat_id].kind {
                    vp.resolved = Some(item_id);
                }
            } else {
                errors.push(IrDiagnostic::InvalidTarget {
                    name: span,
                    found: DefTarget::Item(item_id),
                });
            }
        }
        Some(found) => {
            errors.push(IrDiagnostic::InvalidTarget { name: span, found });
        }
        None => {
            errors.push(IrDiagnostic::Unresolved { name: span });
        }
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Find the decl_order of an item in its enclosing scope.
/// Used for struct/enum default-value resolution context.
fn item_decl_order(arena: &VirArena, item_id: ItemId) -> u32 {
    // Search all scopes for an entry pointing to this item.
    for scope in &arena.scopes {
        for entry in &scope.entries {
            if let ScopeEntryTarget::Item(id) = entry.target
                && id == item_id
            {
                return entry.decl_order;
            }
        }
    }
    // Fallback — shouldn't happen for well-formed VIR.
    u32::MAX
}
