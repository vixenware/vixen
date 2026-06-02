//! The exec **bundle**: the static transitive closure of an observer's `Body` —
//! every node and type it can reach — packed in the orchestrator's id space, ready
//! to ship to a fresh **mirror** runtime that installs it verbatim and never
//! re-allocates an id (see the exec design dialogue).
//!
//! Reachability follows **resolution** edges, not just the syntax tree: a `NameRef`
//! resolved to a function pulls in that function's `Body`; a struct literal pulls in
//! its `Item` and type def; a captured binding pulls in its defining body; every
//! node's `.ty` pulls in its transitive type closure. A missed edge silently breaks
//! the mirror (it would index a node that didn't travel), so **completeness is the
//! whole job** — hence the worklist + tests.

use std::collections::HashSet;

use vx_types::{Ty, TypeDef, TypeEnv, TypeId};

use crate::arena::VirArena;
use crate::binding::Binding;
use crate::body::{Body, BodyOwner};
use crate::expr::{
    ArrayElement, Expr, ExprKind, MapElement, MapKey, PartialArg, StringPart, UpdateField,
};
use crate::item::{Item, ItemKind};
use crate::pattern::{Pattern, PatternKind};
use crate::scope::DefTarget;
use crate::stmt::{Stmt, StmtKind};
use crate::type_ref::{TypeRef, TypeRefKind};
use crate::{BindingId, BodyId, ExprId, ItemId, PatternId, StmtId, TypeRefId};
use vx_types::TypeDefId;

/// A self-contained slice of a program: an observer `Body` and everything it can
/// reach, every node keyed by its **original (orchestrator) id**. A fresh mirror
/// installs these verbatim — the ids are sparse (holes where the program had nodes
/// the observer can't reach) but never re-numbered. Facet-encoded for the wire.
#[derive(facet::Facet, Debug, Clone)]
pub struct Bundle {
    /// The observer body to evaluate.
    pub entry: BodyId,
    pub exprs: Vec<(ExprId, Expr)>,
    pub stmts: Vec<(StmtId, Stmt)>,
    pub patterns: Vec<(PatternId, Pattern)>,
    pub bindings: Vec<(BindingId, Binding)>,
    pub bodies: Vec<(BodyId, Body)>,
    pub items: Vec<(ItemId, Item)>,
    pub type_refs: Vec<(TypeRefId, TypeRef)>,
    pub types: Vec<(TypeId, Ty)>,
    pub type_defs: Vec<(TypeDefId, TypeDef)>,
}

/// Compute the bundle: the transitive closure of `entry` over the type-checked
/// `arena` + `env`.
pub fn reachable(arena: &VirArena, env: &TypeEnv, entry: BodyId) -> Bundle {
    let mut c = Collector {
        arena,
        env,
        exprs: HashSet::new(),
        stmts: HashSet::new(),
        patterns: HashSet::new(),
        bindings: HashSet::new(),
        bodies: HashSet::new(),
        items: HashSet::new(),
        type_refs: HashSet::new(),
        types: HashSet::new(),
        type_defs: HashSet::new(),
        body_q: Vec::new(),
        item_q: Vec::new(),
    };
    c.enqueue_body(entry);
    loop {
        if let Some(b) = c.body_q.pop() {
            c.collect_body(b);
        } else if let Some(i) = c.item_q.pop() {
            c.collect_item(i);
        } else {
            break;
        }
    }
    c.into_bundle(entry)
}

impl Bundle {
    /// Install this bundle into a fresh `VirProgram` + `TypeEnv` in the **original id
    /// space** — the mirror side. Each node lands at its verbatim id; the holes (ids
    /// the observer can't reach) hold placeholders that eval never indexes, because
    /// the closure is complete. Scopes/source-units stay empty: eval reads neither
    /// when a body is invoked directly.
    pub fn install(&self) -> (crate::build::VirProgram, TypeEnv) {
        let arena = VirArena {
            exprs: densify(&self.exprs, |id| id.index()),
            stmts: densify(&self.stmts, |id| id.index()),
            patterns: densify(&self.patterns, |id| id.index()),
            bindings: densify(&self.bindings, |id| id.index()),
            bodies: densify(&self.bodies, |id| id.index()),
            items: densify(&self.items, |id| id.index()),
            scopes: Vec::new(),
            type_refs: densify(&self.type_refs, |id| id.index()),
            source_units: Vec::new(),
            imports: std::collections::HashMap::new(),
            grammars: std::collections::HashMap::new(),
        };
        let env = TypeEnv::from_sparse(self.types.clone(), self.type_defs.clone());
        (
            crate::build::VirProgram {
                arena,
                source_units: Vec::new(),
            },
            env,
        )
    }
}

