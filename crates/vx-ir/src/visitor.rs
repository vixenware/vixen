//! Visitor infrastructure for walking the VIR tree.
//!
//! Override any `visit_*` method to intercept traversal at that node kind.
//! The default implementations call the corresponding `walk_*` free function,
//! which recurses into child nodes. To prevent recursion, override the method
//! and do NOT call the walk function.
//!
//! The arena is passed to every method so visitors can look up child nodes
//! without needing to store the arena reference themselves.

use crate::arena::VirArena;
use crate::expr::{
    ArrayElement, ExprKind, MapElement, MapKey, PartialArg, StringPart, UpdateField,
};
use crate::item::ItemKind;
use crate::pattern::PatternKind;
use crate::scope::ScopeEntryTarget;
use crate::stmt::StmtKind;
use crate::type_ref::TypeRefKind;
use crate::{
    BindingId, BodyId, ExprId, ItemId, PatternId, ScopeId, SourceUnitId, StmtId, TypeRefId,
};

/// A visitor for walking the VIR tree.
///
/// Override any method to intercept traversal. Defaults call the corresponding
/// `walk_*` function for recursive descent.
pub trait VirVisitor {
    /// Called for each expression.
    fn visit_expr(&mut self, arena: &VirArena, id: ExprId) {
        walk_expr(self, arena, id);
    }

    /// Called for each statement.
    fn visit_stmt(&mut self, arena: &VirArena, id: StmtId) {
        walk_stmt(self, arena, id);
    }

    /// Called for each pattern.
    fn visit_pattern(&mut self, arena: &VirArena, id: PatternId) {
        walk_pattern(self, arena, id);
    }

    /// Called for each body.
    fn visit_body(&mut self, arena: &VirArena, id: BodyId) {
        walk_body(self, arena, id);
    }

    /// Called for each binding. Default: no-op (leaf node).
    fn visit_binding(&mut self, _arena: &VirArena, _id: BindingId) {}

    /// Called for each item.
    fn visit_item(&mut self, arena: &VirArena, id: ItemId) {
        walk_item(self, arena, id);
    }

    /// Called for each type reference.
    fn visit_type_ref(&mut self, arena: &VirArena, id: TypeRefId) {
        walk_type_ref(self, arena, id);
    }

    /// Called for each scope.
    fn visit_scope(&mut self, arena: &VirArena, id: ScopeId) {
        walk_scope(self, arena, id);
    }

    /// Called for each source unit (per-file root).
    fn visit_source_unit(&mut self, arena: &VirArena, id: SourceUnitId) {
        walk_source_unit(self, arena, id);
    }
}

