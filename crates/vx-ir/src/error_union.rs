//! Inferred error-union inference for the `?` operator (the failure model).
//!
//! `expr?` types as `Result[inner, E]`, where `E` is the union of error types
//! that can reach that `?` through the call graph. The type-check pass leaves a
//! placeholder `E = String`; this post-pass replaces it with the real inferred
//! union and re-types any `match` that observes a `?` (so `Err(e)` binds the
//! true `E`), then checks those matches for exhaustiveness.
//!
//! ## Why a separate pass
//!
//! Computing `E` is a whole-call-graph fixpoint (`fail e` reaches a `?` through
//! any number of intervening calls). The single-sweep type checker cannot do it
//! in source order, so — exactly like [`cap_check`](crate::cap_check) — this
//! runs after `type_check_vir`, reading the types it wrote and overwriting the
//! `Fallible` (and dependent) nodes in place.
//!
//! ## Four phases
//!
//! - **A.** Build the call graph: for every function (incl. nested), the
//!   directly-`fail`ed error types and the functions it calls, *not* counting
//!   anything under a nested `?` (a `?` catches everything beneath it).
//! - **B.** Fixpoint: `raised[f] = direct[f] ∪ ⋃ raised[callee]` until stable.
//! - **C.** Re-type each `?`: union the direct fails + callees' raised sets over
//!   the `?`'s *inner* subtree, [`mint_union`](TypeCollection::mint_union) it,
//!   and overwrite the `Fallible` node's `Result[inner, E]`.
//! - **D.** Re-type `match` dependents (scrutinee is a `?`, or a `NameRef` to a
//!   `let x = <?>` binding), then flag non-exhaustive top-level `Ok`/`Err`.
//!
//! ## v1 boundaries
//!
//! - Re-assignment of `E` covers `match expr? (…)` and `let r = expr?; match r
//!   (…)` only. Deeper dataflow (a `?` result stored in a field, returned and
//!   re-`?`d, etc.) is out of scope — a snapshot, per the failure model.
//! - Callees that are not `DefTarget::Item(Function)` (variant constructors,
//!   closures bound to names, unresolved method/field calls) contribute no
//!   error alternatives. Method calls are best-effort: resolved-to-item ones are
//!   counted, the rest raise nothing.
//! - Exhaustiveness is top-level `Ok`/`Err` only; a bare `Err(e)` binding covers
//!   all of `Err`. Nested-union exhaustiveness *inside* `Err` is deferred.

use std::collections::{BTreeSet, HashMap};

use vx_types::{Ty, TypeDef, TypeId};

use crate::arena::VirArena;
use crate::diagnostic::IrDiagnostic;
use crate::expr::{
    ArrayElement, ExprKind, MapElement, MapKey, PartialArg, StringPart, UpdateField,
};
use crate::item::ItemKind;
use crate::pattern::PatternKind;
use crate::scope::DefTarget;
use crate::stmt::StmtKind;
use crate::type_check::TypeCheckVirResult;
use crate::type_collection::TypeCollection;
use crate::{ExprId, ItemId, PatternId, SourceUnitId, StmtId};

/// What a function can raise, before the fixpoint: the error types it `fail`s
/// directly and the functions it calls (both ignoring anything under a nested
/// `?`).
#[derive(Default)]
struct RaiseInfo {
    direct: Vec<TypeId>,
    callees: Vec<ItemId>,
}

/// Run inferred-error-union inference over a fully type-checked arena.
///
/// Overwrites `Fallible` expression types (and dependent `let`/`match` binding
/// and pattern types) in place; returns any [`IrDiagnostic::NonExhaustiveMatch`]
/// it finds.
pub fn infer_error_unions(
    arena: &mut VirArena,
    types: &mut TypeCollection,
    tc_result: &TypeCheckVirResult,
) -> Vec<IrDiagnostic> {
    let mut pass = ErrorUnionPass {
        arena,
        types,
        member_defs: &tc_result.member_defs,
        errors: Vec::new(),
    };

    // A. Build per-function raise info over every function item (incl. nested).
    let mut raise_info: HashMap<ItemId, RaiseInfo> = HashMap::new();
    let su_ids: Vec<SourceUnitId> = (0..pass.arena.source_units.len())
        .map(SourceUnitId::from_index)
        .collect();
    for &su_id in &su_ids {
        let body = &pass.arena[pass.arena[su_id].top_level_body];
        let root = &pass.arena[body.root_expr];
        if let ExprKind::Block(block) = &root.kind {
            let stmts = block.stmts.clone();
            for stmt_id in stmts {
                if let StmtKind::Item(item_id) = pass.arena[stmt_id].kind {
                    pass.collect_item_raises(item_id, &mut raise_info);
                }
            }
        }
    }

    // B. Fixpoint: propagate callees' raised sets until no set grows.
    let raised = fixpoint(&raise_info);

    // C. Re-type every `?` over the whole module (user units + stdlib + pkgs).
    for &su_id in &su_ids {
        let root = pass.arena[pass.arena[su_id].top_level_body].root_expr;
        pass.retype_fallibles(root, &raised);
    }

    // D. Re-type match dependents and check exhaustiveness.
    for &su_id in &su_ids {
        let root = pass.arena[pass.arena[su_id].top_level_body].root_expr;
        pass.retype_dependents(root);
    }

    pass.errors
}

