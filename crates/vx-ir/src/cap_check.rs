//! Capability checking for VIR.
//!
//! Walks VIR directly (no Rowan dependency).
//!
//! Three passes:
//! 1. Collect declared function capabilities → `HashMap<ItemId, CapSet>`.
//! 2. Check callback slots: closure args must satisfy callee param masks.
//! 3. Validate capability tags: reject unknown `#tag` names.
//!
//! ## Intentional divergences from the old checker
//!
//! - `fn_caps` keyed by `ItemId` (not name string) — fixes ambiguity for
//!   same-name functions in different scopes.
//! - Pass 1 collects caps from all function items recursively (not just
//!   top-level) — fixes missed local/nested functions with cap tags.

use std::collections::HashMap;

use vx_parser::SourceSpan;
use vx_types::{Capability, FnSig, Ty};

use crate::cap::{CapCheckResult, CapSet};
use crate::def::Definition;
use crate::diagnostic::IrDiagnostic;
use crate::type_collection::TypeCollection;

use crate::arena::VirArena;
use crate::expr::{ExprKind, UpdateField};
use crate::item::ItemKind;
use crate::scope::DefTarget;
use crate::stmt::StmtKind;
use crate::type_check::TypeCheckVirResult;
use crate::{ExprId, ItemId, SourceUnitId, StmtId};

/// Derive a CapSet from the capabilities on a function type.
fn caps_from_fn_ty_capabilities(caps: &[Capability]) -> CapSet {
    let mut result = CapSet::EMPTY;
    for cap in caps {
        if let Some(c) = CapSet::from_name(&cap.name) {
            result = result.union(c);
        }
    }
    result
}

/// Run capability checking on a VIR arena.
pub fn check_capabilities_vir(
    arena: &VirArena,
    types: &TypeCollection,
    tc_result: &TypeCheckVirResult,
) -> CapCheckResult {
    let mut checker = CapChecker {
        arena,
        types,
        member_defs: &tc_result.member_defs,
        fn_caps: HashMap::new(),
        errors: Vec::new(),
    };

    let su_ids: Vec<SourceUnitId> = (0..arena.source_units.len())
        .map(SourceUnitId::from_index)
        .collect();

    // Pass 1: collect declared function capabilities.
    for &su_id in &su_ids {
        let body = &arena[arena[su_id].top_level_body];
        let root = &arena[body.root_expr];
        if let ExprKind::Block(block) = &root.kind {
            for &stmt_id in &block.stmts {
                if let StmtKind::Item(item_id) = arena[stmt_id].kind {
                    checker.collect_item_caps(item_id);
                }
            }
        }
    }

    // Pass 2: check callback slots.
    for &su_id in &su_ids {
        let body_id = arena[su_id].top_level_body;
        let root_expr = arena[body_id].root_expr;
        checker.check_expr_for_callbacks(root_expr);
    }

    // Pass 3: validate capability tags.
    for &su_id in &su_ids {
        let body = &arena[arena[su_id].top_level_body];
        let root = &arena[body.root_expr];
        if let ExprKind::Block(block) = &root.kind {
            for &stmt_id in &block.stmts {
                if let StmtKind::Item(item_id) = arena[stmt_id].kind {
                    checker.validate_item_caps(item_id);
                }
            }
        }
    }

    CapCheckResult {
        errors: checker.errors,
    }
}

struct CapChecker<'a> {
    arena: &'a VirArena,
    types: &'a TypeCollection,
    member_defs: &'a HashMap<SourceSpan, Definition>,
    fn_caps: HashMap<ItemId, CapSet>,
    errors: Vec<IrDiagnostic>,
}

// =========================================================================
// Pass 1: collect_fn_caps
// =========================================================================