/// Walk the children of an expression.
pub fn walk_expr<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: ExprId) {
    match &arena[id].kind {
        ExprKind::Literal(_) | ExprKind::Import(_) | ExprKind::Bomb | ExprKind::Error => {}
        ExprKind::PathLiteral(pl) => {
            for part in &pl.parts {
                if let StringPart::Expr(id) = part {
                    v.visit_expr(arena, *id);
                }
            }
        }
        ExprKind::NameRef(_) => {}
        ExprKind::Block(block) => {
            v.visit_scope(arena, block.scope);
            for &stmt_id in &block.stmts {
                v.visit_stmt(arena, stmt_id);
            }
            if let Some(tail) = block.tail {
                v.visit_expr(arena, tail);
            }
        }
        ExprKind::Closure(closure) => {
            v.visit_body(arena, closure.body);
        }
        ExprKind::Tuple(tuple) => {
            for &elem in &tuple.elements {
                v.visit_expr(arena, elem);
            }
        }
        ExprKind::Array(arr) => {
            for elem in &arr.elements {
                match elem {
                    ArrayElement::Expr(id) | ArrayElement::Spread(id) => {
                        v.visit_expr(arena, *id);
                    }
                }
            }
        }
        ExprKind::Record(rec) => {
            for field in &rec.fields {
                v.visit_expr(arena, field.value);
            }
        }
        ExprKind::Map(map) => {
            for elem in &map.entries {
                match elem {
                    MapElement::Entry(entry) => {
                        if let MapKey::Expr(k) = &entry.key {
                            v.visit_expr(arena, *k);
                        }
                        v.visit_expr(arena, entry.value);
                    }
                    MapElement::Spread(id) => {
                        v.visit_expr(arena, *id);
                    }
                }
            }
        }
        ExprKind::StructLiteral(sl) => {
            for field in &sl.fields {
                v.visit_expr(arena, field.value);
            }
        }
        ExprKind::Call(call) => {
            v.visit_expr(arena, call.callee);
            for arg in &call.args {
                v.visit_expr(arena, arg.expr);
            }
            for &tref in &call.type_args {
                v.visit_type_ref(arena, tref);
            }
        }
        ExprKind::Where(w) => {
            v.visit_expr(arena, w.callee);
            v.visit_expr(arena, w.keywords);
        }
        ExprKind::Invoke(inv) => {
            v.visit_expr(arena, inv.callee);
        }
        ExprKind::PartialApp(pa) => {
            v.visit_expr(arena, pa.callee);
            for arg in &pa.args {
                if let PartialArg::Expr(id) = arg {
                    v.visit_expr(arena, *id);
                }
            }
        }
        ExprKind::Index(idx) => {
            v.visit_expr(arena, idx.receiver);
            v.visit_expr(arena, idx.index);
        }
        ExprKind::Field(f) => {
            v.visit_expr(arena, f.receiver);
        }
        ExprKind::If(if_) => {
            v.visit_expr(arena, if_.condition);
            v.visit_expr(arena, if_.then_branch);
            if let Some(else_) = if_.else_branch {
                v.visit_expr(arena, else_);
            }
        }
        ExprKind::Match(m) => {
            v.visit_expr(arena, m.scrutinee);
            for arm in &m.arms {
                v.visit_scope(arena, arm.scope);
                v.visit_pattern(arena, arm.pattern);
                v.visit_expr(arena, arm.body);
            }
        }
        ExprKind::Binary(bin) => {
            v.visit_expr(arena, bin.lhs);
            v.visit_expr(arena, bin.rhs);
        }
        ExprKind::Prefix(pre) => {
            v.visit_expr(arena, pre.operand);
        }
        ExprKind::Range(range) => {
            if let Some(s) = range.start {
                v.visit_expr(arena, s);
            }
            if let Some(e) = range.end {
                v.visit_expr(arena, e);
            }
        }
        ExprKind::TypeOp(top) => {
            v.visit_expr(arena, top.expr);
            v.visit_type_ref(arena, top.ty);
        }
        ExprKind::With(w) => {
            v.visit_expr(arena, w.receiver);
            walk_update_fields(v, arena, &w.updates);
        }
        ExprKind::Fallible(f) => {
            v.visit_expr(arena, f.expr);
        }
        ExprKind::InterpolatedString(is) => {
            for part in &is.parts {
                if let StringPart::Expr(id) = part {
                    v.visit_expr(arena, *id);
                }
            }
        }
        ExprKind::Fail(f) => {
            if let Some(msg) = f.message {
                v.visit_expr(arena, msg);
            }
        }
    }
}

/// Walk update fields recursively (shared by `with` expressions and compound updates).
fn walk_update_fields<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, fields: &[UpdateField]) {
    for field in fields {
        match field {
            UpdateField::Set { value, .. } => {
                v.visit_expr(arena, *value);
            }
            UpdateField::Nested { updates, .. } => {
                walk_update_fields(v, arena, updates);
            }
        }
    }
}

/// Walk the children of a statement.
pub fn walk_stmt<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: StmtId) {
    match &arena[id].kind {
        StmtKind::Let(let_) => {
            v.visit_pattern(arena, let_.pattern);
            if let Some(tref) = let_.ty {
                v.visit_type_ref(arena, tref);
            }
            v.visit_expr(arena, let_.init);
        }
        StmtKind::Assign(assign) => {
            v.visit_expr(arena, assign.value);
        }
        StmtKind::CompoundUpdate(cu) => {
            walk_update_fields(v, arena, &cu.updates);
        }
        StmtKind::FieldAssign(fa) => {
            v.visit_expr(arena, fa.value);
        }
        StmtKind::Return(ret) => {
            if let Some(val) = ret.value {
                v.visit_expr(arena, val);
            }
        }
        StmtKind::Item(item_id) => {
            v.visit_item(arena, *item_id);
        }
        StmtKind::Expr(expr_id) => {
            v.visit_expr(arena, *expr_id);
        }
        StmtKind::Error => {}
    }
}