/// Intern `Result[ok, err]` from the stdlib `Result` enum, or `None` if it is
/// not in scope (it always is — declared in `core.vx`). Shared with
/// `type_check::special` so the placeholder and the real union use one path.
pub(crate) fn intern_result_in(
    env: &mut vx_types::TypeEnv,
    ok: TypeId,
    err: TypeId,
) -> Option<TypeId> {
    let def = env
        .lookup_def("Result")
        .iter()
        .copied()
        .find(|&id| matches!(env.get_def(id), TypeDef::Enum(_)))?;
    Some(env.intern_enum(def, vec![ok, err]))
}

/// Intern `Failure[E]` from the stdlib `Failure` struct, or `None` if it is not in
/// scope (it always is — declared in `core.vx`). The caught `Err` payload of a `?`
/// is `Failure[E]` (the failure model's envelope around the domain error `E`), so
/// matching `Err(Failure { err: x })` binds `x: E`. Shared with
/// `type_check::special` so the placeholder and the real union wrap the same way.
pub(crate) fn intern_failure_in(env: &mut vx_types::TypeEnv, err: TypeId) -> Option<TypeId> {
    let def = env
        .lookup_def("Failure")
        .iter()
        .copied()
        .find(|&id| matches!(env.get_def(id), TypeDef::Struct(_)))?;
    Some(env.intern_struct(def, vec![err]))
}

struct ErrorUnionPass<'a> {
    arena: &'a mut VirArena,
    types: &'a mut TypeCollection,
    member_defs: &'a HashMap<vx_parser::SourceSpan, crate::def::Definition>,
    errors: Vec<IrDiagnostic>,
}

// ===========================================================================
// Phase A: collect direct fails + callees per function.
// ===========================================================================

impl ErrorUnionPass<'_> {
    fn collect_item_raises(&mut self, item_id: ItemId, out: &mut HashMap<ItemId, RaiseInfo>) {
        let kind = self.arena[item_id].kind.clone();
        match kind {
            ItemKind::Function(f) => {
                let root = self.arena[f.body].root_expr;
                let mut info = RaiseInfo::default();
                self.walk_collect(root, &mut info);
                out.insert(item_id, info);
                // Recurse into nested fn declarations in the body.
                self.collect_nested_raises(root, out);
            }
            ItemKind::Namespace(ns) => {
                for child in ns.items.clone() {
                    self.collect_item_raises(child, out);
                }
            }
            _ => {}
        }
    }

    /// Walk an expression tree looking for nested fn declarations to collect.
    fn collect_nested_raises(&mut self, expr_id: ExprId, out: &mut HashMap<ItemId, RaiseInfo>) {
        for stmt_id in self.item_decl_stmts(expr_id) {
            if let StmtKind::Item(item_id) = self.arena[stmt_id].kind {
                self.collect_item_raises(item_id, out);
            }
        }
    }

    /// Every statement reachable from `expr_id` that could be an item decl.
    /// (We only care about `StmtKind::Item`, but block bodies nest arbitrarily,
    /// so collect all stmts and let the caller filter.)
    fn item_decl_stmts(&self, expr_id: ExprId) -> Vec<StmtId> {
        let mut stmts = Vec::new();
        self.gather_stmts(expr_id, &mut stmts);
        stmts
    }

    fn gather_stmts(&self, expr_id: ExprId, out: &mut Vec<StmtId>) {
        match &self.arena[expr_id].kind {
            ExprKind::Block(block) => {
                for &stmt_id in &block.stmts {
                    out.push(stmt_id);
                    self.gather_stmts_in_stmt(stmt_id, out);
                }
                if let Some(tail) = block.tail {
                    self.gather_stmts(tail, out);
                }
            }
            ExprKind::Closure(c) => {
                let root = self.arena[c.body].root_expr;
                self.gather_stmts(root, out);
            }
            ExprKind::If(if_) => {
                self.gather_stmts(if_.condition, out);
                self.gather_stmts(if_.then_branch, out);
                if let Some(else_br) = if_.else_branch {
                    self.gather_stmts(else_br, out);
                }
            }
            ExprKind::Match(m) => {
                self.gather_stmts(m.scrutinee, out);
                for arm in &m.arms {
                    self.gather_stmts(arm.body, out);
                }
            }
            _ => {
                for child in self.child_exprs(expr_id) {
                    self.gather_stmts(child, out);
                }
            }
        }
    }

    fn gather_stmts_in_stmt(&self, stmt_id: StmtId, out: &mut Vec<StmtId>) {
        match &self.arena[stmt_id].kind {
            StmtKind::Item(_) | StmtKind::Error => {}
            StmtKind::Let(let_) => self.gather_stmts(let_.init, out),
            StmtKind::Expr(expr_id) => self.gather_stmts(*expr_id, out),
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.gather_stmts(val, out);
                }
            }
            StmtKind::Assign(assign) => self.gather_stmts(assign.value, out),
            StmtKind::FieldAssign(fa) => self.gather_stmts(fa.value, out),
            StmtKind::CompoundUpdate(cu) => {
                for child in update_field_exprs(&cu.updates) {
                    self.gather_stmts(child, out);
                }
            }
        }
    }
}