impl CapChecker<'_> {
    fn collect_item_caps(&mut self, item_id: ItemId) {
        let kind = self.arena[item_id].kind.clone();
        match kind {
            ItemKind::Function(f) => {
                let body = &self.arena[f.body];
                let mut caps = CapSet::EMPTY;
                for tag in &body.capabilities {
                    if let Some(c) = CapSet::from_name(&tag.name) {
                        caps = caps.union(c);
                    }
                }
                self.fn_caps.insert(item_id, caps);
                // Recurse into body for nested fn declarations.
                self.collect_nested_item_caps(body.root_expr);
            }
            ItemKind::Namespace(ns) => {
                for &child in &ns.items {
                    self.collect_item_caps(child);
                }
            }
            _ => {}
        }
    }

    /// Walk an expression tree looking for ItemDecl stmts to collect caps from.
    fn collect_nested_item_caps(&mut self, expr_id: ExprId) {
        let kind = self.arena[expr_id].kind.clone();
        match kind {
            ExprKind::Block(block) => {
                for &stmt_id in &block.stmts {
                    self.collect_nested_item_caps_stmt(stmt_id);
                }
                if let Some(tail) = block.tail {
                    self.collect_nested_item_caps(tail);
                }
            }
            ExprKind::Closure(c) => {
                let root = self.arena[c.body].root_expr;
                self.collect_nested_item_caps(root);
            }
            ExprKind::If(if_) => {
                self.collect_nested_item_caps(if_.condition);
                self.collect_nested_item_caps(if_.then_branch);
                if let Some(else_br) = if_.else_branch {
                    self.collect_nested_item_caps(else_br);
                }
            }
            ExprKind::Match(m) => {
                for arm in &m.arms {
                    self.collect_nested_item_caps(arm.body);
                }
            }
            _ => {}
        }
    }

    fn collect_nested_item_caps_stmt(&mut self, stmt_id: StmtId) {
        let kind = self.arena[stmt_id].kind.clone();
        match kind {
            StmtKind::Item(item_id) => self.collect_item_caps(item_id),
            StmtKind::Let(let_) => self.collect_nested_item_caps(let_.init),
            StmtKind::Expr(expr_id) => self.collect_nested_item_caps(expr_id),
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.collect_nested_item_caps(val);
                }
            }
            StmtKind::Assign(assign) => self.collect_nested_item_caps(assign.value),
            StmtKind::CompoundUpdate(cu) => self.collect_nested_update_fields(&cu.updates),
            StmtKind::FieldAssign(fa) => self.collect_nested_item_caps(fa.value),
            StmtKind::Error => {}
        }
    }

    fn collect_nested_update_fields(&mut self, updates: &[UpdateField]) {
        for field in updates {
            match field {
                UpdateField::Set { value, .. } => self.collect_nested_item_caps(*value),
                UpdateField::Nested { updates, .. } => self.collect_nested_update_fields(updates),
            }
        }
    }
}

// =========================================================================
// Pass 2: check_callbacks
// =========================================================================