/// Walk the children of a pattern.
pub fn walk_pattern<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: PatternId) {
    match &arena[id].kind {
        PatternKind::Binding(b) => {
            v.visit_binding(arena, b.binding);
        }
        PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::Unit
        | PatternKind::Error => {}
        PatternKind::Tuple(t) => {
            for &elem in &t.elements {
                v.visit_pattern(arena, elem);
            }
        }
        PatternKind::Struct(s) => {
            for field in &s.fields {
                v.visit_pattern(arena, field.pattern);
            }
        }
        PatternKind::Array(a) => {
            for &elem in &a.elements {
                v.visit_pattern(arena, elem);
            }
        }
        PatternKind::ArrayRest(ar) => {
            if let Some(binding) = ar.binding {
                v.visit_binding(arena, binding);
            }
        }
        PatternKind::Variant(vp) => {
            if let Some(fields) = &vp.fields {
                for field in fields {
                    v.visit_pattern(arena, field.pattern);
                }
            }
        }
        PatternKind::At(at) => {
            v.visit_binding(arena, at.binding);
            v.visit_pattern(arena, at.inner);
        }
        PatternKind::Or(or) => {
            for &alt in &or.alternatives {
                v.visit_pattern(arena, alt);
            }
        }
    }
}

/// Walk the children of a body.
pub fn walk_body<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: BodyId) {
    let body = &arena[id];
    v.visit_scope(arena, body.scope);
    for param in &body.params {
        v.visit_pattern(arena, param.pattern);
        if let Some(tref) = param.ty {
            v.visit_type_ref(arena, tref);
        }
    }
    if let Some(ret_ty) = body.return_ty {
        v.visit_type_ref(arena, ret_ty);
    }
    v.visit_expr(arena, body.root_expr);
}

/// Walk the children of an item.
pub fn walk_item<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: ItemId) {
    match &arena[id].kind {
        ItemKind::Function(f) => {
            v.visit_body(arena, f.body);
        }
        ItemKind::Struct(s) => {
            for field in &s.fields {
                if let Some(tref) = field.ty {
                    v.visit_type_ref(arena, tref);
                }
                if let Some(default) = field.default_value {
                    v.visit_expr(arena, default);
                }
            }
        }
        ItemKind::Enum(e) => {
            for variant in &e.variants {
                for field in &variant.fields {
                    if let Some(tref) = field.ty {
                        v.visit_type_ref(arena, tref);
                    }
                    if let Some(default) = field.default_value {
                        v.visit_expr(arena, default);
                    }
                }
            }
        }
        ItemKind::Newtype(n) => {
            if let Some(tref) = n.inner_ty {
                v.visit_type_ref(arena, tref);
            }
        }
        ItemKind::TypeAlias(ta) => {
            if let Some(tref) = ta.aliased_ty {
                v.visit_type_ref(arena, tref);
            }
        }
        ItemKind::Namespace(ns) => {
            v.visit_scope(arena, ns.scope);
            for &child_id in &ns.items {
                v.visit_item(arena, child_id);
            }
        }
        ItemKind::Error => {
            // Nothing to walk — error items have no children.
        }
    }
}

/// Walk the children of a type reference.
pub fn walk_type_ref<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: TypeRefId) {
    match &arena[id].kind {
        TypeRefKind::Path(p) => {
            for &arg in &p.type_args {
                v.visit_type_ref(arena, arg);
            }
        }
        TypeRefKind::Function(f) => {
            for &param in &f.params {
                v.visit_type_ref(arena, param);
            }
            v.visit_type_ref(arena, f.ret);
        }
        TypeRefKind::Tuple(t) => {
            for &elem in &t.elements {
                v.visit_type_ref(arena, elem);
            }
        }
        TypeRefKind::Optional(o) => {
            v.visit_type_ref(arena, o.inner);
        }
        TypeRefKind::Sum(s) => {
            for &alt in &s.alternatives {
                v.visit_type_ref(arena, alt);
            }
        }
        TypeRefKind::Error => {}
    }
}

/// Walk the entries in a scope (visits bindings only — items are visited
/// through their parent item/source-unit walk).
pub fn walk_scope<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: ScopeId) {
    let scope = &arena[id];
    for entry in &scope.entries {
        if let ScopeEntryTarget::Binding(binding_id) = entry.target {
            v.visit_binding(arena, binding_id);
        }
    }
}

/// Walk a source unit: visits all top-level items and the top-level body.
pub fn walk_source_unit<V: VirVisitor + ?Sized>(v: &mut V, arena: &VirArena, id: SourceUnitId) {
    let su = &arena[id];
    for &item_id in &su.items {
        v.visit_item(arena, item_id);
    }
    v.visit_body(arena, su.top_level_body);
}