// ===========================================================================
// The shared walker: visit children, but never descend into a nested `?`.
// ===========================================================================

impl ErrorUnionPass<'_> {
    /// Walk `expr_id`, recording every direct `fail`'s error type and every
    /// resolved function callee into `info` — never descending into a nested
    /// `Fallible` (it catches everything beneath it).
    fn walk_collect(&self, expr_id: ExprId, info: &mut RaiseInfo) {
        let kind = self.arena[expr_id].kind.clone();
        match kind {
            // The one special rule: a nested `?` catches everything below it.
            ExprKind::Fallible(_) => {}
            ExprKind::Fail(f) => {
                // A bare `fail` (no message) still makes the fn fallible: it
                // contributes Unit so the union is non-empty. A `fail e`
                // contributes `e`'s type, unless poison/None.
                match f.message {
                    Some(msg) => {
                        if let Some(ty) = self.arena[msg].ty
                            && !self.types.env.get_ty(ty).is_poison()
                        {
                            info.direct.push(ty);
                        }
                        self.walk_collect(msg, info);
                    }
                    None => info.direct.push(self.unit_ty()),
                }
            }
            ExprKind::Call(call) => {
                if let Some(callee) = self.resolve_fn_callee(call.callee) {
                    info.callees.push(callee);
                }
                self.walk_collect(call.callee, info);
                for arg in &call.args {
                    self.walk_collect(arg.expr, info);
                }
            }
            ExprKind::Where(w) => {
                // The real callee may be wrapped in an inner Call.
                let real = self.where_real_callee(w.callee);
                if let Some(callee) = self.resolve_fn_callee(real) {
                    info.callees.push(callee);
                }
                self.walk_collect(w.callee, info);
                self.walk_collect(w.keywords, info);
            }
            ExprKind::Invoke(inv) => {
                if let Some(callee) = self.resolve_fn_callee(inv.callee) {
                    info.callees.push(callee);
                }
                self.walk_collect(inv.callee, info);
            }
            ExprKind::PartialApp(pa) => {
                if let Some(callee) = self.resolve_fn_callee(pa.callee) {
                    info.callees.push(callee);
                }
                self.walk_collect(pa.callee, info);
                for arg in &pa.args {
                    if let PartialArg::Expr(e) = arg {
                        self.walk_collect(*e, info);
                    }
                }
            }
            ExprKind::Block(block) => {
                for &stmt_id in &block.stmts {
                    self.walk_collect_stmt(stmt_id, info);
                }
                if let Some(tail) = block.tail {
                    self.walk_collect(tail, info);
                }
            }
            ExprKind::Closure(c) => {
                // A closure body's failures are caught when the closure is
                // *called*, not where it is written — so do not attribute them
                // to the enclosing fn. (v1: closures contribute nothing here.)
                let _ = c;
            }
            ExprKind::If(if_) => {
                self.walk_collect(if_.condition, info);
                self.walk_collect(if_.then_branch, info);
                if let Some(else_br) = if_.else_branch {
                    self.walk_collect(else_br, info);
                }
            }
            ExprKind::Match(m) => {
                self.walk_collect(m.scrutinee, info);
                for arm in &m.arms {
                    self.walk_collect(arm.body, info);
                }
            }
            _ => {
                for child in self.child_exprs(expr_id) {
                    self.walk_collect(child, info);
                }
            }
        }
    }

    fn walk_collect_stmt(&self, stmt_id: StmtId, info: &mut RaiseInfo) {
        match &self.arena[stmt_id].kind {
            StmtKind::Item(_) | StmtKind::Error => {}
            StmtKind::Let(let_) => self.walk_collect(let_.init, info),
            StmtKind::Expr(expr_id) => self.walk_collect(*expr_id, info),
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.walk_collect(val, info);
                }
            }
            StmtKind::Assign(assign) => self.walk_collect(assign.value, info),
            StmtKind::FieldAssign(fa) => self.walk_collect(fa.value, info),
            StmtKind::CompoundUpdate(cu) => {
                for child in update_field_exprs(&cu.updates) {
                    self.walk_collect(child, info);
                }
            }
        }
    }
}