impl CapChecker<'_> {
    fn check_expr_for_callbacks(&mut self, expr_id: ExprId) {
        let kind = self.arena[expr_id].kind.clone();
        match kind {
            ExprKind::Call(call) => {
                self.check_call_callbacks(expr_id, &call.args);
                // Recurse into callee and args.
                self.check_expr_for_callbacks(call.callee);
                for arg in &call.args {
                    self.check_expr_for_callbacks(arg.expr);
                }
            }
            ExprKind::Where(w) => {
                // Where handles its callee (including inner Call). We only
                // recurse into keywords here — not w.callee.
                self.check_where_callbacks(expr_id, &w);
                self.check_expr_for_callbacks(w.keywords);
            }
            ExprKind::Block(block) => {
                self.check_stmts_for_callbacks(&block.stmts);
                if let Some(tail) = block.tail {
                    self.check_expr_for_callbacks(tail);
                }
            }
            ExprKind::Closure(c) => {
                let root = self.arena[c.body].root_expr;
                self.check_expr_for_callbacks(root);
            }
            ExprKind::If(if_) => {
                self.check_expr_for_callbacks(if_.condition);
                self.check_expr_for_callbacks(if_.then_branch);
                if let Some(else_br) = if_.else_branch {
                    self.check_expr_for_callbacks(else_br);
                }
            }
            ExprKind::Match(m) => {
                for arm in &m.arms {
                    self.check_expr_for_callbacks(arm.body);
                }
            }
            _ => {
                // Recurse into known child expressions.
                for child in self.child_exprs(expr_id) {
                    self.check_expr_for_callbacks(child);
                }
            }
        }
    }

    fn check_stmts_for_callbacks(&mut self, stmts: &[StmtId]) {
        for &stmt_id in stmts {
            self.check_stmt_for_callbacks(stmt_id);
        }
    }

    fn check_stmt_for_callbacks(&mut self, stmt_id: StmtId) {
        let kind = self.arena[stmt_id].kind.clone();
        match kind {
            StmtKind::Let(let_) => self.check_expr_for_callbacks(let_.init),
            StmtKind::Expr(expr_id) => self.check_expr_for_callbacks(expr_id),
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.check_expr_for_callbacks(val);
                }
            }
            StmtKind::Assign(assign) => self.check_expr_for_callbacks(assign.value),
            StmtKind::CompoundUpdate(cu) => self.check_update_fields_for_callbacks(&cu.updates),
            StmtKind::FieldAssign(fa) => self.check_expr_for_callbacks(fa.value),
            StmtKind::Item(item_id) => self.check_item_for_callbacks(item_id),
            StmtKind::Error => {}
        }
    }

    fn check_update_fields_for_callbacks(&mut self, updates: &[UpdateField]) {
        for field in updates {
            match field {
                UpdateField::Set { value, .. } => self.check_expr_for_callbacks(*value),
                UpdateField::Nested { updates, .. } => {
                    self.check_update_fields_for_callbacks(updates)
                }
            }
        }
    }

    fn check_item_for_callbacks(&mut self, item_id: ItemId) {
        let kind = self.arena[item_id].kind.clone();
        match kind {
            ItemKind::Function(f) => {
                let root = self.arena[f.body].root_expr;
                self.check_expr_for_callbacks(root);
            }
            ItemKind::Namespace(ns) => {
                for &child in &ns.items {
                    self.check_item_for_callbacks(child);
                }
            }
            _ => {}
        }
    }

    /// Check positional args of a Call against callee sig params.
    fn check_call_callbacks(&mut self, _call_expr_id: ExprId, args: &[crate::expr::Arg]) {
        // We need the callee from the call expression.
        let kind = self.arena[_call_expr_id].kind.clone();
        let ExprKind::Call(call) = kind else { return };

        let Some(sig) = self.sig_for_callee(call.callee) else {
            return;
        };

        for (arg, param) in args.iter().zip(sig.positional_params.iter()) {
            if let Ty::Function(fn_ty) = self.types.env.get_ty(param.ty) {
                let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                self.check_closure_mask(arg.expr, mask, param.name_span);
            }
        }
    }

    /// Check a Where expression's positional + keyword args.
    fn check_where_callbacks(&mut self, _where_expr_id: ExprId, w: &crate::expr::WhereExpr) {
        let callee_kind = self.arena[w.callee].kind.clone();

        let (real_callee_id, positional_args) = match callee_kind {
            ExprKind::Call(inner_call) => {
                let real = inner_call.callee;
                let args = inner_call.args.clone();
                // Recurse into inner call's callee and args.
                self.check_expr_for_callbacks(real);
                for arg in &args {
                    self.check_expr_for_callbacks(arg.expr);
                }
                (real, Some(args))
            }
            _ => {
                // Non-Call callee: recurse into it (outer Where arm suppresses this).
                self.check_expr_for_callbacks(w.callee);
                (w.callee, None)
            }
        };

        let Some(sig) = self.sig_for_callee(real_callee_id) else {
            return;
        };

        // Check positional args if present.
        if let Some(args) = &positional_args {
            for (arg, param) in args.iter().zip(sig.positional_params.iter()) {
                if let Ty::Function(fn_ty) = self.types.env.get_ty(param.ty) {
                    let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                    self.check_closure_mask(arg.expr, mask, param.name_span);
                }
            }
        }

        // Check keyword args from the Record.
        let kw_kind = self.arena[w.keywords].kind.clone();
        let ExprKind::Record(record) = kw_kind else {
            return;
        };

        for field in &record.fields {
            // Try keyword params first.
            if let Some(kw_param) = sig.keyword_params.iter().find(|p| p.name == field.name) {
                if let Ty::Function(fn_ty) = self.types.env.get_ty(kw_param.ty) {
                    let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                    self.check_closure_mask(field.value, mask, kw_param.name_span);
                }
                continue;
            }
            // Fall back to positional params by name.
            if let Some(pos_param) = sig.positional_params.iter().find(|p| p.name == field.name)
                && let Ty::Function(fn_ty) = self.types.env.get_ty(pos_param.ty)
            {
                let mask = caps_from_fn_ty_capabilities(&fn_ty.capabilities);
                self.check_closure_mask(field.value, mask, pos_param.name_span);
            }
        }
    }

    /// Check that an expression's inferred caps satisfy a mask.
    fn check_closure_mask(&mut self, expr_id: ExprId, mask: CapSet, slot: SourceSpan) {
        let inferred = self.infer_expr_caps(expr_id);
        if !inferred.satisfies(mask).is_empty() {
            self.errors.push(IrDiagnostic::CapabilityViolation {
                closure: self.arena[expr_id].span,
                slot,
                allowed: mask,
                found: inferred,
            });
        }
    }
}