/// Reconstruct a dense node `Vec` from `(id, node)` pairs: place each at its id
/// index, fill holes with a clone of the first node. The holes are never indexed —
/// a complete bundle only references collected ids — so their content is irrelevant.
fn densify<Id: Copy, T: Clone>(pairs: &[(Id, T)], idx: impl Fn(Id) -> usize) -> Vec<T> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let max = pairs.iter().map(|(id, _)| idx(*id)).max().unwrap();
    let mut v = vec![pairs[0].1.clone(); max + 1];
    for (id, node) in pairs {
        v[idx(*id)] = node.clone();
    }
    v
}

struct Collector<'a> {
    arena: &'a VirArena,
    env: &'a TypeEnv,
    exprs: HashSet<ExprId>,
    stmts: HashSet<StmtId>,
    patterns: HashSet<PatternId>,
    bindings: HashSet<BindingId>,
    bodies: HashSet<BodyId>,
    items: HashSet<ItemId>,
    type_refs: HashSet<TypeRefId>,
    types: HashSet<TypeId>,
    type_defs: HashSet<TypeDefId>,
    body_q: Vec<BodyId>,
    item_q: Vec<ItemId>,
}

impl Collector<'_> {
    fn enqueue_body(&mut self, b: BodyId) {
        if self.bodies.insert(b) {
            self.body_q.push(b);
        }
    }

    fn enqueue_item(&mut self, i: ItemId) {
        if self.items.insert(i) {
            self.item_q.push(i);
        }
    }

    // ---- bodies / items -------------------------------------------------------

    fn collect_body(&mut self, b: BodyId) {
        let body = &self.arena[b];
        if let BodyOwner::Function { item } = body.owner {
            self.enqueue_item(item);
        }
        for &cap in &body.captures {
            self.collect_binding(cap);
        }
        for p in &body.params {
            self.collect_pattern(p.pattern);
            if let Some(tr) = p.ty {
                self.collect_type_ref(tr);
            }
        }
        if let Some(tr) = body.return_ty {
            self.collect_type_ref(tr);
        }
        self.collect_expr(body.root_expr);
    }

    fn collect_item(&mut self, i: ItemId) {
        match &self.arena[i].kind {
            ItemKind::Function(f) => {
                self.enqueue_body(f.body);
                if let Some(td) = f.type_id {
                    self.collect_type_def(td);
                }
            }
            ItemKind::Struct(s) => {
                if let Some(td) = s.type_id {
                    self.collect_type_def(td);
                }
                for fd in &s.fields {
                    if let Some(tr) = fd.ty {
                        self.collect_type_ref(tr);
                    }
                    if let Some(dv) = fd.default_value {
                        self.collect_expr(dv);
                    }
                }
            }
            ItemKind::Enum(e) => {
                if let Some(td) = e.type_id {
                    self.collect_type_def(td);
                }
                for v in &e.variants {
                    for fd in &v.fields {
                        if let Some(tr) = fd.ty {
                            self.collect_type_ref(tr);
                        }
                        if let Some(dv) = fd.default_value {
                            self.collect_expr(dv);
                        }
                    }
                }
            }
            ItemKind::Newtype(n) => {
                if let Some(td) = n.type_id {
                    self.collect_type_def(td);
                }
                if let Some(tr) = n.inner_ty {
                    self.collect_type_ref(tr);
                }
            }
            ItemKind::TypeAlias(t) => {
                if let Some(td) = t.type_id {
                    self.collect_type_def(td);
                }
                if let Some(tr) = t.aliased_ty {
                    self.collect_type_ref(tr);
                }
            }
            ItemKind::Namespace(ns) => {
                for &it in &ns.items {
                    self.enqueue_item(it);
                }
            }
            ItemKind::Error => {}
        }
    }

    fn collect_binding(&mut self, b: BindingId) {
        if !self.bindings.insert(b) {
            return;
        }
        let binding = &self.arena[b];
        // The binding's defining body must travel: forcing a captured thunk re-enters
        // it (eval sets `current_body`), so an uncollected body would be a hole.
        self.enqueue_body(binding.body);
        if let Some(ty) = binding.ty {
            self.collect_type(ty);
        }
        if let Some(init) = binding.init_expr {
            self.collect_expr(init);
        }
    }

    // ---- exprs ----------------------------------------------------------------

    fn collect_expr(&mut self, e: ExprId) {
        if !self.exprs.insert(e) {
            return;
        }
        let expr = &self.arena[e];
        if let Some(ty) = expr.ty {
            self.collect_type(ty);
        }
        match &expr.kind {
            ExprKind::Literal(_) | ExprKind::Bomb | ExprKind::Error => {}
            ExprKind::PathLiteral(p) => self.collect_string_parts(&p.parts),
            ExprKind::NameRef(n) => {
                if let Some(t) = n.resolved {
                    self.collect_def_target(t);
                }
            }
            ExprKind::Block(b) => {
                for &s in &b.stmts {
                    self.collect_stmt(s);
                }
                if let Some(tail) = b.tail {
                    self.collect_expr(tail);
                }
            }
            ExprKind::Closure(c) => self.enqueue_body(c.body),
            ExprKind::Tuple(t) => self.collect_exprs(&t.elements),
            ExprKind::Array(a) => {
                for el in &a.elements {
                    match el {
                        ArrayElement::Expr(x) | ArrayElement::Spread(x) => self.collect_expr(*x),
                    }
                }
            }
            ExprKind::Record(r) => {
                for f in &r.fields {
                    self.collect_expr(f.value);
                }
            }
            ExprKind::Map(m) => {
                for el in &m.entries {
                    match el {
                        MapElement::Entry(en) => {
                            if let MapKey::Expr(k) = &en.key {
                                self.collect_expr(*k);
                            }
                            self.collect_expr(en.value);
                        }
                        MapElement::Spread(x) => self.collect_expr(*x),
                    }
                }
            }
            ExprKind::StructLiteral(s) => {
                if let Some(item) = s.resolved {
                    self.enqueue_item(item);
                }
                for f in &s.fields {
                    self.collect_expr(f.value);
                }
            }
            ExprKind::Call(call) => {
                self.collect_expr(call.callee);
                for a in &call.args {
                    self.collect_expr(a.expr);
                }
                for &tr in &call.type_args {
                    self.collect_type_ref(tr);
                }
            }
            ExprKind::Where(w) => {
                self.collect_expr(w.callee);
                self.collect_expr(w.keywords);
            }
            ExprKind::Invoke(i) => self.collect_expr(i.callee),
            ExprKind::PartialApp(p) => {
                self.collect_expr(p.callee);
                for a in &p.args {
                    if let PartialArg::Expr(x) = a {
                        self.collect_expr(*x);
                    }
                }
            }
            ExprKind::Index(ix) => {
                self.collect_expr(ix.receiver);
                self.collect_expr(ix.index);
            }
            ExprKind::Field(f) => self.collect_expr(f.receiver),
            ExprKind::If(i) => {
                self.collect_expr(i.condition);
                self.collect_expr(i.then_branch);
                if let Some(e) = i.else_branch {
                    self.collect_expr(e);
                }
            }
            ExprKind::Match(m) => {
                self.collect_expr(m.scrutinee);
                for arm in &m.arms {
                    self.collect_pattern(arm.pattern);
                    self.collect_expr(arm.body);
                }
            }
            ExprKind::Binary(b) => {
                self.collect_expr(b.lhs);
                self.collect_expr(b.rhs);
            }
            ExprKind::Prefix(p) => self.collect_expr(p.operand),
            ExprKind::Range(r) => {
                if let Some(s) = r.start {
                    self.collect_expr(s);
                }
                if let Some(e) = r.end {
                    self.collect_expr(e);
                }
            }
            ExprKind::TypeOp(t) => {
                self.collect_expr(t.expr);
                self.collect_type_ref(t.ty);
            }
            ExprKind::With(w) => {
                self.collect_expr(w.receiver);
                self.collect_update_fields(&w.updates);
            }
            ExprKind::Fallible(f) => self.collect_expr(f.expr),
            ExprKind::InterpolatedString(s) => self.collect_string_parts(&s.parts),
            ExprKind::Import(_) => {
                // The imported items are reached through the resolved NameRef/Field
                // edges that select them; the import expr itself carries no node refs.
            }
            ExprKind::CliLiteral(c) => {
                for arg in &c.args {
                    self.collect_string_parts(&arg.pieces);
                }
            }
            ExprKind::Fail(f) => {
                if let Some(m) = f.message {
                    self.collect_expr(m);
                }
            }
        }
    }

    fn collect_exprs(&mut self, ids: &[ExprId]) {
        for &e in ids {
            self.collect_expr(e);
        }
    }

    fn collect_string_parts(&mut self, parts: &[StringPart]) {
        for p in parts {
            if let StringPart::Expr(e) = p {
                self.collect_expr(*e);
            }
        }
    }

    fn collect_update_fields(&mut self, fields: &[UpdateField]) {
        for f in fields {
            match f {
                UpdateField::Set { value, .. } => self.collect_expr(*value),
                UpdateField::Nested { updates, .. } => self.collect_update_fields(updates),
            }
        }
    }

    fn collect_def_target(&mut self, t: DefTarget) {
        match t {
            DefTarget::Binding(b) => self.collect_binding(b),
            DefTarget::Item(i) => self.enqueue_item(i),
            DefTarget::Variant { enum_item, .. } => self.enqueue_item(enum_item),
        }
    }

    // ---- stmts ----------------------------------------------------------------

    fn collect_stmt(&mut self, s: StmtId) {
        if !self.stmts.insert(s) {
            return;
        }
        match &self.arena[s].kind {
            StmtKind::Let(l) => {
                self.collect_pattern(l.pattern);
                if let Some(tr) = l.ty {
                    self.collect_type_ref(tr);
                }
                self.collect_expr(l.init);
            }
            StmtKind::Assign(a) => {
                self.collect_expr(a.value);
                if let Some(b) = a.resolved {
                    self.collect_binding(b);
                }
            }
            StmtKind::CompoundUpdate(cu) => {
                self.collect_update_fields(&cu.updates);
                if let Some(b) = cu.resolved {
                    self.collect_binding(b);
                }
            }
            StmtKind::FieldAssign(fa) => {
                self.collect_expr(fa.value);
                if let Some(b) = fa.resolved {
                    self.collect_binding(b);
                }
            }
            StmtKind::Return(r) => {
                if let Some(v) = r.value {
                    self.collect_expr(v);
                }
            }
            StmtKind::Item(i) => self.enqueue_item(*i),
            StmtKind::Expr(e) => self.collect_expr(*e),
            StmtKind::Error => {}
        }
    }

    // ---- patterns -------------------------------------------------------------

    fn collect_pattern(&mut self, p: PatternId) {
        if !self.patterns.insert(p) {
            return;
        }
        let pat = &self.arena[p];
        if let Some(ty) = pat.ty {
            self.collect_type(ty);
        }
        match &pat.kind {
            PatternKind::Binding(b) => self.collect_binding(b.binding),
            PatternKind::Wildcard | PatternKind::Unit | PatternKind::Error => {}
            PatternKind::Literal(_) => {}
            PatternKind::Tuple(t) => self.collect_patterns(&t.elements),
            PatternKind::Struct(s) => {
                for f in &s.fields {
                    self.collect_pattern(f.pattern);
                }
            }
            PatternKind::Array(a) => self.collect_patterns(&a.elements),
            PatternKind::ArrayRest(r) => {
                if let Some(b) = r.binding {
                    self.collect_binding(b);
                }
            }
            PatternKind::Variant(v) => {
                if let Some(item) = v.resolved {
                    self.enqueue_item(item);
                }
                if let Some(fields) = &v.fields {
                    for f in fields {
                        self.collect_pattern(f.pattern);
                    }
                }
            }
            PatternKind::At(a) => {
                self.collect_binding(a.binding);
                self.collect_pattern(a.inner);
            }
            PatternKind::Or(o) => self.collect_patterns(&o.alternatives),
        }
    }

    fn collect_patterns(&mut self, ids: &[PatternId]) {
        for &p in ids {
            self.collect_pattern(p);
        }
    }

    // ---- type refs (source-level annotations) ---------------------------------

    fn collect_type_ref(&mut self, tr: TypeRefId) {
        if !self.type_refs.insert(tr) {
            return;
        }
        match &self.arena[tr].kind {
            TypeRefKind::Path(p) => {
                for &a in &p.type_args {
                    self.collect_type_ref(a);
                }
            }
            TypeRefKind::Function(f) => {
                for &p in &f.params {
                    self.collect_type_ref(p);
                }
                self.collect_type_ref(f.ret);
            }
            TypeRefKind::Tuple(t) => {
                for &el in &t.elements {
                    self.collect_type_ref(el);
                }
            }
            TypeRefKind::Sum(s) => {
                for &alt in &s.alternatives {
                    self.collect_type_ref(alt);
                }
            }
            TypeRefKind::Error => {}
        }
    }

    // ---- the type graph (interned Ty + TypeDef) -------------------------------

    fn collect_type(&mut self, id: TypeId) {
        if !self.types.insert(id) {
            return;
        }
        match self.env.get_ty(id).clone() {
            Ty::Scalar(_) | Ty::Unit | Ty::Poison | Ty::Infer(_) | Ty::Param(_) => {}
            Ty::Struct(t) => self.collect_def_and_args(t.def, &t.type_args),
            Ty::Enum(t) => self.collect_def_and_args(t.def, &t.type_args),
            Ty::Newtype(t) => self.collect_def_and_args(t.def, &t.type_args),
            Ty::Alias(t) => self.collect_def_and_args(t.def, &t.type_args),
            Ty::Opaque(t) => self.collect_def_and_args(t.def, &t.type_args),
            Ty::Tuple(t) => {
                for &el in &t.0 {
                    self.collect_type(el);
                }
            }
            Ty::Function(f) => {
                for &p in &f.params {
                    self.collect_type(p);
                }
                self.collect_type(f.ret);
            }
            Ty::Array(a) => self.collect_type(a.0),
            Ty::Map(m) => {
                self.collect_type(m.key);
                self.collect_type(m.value);
            }
            Ty::Stream(s) => self.collect_type(s.0),
        }
    }

    fn collect_def_and_args(&mut self, def: TypeDefId, args: &[TypeId]) {
        self.collect_type_def(def);
        for &a in args {
            self.collect_type(a);
        }
    }

    fn collect_type_def(&mut self, id: TypeDefId) {
        if !self.type_defs.insert(id) {
            return;
        }
        match self.env.get_def(id).clone() {
            TypeDef::Struct(s) => {
                if let Some(pe) = s.parent_enum {
                    self.collect_type_def(pe);
                }
                for f in &s.fields {
                    self.collect_type(f.ty);
                }
            }
            TypeDef::Enum(e) => {
                for &v in &e.variants {
                    self.collect_type_def(v);
                }
            }
            TypeDef::Newtype(n) => self.collect_type(n.inner),
            TypeDef::Alias(a) => self.collect_type(a.target),
            TypeDef::Opaque(_) => {}
        }
    }

    // ---- pack -----------------------------------------------------------------

    fn into_bundle(self, entry: BodyId) -> Bundle {
        // Sorted by id index for a deterministic wire encoding.
        let mut exprs: Vec<_> = self
            .exprs
            .iter()
            .map(|&id| (id, self.arena[id].clone()))
            .collect();
        exprs.sort_by_key(|(id, _)| id.index());
        let mut stmts: Vec<_> = self
            .stmts
            .iter()
            .map(|&id| (id, self.arena[id].clone()))
            .collect();
        stmts.sort_by_key(|(id, _)| id.index());
        let mut patterns: Vec<_> = self
            .patterns
            .iter()
            .map(|&id| (id, self.arena[id].clone()))
            .collect();
        patterns.sort_by_key(|(id, _)| id.index());
        let mut bindings: Vec<_> = self
            .bindings
            .iter()
            .map(|&id| (id, self.arena[id].clone()))
            .collect();
        bindings.sort_by_key(|(id, _)| id.index());
        let mut bodies: Vec<_> = self
            .bodies
            .iter()
            .map(|&id| (id, self.arena[id].clone()))
            .collect();
        bodies.sort_by_key(|(id, _)| id.index());
        let mut items: Vec<_> = self
            .items
            .iter()
            .map(|&id| (id, self.arena[id].clone()))
            .collect();
        items.sort_by_key(|(id, _)| id.index());
        let mut type_refs: Vec<_> = self
            .type_refs
            .iter()
            .map(|&id| (id, self.arena[id].clone()))
            .collect();
        type_refs.sort_by_key(|(id, _)| id.index());
        let mut types: Vec<_> = self
            .types
            .iter()
            .map(|&id| (id, self.env.get_ty(id).clone()))
            .collect();
        types.sort_by_key(|(id, _)| id.0);
        let mut type_defs: Vec<_> = self
            .type_defs
            .iter()
            .map(|&id| (id, self.env.get_def(id).clone()))
            .collect();
        type_defs.sort_by_key(|(id, _)| id.0);

        Bundle {
            entry,
            exprs,
            stmts,
            patterns,
            bindings,
            bodies,
            items,
            type_refs,
            types,
            type_defs,
        }
    }
}