// ===========================================================================
// Phase B: fixpoint.
// ===========================================================================

fn fixpoint(raise_info: &HashMap<ItemId, RaiseInfo>) -> HashMap<ItemId, BTreeSet<TypeId>> {
    // Seed each fn's raised set with its direct fails.
    let mut raised: HashMap<ItemId, BTreeSet<TypeId>> = raise_info
        .iter()
        .map(|(&id, info)| (id, info.direct.iter().copied().collect::<BTreeSet<_>>()))
        .collect();

    // Iterate `raised[f] = direct[f] ∪ ⋃ raised[callee]` to a fixpoint.
    // Monotonic growth over a finite type set → terminates; handles cycles.
    loop {
        let mut changed = false;
        for (&f, info) in raise_info {
            let mut additions: BTreeSet<TypeId> = BTreeSet::new();
            for &callee in &info.callees {
                if let Some(callee_set) = raised.get(&callee) {
                    for &t in callee_set {
                        additions.insert(t);
                    }
                }
            }
            let set = raised.entry(f).or_default();
            for t in additions {
                if set.insert(t) {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    raised
}

// ===========================================================================
// Phase C: re-type every `?`.
// ===========================================================================

impl ErrorUnionPass<'_> {
    /// Walk the whole tree (recursing into nested `?`s too — each one is
    /// re-typed against its *own* inner subtree) and overwrite each `Fallible`'s
    /// `Result[inner, E]` with the inferred union.
    fn retype_fallibles(&mut self, expr_id: ExprId, raised: &HashMap<ItemId, BTreeSet<TypeId>>) {
        let kind = self.arena[expr_id].kind.clone();
        if let ExprKind::Fallible(f) = &kind {
            let inner = f.expr;
            // Compute the error set over the inner subtree, then re-type the
            // inner first so nested `?`s are already correct.
            let mut error_set: BTreeSet<TypeId> = BTreeSet::new();
            self.collect_error_set(inner, raised, &mut error_set);
            self.retype_fallibles(inner, raised);
            self.overwrite_fallible_ty(expr_id, inner, error_set);
            return;
        }
        // Generic recursion into all children (incl. through stmts).
        match kind {
            ExprKind::Block(block) => {
                for stmt_id in block.stmts {
                    self.retype_fallibles_stmt(stmt_id, raised);
                }
                if let Some(tail) = block.tail {
                    self.retype_fallibles(tail, raised);
                }
            }
            ExprKind::Closure(c) => {
                let root = self.arena[c.body].root_expr;
                self.retype_fallibles(root, raised);
            }
            ExprKind::If(if_) => {
                self.retype_fallibles(if_.condition, raised);
                self.retype_fallibles(if_.then_branch, raised);
                if let Some(else_br) = if_.else_branch {
                    self.retype_fallibles(else_br, raised);
                }
            }
            ExprKind::Match(m) => {
                self.retype_fallibles(m.scrutinee, raised);
                for arm in m.arms {
                    self.retype_fallibles(arm.body, raised);
                }
            }
            _ => {
                for child in self.child_exprs(expr_id) {
                    self.retype_fallibles(child, raised);
                }
            }
        }
    }

    fn retype_fallibles_stmt(
        &mut self,
        stmt_id: StmtId,
        raised: &HashMap<ItemId, BTreeSet<TypeId>>,
    ) {
        let kind = self.arena[stmt_id].kind.clone();
        match kind {
            StmtKind::Item(item_id) => self.retype_fallibles_item(item_id, raised),
            StmtKind::Let(let_) => self.retype_fallibles(let_.init, raised),
            StmtKind::Expr(expr_id) => self.retype_fallibles(expr_id, raised),
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.retype_fallibles(val, raised);
                }
            }
            StmtKind::Assign(assign) => self.retype_fallibles(assign.value, raised),
            StmtKind::FieldAssign(fa) => self.retype_fallibles(fa.value, raised),
            StmtKind::CompoundUpdate(cu) => {
                for child in update_field_exprs(&cu.updates) {
                    self.retype_fallibles(child, raised);
                }
            }
            StmtKind::Error => {}
        }
    }

    fn retype_fallibles_item(
        &mut self,
        item_id: ItemId,
        raised: &HashMap<ItemId, BTreeSet<TypeId>>,
    ) {
        let kind = self.arena[item_id].kind.clone();
        match kind {
            ItemKind::Function(f) => {
                let root = self.arena[f.body].root_expr;
                self.retype_fallibles(root, raised);
            }
            ItemKind::Namespace(ns) => {
                for child in ns.items {
                    self.retype_fallibles_item(child, raised);
                }
            }
            _ => {}
        }
    }

    /// Compute the set of error alternatives reachable at a `?` over `inner`:
    /// the direct `fail` types + each called fn's raised set, gathered over the
    /// inner subtree, never descending into a *further* nested `?`. Callee
    /// raised sets that are unions are flattened into their alternatives.
    fn collect_error_set(
        &self,
        inner: ExprId,
        raised: &HashMap<ItemId, BTreeSet<TypeId>>,
        out: &mut BTreeSet<TypeId>,
    ) {
        let mut info = RaiseInfo::default();
        self.walk_collect(inner, &mut info);
        for t in info.direct {
            for alt in self.types.canonicalize_alternatives(vec![t]) {
                out.insert(alt);
            }
        }
        for callee in info.callees {
            if let Some(set) = raised.get(&callee) {
                let alts: Vec<TypeId> = set.iter().copied().collect();
                for alt in self.types.canonicalize_alternatives(alts) {
                    out.insert(alt);
                }
            }
        }
    }

    /// Overwrite a `Fallible` node's type with `Result[inner, Failure[E]]`, where
    /// `E` is the inferred error union — the caught `Err` payload is the `Failure`
    /// envelope around `E`, so the failure's context survives the catch (matching
    /// `Err(Failure { err: x })` binds `x: E`). Rebuilds through the function-type
    /// shape when the inner is itself a function.
    fn overwrite_fallible_ty(
        &mut self,
        fallible_id: ExprId,
        inner: ExprId,
        error_set: BTreeSet<TypeId>,
    ) {
        let Some(inner_ty) = self.arena[inner].ty else {
            return;
        };
        if self.types.env.get_ty(inner_ty).is_poison() {
            return;
        }
        let alts: Vec<TypeId> = error_set.into_iter().collect();
        let domain_err = self.types.mint_union(alts);
        // Wrap the domain error union `E` in `Failure[E]` — the caught `Err` payload.
        let Some(err_ty) = intern_failure_in(&mut self.types.env, domain_err) else {
            return;
        };
        let inner_clone = self.types.env.get_ty(inner_ty).clone();
        let new_ty = match inner_clone {
            Ty::Function(fn_ty) => {
                let Some(res_ret) = intern_result_in(&mut self.types.env, fn_ty.ret, err_ty) else {
                    return;
                };
                self.types
                    .env
                    .intern_function(fn_ty.params, res_ret, fn_ty.capabilities)
            }
            _ => match intern_result_in(&mut self.types.env, inner_ty, err_ty) {
                Some(t) => t,
                None => return,
            },
        };
        self.arena[fallible_id].ty = Some(new_ty);
    }
}

// ===========================================================================
// Phase D: re-type match dependents + exhaustiveness.
// ===========================================================================

impl ErrorUnionPass<'_> {
    fn retype_dependents(&mut self, expr_id: ExprId) {
        let kind = self.arena[expr_id].kind.clone();
        if let ExprKind::Match(m) = &kind {
            self.handle_match(m.scrutinee, &m.arms.clone());
        }
        // Recurse into all children (incl. through stmts/items).
        match kind {
            ExprKind::Block(block) => {
                for stmt_id in block.stmts {
                    self.retype_dependents_stmt(stmt_id);
                }
                if let Some(tail) = block.tail {
                    self.retype_dependents(tail);
                }
            }
            ExprKind::Closure(c) => {
                let root = self.arena[c.body].root_expr;
                self.retype_dependents(root);
            }
            ExprKind::If(if_) => {
                self.retype_dependents(if_.condition);
                self.retype_dependents(if_.then_branch);
                if let Some(else_br) = if_.else_branch {
                    self.retype_dependents(else_br);
                }
            }
            ExprKind::Match(m) => {
                self.retype_dependents(m.scrutinee);
                for arm in m.arms {
                    self.retype_dependents(arm.body);
                }
            }
            _ => {
                for child in self.child_exprs(expr_id) {
                    self.retype_dependents(child);
                }
            }
        }
    }

    fn retype_dependents_stmt(&mut self, stmt_id: StmtId) {
        let kind = self.arena[stmt_id].kind.clone();
        match kind {
            StmtKind::Item(item_id) => self.retype_dependents_item(item_id),
            StmtKind::Let(let_) => {
                // Re-type a `let x = <Fallible>` binding to the new Result type.
                let new_ty = self.arena[let_.init].ty;
                if matches!(self.arena[let_.init].kind, ExprKind::Fallible(_))
                    && let Some(ty) = new_ty
                    && !self.types.env.get_ty(ty).is_poison()
                {
                    crate::type_check::patterns::retype_pattern(
                        self.arena,
                        self.types,
                        let_.pattern,
                        ty,
                    );
                }
                self.retype_dependents(let_.init);
            }
            StmtKind::Expr(expr_id) => self.retype_dependents(expr_id),
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.retype_dependents(val);
                }
            }
            StmtKind::Assign(assign) => self.retype_dependents(assign.value),
            StmtKind::FieldAssign(fa) => self.retype_dependents(fa.value),
            StmtKind::CompoundUpdate(cu) => {
                for child in update_field_exprs(&cu.updates) {
                    self.retype_dependents(child);
                }
            }
            StmtKind::Error => {}
        }
    }

    fn retype_dependents_item(&mut self, item_id: ItemId) {
        let kind = self.arena[item_id].kind.clone();
        match kind {
            ItemKind::Function(f) => {
                let root = self.arena[f.body].root_expr;
                self.retype_dependents(root);
            }
            ItemKind::Namespace(ns) => {
                for child in ns.items {
                    self.retype_dependents_item(child);
                }
            }
            _ => {}
        }
    }

    /// Handle one `match`: if its scrutinee observes a `?` (directly, or via a
    /// `let x = <?>` binding), re-type the arms' patterns against the new
    /// `Result` type so `Err(e)` binds the real `E`, then check exhaustiveness.
    fn handle_match(&mut self, scrutinee: ExprId, arms: &[crate::expr::MatchArm]) {
        let Some(scrut_ty) = self.fallible_scrutinee_ty(scrutinee) else {
            return;
        };
        if self.types.env.get_ty(scrut_ty).is_poison() {
            return;
        }
        // Re-type each arm pattern against the (possibly updated) Result type.
        for arm in arms {
            crate::type_check::patterns::retype_pattern(
                self.arena,
                self.types,
                arm.pattern,
                scrut_ty,
            );
        }
        self.check_exhaustiveness(scrutinee, scrut_ty, arms);
    }

    /// The scrutinee's type *iff* it observes a `?` we re-typed: a direct
    /// `Fallible`, or a `NameRef` to a `let`-binding initialized by a `Fallible`.
    /// Returns the current (post-phase-C) Result type to match against.
    fn fallible_scrutinee_ty(&self, scrutinee: ExprId) -> Option<TypeId> {
        match &self.arena[scrutinee].kind {
            ExprKind::Fallible(_) => self.arena[scrutinee].ty,
            ExprKind::NameRef(nr) => {
                let DefTarget::Binding(binding_id) = nr.resolved? else {
                    return None;
                };
                let init = self.arena[binding_id].init_expr?;
                if matches!(self.arena[init].kind, ExprKind::Fallible(_)) {
                    self.arena[init].ty
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Flag a non-exhaustive top-level `Ok`/`Err` match. An irrefutable arm
    /// (`_` or a bare binding) makes it exhaustive; otherwise every variant of
    /// the scrutinee enum must be covered by some `Variant` arm (flattening
    /// `Or`). v1: top-level only — a bare `Err(e)` covers all of `Err`.
    fn check_exhaustiveness(
        &mut self,
        scrutinee: ExprId,
        scrut_ty: TypeId,
        arms: &[crate::expr::MatchArm],
    ) {
        // Any irrefutable arm → exhaustive.
        for arm in arms {
            if self.is_irrefutable(arm.pattern) {
                return;
            }
        }
        // Required variants come from the scrutinee enum.
        let Ty::Enum(enum_ty) = self.types.env.get_ty(scrut_ty).clone() else {
            return;
        };
        let TypeDef::Enum(edef) = self.types.env.get_def(enum_ty.def).clone() else {
            return;
        };
        let required: Vec<String> = edef
            .variants
            .iter()
            .filter_map(|&vid| match self.types.env.get_def(vid) {
                TypeDef::Struct(sd) => Some(sd.name.clone()),
                _ => None,
            })
            .collect();
        // Matched variant names (flatten Or patterns).
        let mut matched: BTreeSet<String> = BTreeSet::new();
        for arm in arms {
            self.collect_matched_variants(arm.pattern, &mut matched);
        }
        let missing: Vec<String> = required
            .into_iter()
            .filter(|v| !matched.contains(v))
            .collect();
        if !missing.is_empty() {
            let match_span = self.arena[scrutinee].span;
            self.errors.push(IrDiagnostic::NonExhaustiveMatch {
                match_span,
                missing,
            });
        }
    }

    fn is_irrefutable(&self, pat_id: PatternId) -> bool {
        match &self.arena[pat_id].kind {
            PatternKind::Wildcard | PatternKind::Binding(_) => true,
            PatternKind::At(at) => self.is_irrefutable(at.inner),
            PatternKind::Or(or) => or.alternatives.iter().any(|&a| self.is_irrefutable(a)),
            _ => false,
        }
    }

    fn collect_matched_variants(&self, pat_id: PatternId, out: &mut BTreeSet<String>) {
        match &self.arena[pat_id].kind {
            PatternKind::Variant(vp) => {
                out.insert(vp.name.clone());
            }
            PatternKind::Or(or) => {
                for &alt in &or.alternatives {
                    self.collect_matched_variants(alt, out);
                }
            }
            PatternKind::At(at) => self.collect_matched_variants(at.inner, out),
            _ => {}
        }
    }
}

// ===========================================================================
// Callee resolution + child enumeration (shared helpers).
// ===========================================================================

impl ErrorUnionPass<'_> {
    /// The interned `Ty::Unit` — the error type a bare `fail` (no message)
    /// contributes so the enclosing fn stays fallible. Always present before
    /// this pass runs (no-return-type fns intern it); scanned because the
    /// walker only has `&self`. Falls back to poison if somehow absent.
    fn unit_ty(&self) -> TypeId {
        for i in 0..self.types.env.types_len() {
            let id = TypeId(i as u32);
            match self.types.env.get_ty(id) {
                Ty::Unit => return id,
                _ => continue,
            }
        }
        for i in 0..self.types.env.types_len() {
            let id = TypeId(i as u32);
            if self.types.env.get_ty(id).is_poison() {
                return id;
            }
        }
        TypeId(0)
    }

    /// Resolve a call callee expression to the `ItemId` of a user function, or
    /// `None` (variant constructors, closures, methods we can't resolve). Method
    /// (`Field`) callees are best-effort via `member_defs` → fn-sig name span.
    fn resolve_fn_callee(&self, callee_id: ExprId) -> Option<ItemId> {
        match &self.arena[callee_id].kind {
            ExprKind::NameRef(nr) => match nr.resolved? {
                DefTarget::Item(item_id) => {
                    matches!(self.arena[item_id].kind, ItemKind::Function(_)).then_some(item_id)
                }
                _ => None,
            },
            ExprKind::Field(field) => {
                // Best-effort: map the method's def site back to the fn item.
                let def = self.member_defs.get(&field.field_span)?;
                self.item_for_def_site(def)
            }
            _ => None,
        }
    }

    /// Find the function `ItemId` whose name span equals a member definition's
    /// site (used to attribute method-call failures, best-effort).
    fn item_for_def_site(&self, def: &crate::def::Definition) -> Option<ItemId> {
        let site = def.site();
        for (i, item) in self.arena.items.iter().enumerate() {
            if let ItemKind::Function(f) = &item.kind
                && f.name_span == site
            {
                return Some(ItemId::from_index(i));
            }
        }
        None
    }

    /// Peel an inner `Call` off a `where` callee to find the real callee expr.
    fn where_real_callee(&self, callee_id: ExprId) -> ExprId {
        match &self.arena[callee_id].kind {
            ExprKind::Call(inner) => inner.callee,
            _ => callee_id,
        }
    }

    /// Direct child expressions for generic recursion (mirrors cap_check's
    /// `child_exprs`). Block/Closure/If/Match/Call/Where/Invoke/PartialApp are
    /// handled explicitly by callers, so they return their children here only
    /// where a generic walk reaches them.
    fn child_exprs(&self, expr_id: ExprId) -> Vec<ExprId> {
        match &self.arena[expr_id].kind {
            ExprKind::Binary(bin) => vec![bin.lhs, bin.rhs],
            ExprKind::Prefix(pre) => vec![pre.operand],
            ExprKind::Field(f) => vec![f.receiver],
            ExprKind::Index(idx) => vec![idx.receiver, idx.index],
            ExprKind::Array(arr) => arr
                .elements
                .iter()
                .map(|e| match e {
                    ArrayElement::Expr(id) | ArrayElement::Spread(id) => *id,
                })
                .collect(),
            ExprKind::Tuple(t) => t.elements.clone(),
            ExprKind::Record(r) => r.fields.iter().map(|f| f.value).collect(),
            ExprKind::Map(m) => {
                let mut children = Vec::new();
                for elem in &m.entries {
                    match elem {
                        MapElement::Entry(entry) => {
                            if let MapKey::Expr(k) = &entry.key {
                                children.push(*k);
                            }
                            children.push(entry.value);
                        }
                        MapElement::Spread(id) => children.push(*id),
                    }
                }
                children
            }
            ExprKind::StructLiteral(sl) => sl.fields.iter().map(|f| f.value).collect(),
            ExprKind::With(w) => {
                let mut children = vec![w.receiver];
                children.extend(update_field_exprs(&w.updates));
                children
            }
            ExprKind::Fallible(f) => vec![f.expr],
            ExprKind::Fail(f) => f.message.into_iter().collect(),
            ExprKind::TypeOp(top) => vec![top.expr],
            ExprKind::Range(r) => {
                let mut children = Vec::new();
                if let Some(s) = r.start {
                    children.push(s);
                }
                if let Some(e) = r.end {
                    children.push(e);
                }
                children
            }
            ExprKind::InterpolatedString(is) => is
                .parts
                .iter()
                .filter_map(|p| match p {
                    StringPart::Expr(id) => Some(*id),
                    StringPart::Text(_) => None,
                })
                .collect(),
            ExprKind::PathLiteral(pl) => pl
                .parts
                .iter()
                .filter_map(|p| match p {
                    StringPart::Expr(id) => Some(*id),
                    StringPart::Text(_) => None,
                })
                .collect(),
            ExprKind::CliLiteral(cli) => cli
                .args
                .iter()
                .flat_map(|arg| &arg.pieces)
                .filter_map(|p| match p {
                    StringPart::Expr(id) => Some(*id),
                    StringPart::Text(_) => None,
                })
                .collect(),
            ExprKind::PartialApp(pa) => {
                let mut children = vec![pa.callee];
                for arg in &pa.args {
                    if let PartialArg::Expr(expr) = arg {
                        children.push(*expr);
                    }
                }
                children
            }
            ExprKind::Call(call) => {
                let mut children = vec![call.callee];
                children.extend(call.args.iter().map(|a| a.expr));
                children
            }
            ExprKind::Where(w) => vec![w.callee, w.keywords],
            ExprKind::Invoke(inv) => vec![inv.callee],
            ExprKind::Literal(_)
            | ExprKind::NameRef(_)
            | ExprKind::Import(_)
            | ExprKind::Bomb
            | ExprKind::Error
            | ExprKind::Block(_)
            | ExprKind::Closure(_)
            | ExprKind::If(_)
            | ExprKind::Match(_) => vec![],
        }
    }
}

fn update_field_exprs(updates: &[UpdateField]) -> Vec<ExprId> {
    let mut out = Vec::new();
    collect_update_field_exprs(updates, &mut out);
    out
}

fn collect_update_field_exprs(updates: &[UpdateField], out: &mut Vec<ExprId>) {
    for field in updates {
        match field {
            UpdateField::Set { value, .. } => out.push(*value),
            UpdateField::Nested { updates, .. } => collect_update_field_exprs(updates, out),
        }
    }
}