// =========================================================================
// Pass 3: validate_cap_tags
// =========================================================================

impl CapChecker<'_> {
    fn validate_item_caps(&mut self, item_id: ItemId) {
        let kind = self.arena[item_id].kind.clone();
        match kind {
            ItemKind::Function(f) => {
                let body = &self.arena[f.body];
                for tag in &body.capabilities {
                    if CapSet::from_name(&tag.name).is_none() {
                        self.errors.push(IrDiagnostic::UnknownCapability {
                            name: tag.name_span,
                        });
                    }
                }
                // Recurse for nested fn declarations.
                self.validate_nested_item_caps(body.root_expr);
            }
            ItemKind::Namespace(ns) => {
                for &child in &ns.items {
                    self.validate_item_caps(child);
                }
            }
            _ => {}
        }
    }

    fn validate_nested_item_caps(&mut self, expr_id: ExprId) {
        let kind = self.arena[expr_id].kind.clone();
        match kind {
            ExprKind::Block(block) => {
                for &stmt_id in &block.stmts {
                    self.validate_nested_item_caps_stmt(stmt_id);
                }
                if let Some(tail) = block.tail {
                    self.validate_nested_item_caps(tail);
                }
            }
            ExprKind::Closure(c) => {
                let root = self.arena[c.body].root_expr;
                self.validate_nested_item_caps(root);
            }
            ExprKind::If(if_) => {
                self.validate_nested_item_caps(if_.condition);
                self.validate_nested_item_caps(if_.then_branch);
                if let Some(else_br) = if_.else_branch {
                    self.validate_nested_item_caps(else_br);
                }
            }
            ExprKind::Match(m) => {
                for arm in &m.arms {
                    self.validate_nested_item_caps(arm.body);
                }
            }
            _ => {}
        }
    }

    fn validate_nested_item_caps_stmt(&mut self, stmt_id: StmtId) {
        let kind = self.arena[stmt_id].kind.clone();
        match kind {
            StmtKind::Item(item_id) => self.validate_item_caps(item_id),
            StmtKind::Let(let_) => self.validate_nested_item_caps(let_.init),
            StmtKind::Expr(expr_id) => self.validate_nested_item_caps(expr_id),
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.validate_nested_item_caps(val);
                }
            }
            StmtKind::Assign(assign) => self.validate_nested_item_caps(assign.value),
            StmtKind::CompoundUpdate(cu) => {
                self.validate_nested_update_fields(&cu.updates);
            }
            StmtKind::FieldAssign(fa) => self.validate_nested_item_caps(fa.value),
            StmtKind::Error => {}
        }
    }

    fn validate_nested_update_fields(&mut self, updates: &[UpdateField]) {
        for field in updates {
            match field {
                UpdateField::Set { value, .. } => self.validate_nested_item_caps(*value),
                UpdateField::Nested { updates, .. } => self.validate_nested_update_fields(updates),
            }
        }
    }
}

// =========================================================================
// Capability inference
// =========================================================================

impl CapChecker<'_> {
    /// Infer the capability fingerprint of an expression (top-level entry).
    fn infer_expr_caps(&self, expr_id: ExprId) -> CapSet {
        let kind = self.arena[expr_id].kind.clone();
        match kind {
            ExprKind::Closure(c) => self.infer_closure_caps(c.body),
            ExprKind::NameRef(nr) => self.infer_name_ref_caps(&nr),
            _ => CapSet::EMPTY,
        }
    }

    fn infer_name_ref_caps(&self, nr: &crate::expr::NameRefExpr) -> CapSet {
        match nr.resolved {
            Some(DefTarget::Item(item_id)) => {
                // Check local fn_caps first.
                if let Some(&caps) = self.fn_caps.get(&item_id) {
                    return caps;
                }
                // Then check sig from TypeCollection.
                if let Some(sig) = self.sig_for_item(item_id) {
                    return caps_from_fn_ty_capabilities(&sig.capabilities);
                }
                CapSet::EMPTY
            }
            Some(DefTarget::Binding(_)) => CapSet::EMPTY,
            _ => CapSet::EMPTY,
        }
    }

    /// Infer capabilities from a closure body.
    fn infer_closure_caps(&self, body_id: crate::BodyId) -> CapSet {
        let root = self.arena[body_id].root_expr;
        self.infer_body_caps(root)
    }

    /// Infer capabilities from an expression (body/block context).
    fn infer_body_caps(&self, expr_id: ExprId) -> CapSet {
        let kind = self.arena[expr_id].kind.clone();
        match kind {
            ExprKind::Block(block) => {
                let mut caps = CapSet::EMPTY;
                for &stmt_id in &block.stmts {
                    caps = caps.union(self.infer_stmt_caps(stmt_id));
                }
                if let Some(tail) = block.tail {
                    caps = caps.union(self.infer_body_caps(tail));
                }
                caps
            }
            ExprKind::Call(call) => self.infer_call_caps(&call),
            ExprKind::Where(w) => {
                let mut caps = CapSet::EMPTY;
                // Determine real callee (peel inner Call if present).
                let callee_kind = self.arena[w.callee].kind.clone();
                let real_callee_id = match callee_kind {
                    ExprKind::Call(inner) => {
                        // Recurse into inner call's args too.
                        caps = caps.union(self.infer_body_caps(w.callee));
                        inner.callee
                    }
                    _ => {
                        caps = caps.union(self.infer_body_caps(w.callee));
                        w.callee
                    }
                };
                if let Some(callee_caps) = self.caps_for_callee(real_callee_id) {
                    caps = caps.union(callee_caps);
                }
                caps = caps.union(self.infer_body_caps(w.keywords));
                caps
            }
            ExprKind::Invoke(inv) => {
                let mut caps = self.infer_body_caps(inv.callee);
                if let Some(callee_caps) = self.caps_for_callee(inv.callee) {
                    caps = caps.union(callee_caps);
                }
                caps
            }
            ExprKind::If(if_) => {
                let mut caps = self.infer_body_caps(if_.condition);
                caps = caps.union(self.infer_body_caps(if_.then_branch));
                if let Some(else_br) = if_.else_branch {
                    caps = caps.union(self.infer_body_caps(else_br));
                }
                caps
            }
            ExprKind::Match(m) => {
                let mut caps = CapSet::EMPTY;
                for arm in &m.arms {
                    caps = caps.union(self.infer_body_caps(arm.body));
                }
                caps
            }
            ExprKind::Closure(c) => self.infer_closure_caps(c.body),
            ExprKind::NameRef(nr) => self.infer_name_ref_caps(&nr),
            _ => {
                // Generic child walk.
                let mut caps = CapSet::EMPTY;
                for child in self.child_exprs(expr_id) {
                    caps = caps.union(self.infer_body_caps(child));
                }
                caps
            }
        }
    }

    fn infer_call_caps(&self, call: &crate::expr::CallExpr) -> CapSet {
        let mut caps = CapSet::EMPTY;

        // Callee caps.
        if let Some(callee_caps) = self.caps_for_callee(call.callee) {
            caps = caps.union(callee_caps);
        }
        // Also check fn_caps for NameRef callees.
        let callee_kind = self.arena[call.callee].kind.clone();
        if let ExprKind::NameRef(nr) = callee_kind
            && let Some(DefTarget::Item(item_id)) = nr.resolved
            && let Some(&fn_caps) = self.fn_caps.get(&item_id)
        {
            caps = caps.union(fn_caps);
        }

        // Arg caps (skip closures).
        for arg in &call.args {
            if self.is_closure_expr(arg.expr) {
                continue;
            }
            caps = caps.union(self.infer_body_caps(arg.expr));
        }

        caps
    }

    fn infer_stmt_caps(&self, stmt_id: StmtId) -> CapSet {
        let kind = self.arena[stmt_id].kind.clone();
        match kind {
            StmtKind::Let(let_) => self.infer_body_caps(let_.init),
            StmtKind::Expr(expr_id) => self.infer_body_caps(expr_id),
            StmtKind::Return(ret) => ret
                .value
                .map(|v| self.infer_body_caps(v))
                .unwrap_or(CapSet::EMPTY),
            StmtKind::Assign(assign) => self.infer_body_caps(assign.value),
            StmtKind::CompoundUpdate(cu) => self.infer_update_field_caps(&cu.updates),
            StmtKind::FieldAssign(fa) => self.infer_body_caps(fa.value),
            StmtKind::Item(_) => CapSet::EMPTY,
            StmtKind::Error => CapSet::EMPTY,
        }
    }

    fn infer_update_field_caps(&self, updates: &[UpdateField]) -> CapSet {
        let mut caps = CapSet::EMPTY;
        for field in updates {
            match field {
                UpdateField::Set { value, .. } => {
                    caps = caps.union(self.infer_body_caps(*value));
                }
                UpdateField::Nested { updates, .. } => {
                    caps = caps.union(self.infer_update_field_caps(updates));
                }
            }
        }
        caps
    }

    fn is_closure_expr(&self, expr_id: ExprId) -> bool {
        matches!(self.arena[expr_id].kind, ExprKind::Closure(_))
    }
}

// =========================================================================
// Sig resolution helpers
// =========================================================================

impl CapChecker<'_> {
    fn sig_for_callee(&self, callee_id: ExprId) -> Option<FnSig> {
        let kind = &self.arena[callee_id].kind;
        match kind {
            ExprKind::NameRef(nr) => self.sig_for_name_ref(nr).cloned(),
            ExprKind::Field(field) => self.sig_for_field_call(field),
            _ => None,
        }
    }

    fn sig_for_name_ref(&self, nr: &crate::expr::NameRefExpr) -> Option<&FnSig> {
        match nr.resolved {
            Some(DefTarget::Item(item_id)) => self.sig_for_item(item_id),
            _ => None,
        }
    }

    fn sig_for_item(&self, item_id: ItemId) -> Option<&FnSig> {
        let kind = &self.arena[item_id].kind;
        match kind {
            ItemKind::Function(f) => self.types.def_to_fn_sig.get(&f.name_span),
            _ => None,
        }
    }

    fn sig_for_field_call(&self, field: &crate::expr::FieldExpr) -> Option<FnSig> {
        let def = self.member_defs.get(&field.field_span)?;
        let mut sig = self.types.fn_sig_for_def(def)?.clone();
        if sig
            .positional_params
            .first()
            .is_some_and(|p| p.name == "self")
        {
            sig.positional_params.remove(0);
        }
        Some(sig)
    }

    fn caps_for_callee(&self, callee_id: ExprId) -> Option<CapSet> {
        let sig = self.sig_for_callee(callee_id)?;
        Some(caps_from_fn_ty_capabilities(&sig.capabilities))
    }

    /// Enumerate direct child ExprIds for generic recursion.
    fn child_exprs(&self, expr_id: ExprId) -> Vec<ExprId> {
        let kind = &self.arena[expr_id].kind;
        match kind {
            ExprKind::Bomb => vec![],
            ExprKind::Binary(bin) => vec![bin.lhs, bin.rhs],
            ExprKind::Prefix(pre) => vec![pre.operand],
            ExprKind::Field(f) => vec![f.receiver],
            ExprKind::Index(idx) => vec![idx.receiver, idx.index],
            ExprKind::Array(arr) => {
                arr.elements
                    .iter()
                    .map(|e| match e {
                        crate::expr::ArrayElement::Expr(id)
                        | crate::expr::ArrayElement::Spread(id) => *id,
                    })
                    .collect()
            }
            ExprKind::Tuple(t) => t.elements.clone(),
            ExprKind::Record(r) => r.fields.iter().map(|f| f.value).collect(),
            ExprKind::Map(m) => {
                let mut children = Vec::new();
                for elem in &m.entries {
                    match elem {
                        crate::expr::MapElement::Entry(entry) => {
                            if let crate::expr::MapKey::Expr(k) = entry.key {
                                children.push(k);
                            }
                            children.push(entry.value);
                        }
                        crate::expr::MapElement::Spread(id) => children.push(*id),
                    }
                }
                children
            }
            ExprKind::StructLiteral(sl) => sl.fields.iter().map(|f| f.value).collect(),
            ExprKind::With(w) => {
                let mut children = vec![w.receiver];
                collect_update_field_exprs(&w.updates, &mut children);
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
                    crate::expr::StringPart::Expr(id) => Some(*id),
                    crate::expr::StringPart::Text(_) => None,
                })
                .collect(),
            ExprKind::PartialApp(pa) => {
                let mut children = vec![pa.callee];
                for arg in &pa.args {
                    if let crate::expr::PartialArg::Expr(expr) = arg {
                        children.push(*expr);
                    }
                }
                children
            }
            ExprKind::Literal(_)
            | ExprKind::PathLiteral(_)
            | ExprKind::NameRef(_)
            | ExprKind::Import(_)
            | ExprKind::Error => vec![],
            // These are handled explicitly in callers, but list for completeness.
            ExprKind::Block(_)
            | ExprKind::Call(_)
            | ExprKind::Where(_)
            | ExprKind::Invoke(_)
            | ExprKind::Closure(_)
            | ExprKind::If(_)
            | ExprKind::Match(_) => vec![],
        }
    }
}

fn collect_update_field_exprs(updates: &[UpdateField], out: &mut Vec<ExprId>) {
    for field in updates {
        match field {
            UpdateField::Set { value, .. } => out.push(*value),
            UpdateField::Nested { updates, .. } => collect_update_field_exprs(updates, out),
        }
    }
}
