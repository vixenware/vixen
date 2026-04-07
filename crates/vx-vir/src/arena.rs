//! The central VIR arena.
//!
//! [`VirArena`] holds all VIR nodes for all source files in a program.
//! All ID types ([`ExprId`], [`StmtId`], etc.) index into the corresponding
//! `Vec`. The arena is append-only during construction (CST walk), then
//! mutated in-place during later passes (name resolution populates `resolved`
//! fields, type checking populates `ty` fields).

use crate::binding::Binding;
use crate::body::Body;
use crate::expr::Expr;
use crate::item::{Item, ItemKind};
use crate::pattern::Pattern;
use crate::scope::{Scope, ScopeEntryTarget};
use crate::source_unit::SourceUnit;
use crate::stmt::Stmt;
use crate::type_ref::TypeRef;
use crate::{
    BindingId, BodyId, ExprId, ItemId, PatternId, ScopeId, SourceUnitId, StmtId, TypeRefId,
};
use vx_resolve::SourceId;

/// The central VIR arena. Holds all nodes for all source files in a program.
///
/// Each `Vec` is indexed by the corresponding ID type. For example,
/// `arena.exprs[expr_id.index()]` returns the expression with that ID.
/// The `Index` trait impls provide ergonomic access: `arena[expr_id]`.
#[derive(Debug, Clone)]
pub struct VirArena {
    /// All expression nodes.
    pub exprs: Vec<Expr>,
    /// All statement nodes.
    pub stmts: Vec<Stmt>,
    /// All pattern nodes.
    pub patterns: Vec<Pattern>,
    /// All binding nodes.
    pub bindings: Vec<Binding>,
    /// All body nodes (functions, closures, top-level thunks).
    pub bodies: Vec<Body>,
    /// All item (declaration) nodes.
    pub items: Vec<Item>,
    /// All lexical scopes.
    pub scopes: Vec<Scope>,
    /// All source-level type annotations.
    pub type_refs: Vec<TypeRef>,
    /// All source file roots.
    pub source_units: Vec<SourceUnit>,
}

impl VirArena {
    /// Create a new empty arena.
    pub fn new() -> Self {
        Self {
            exprs: Vec::new(),
            stmts: Vec::new(),
            patterns: Vec::new(),
            bindings: Vec::new(),
            bodies: Vec::new(),
            items: Vec::new(),
            scopes: Vec::new(),
            type_refs: Vec::new(),
            source_units: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Allocation methods (used during CST walk)
    // -----------------------------------------------------------------------

    /// Allocate a new expression, returning its ID.
    pub fn alloc_expr(&mut self, expr: Expr) -> ExprId {
        let id = ExprId::from_index(self.exprs.len());
        self.exprs.push(expr);
        id
    }

    /// Allocate a new statement, returning its ID.
    pub fn alloc_stmt(&mut self, stmt: Stmt) -> StmtId {
        let id = StmtId::from_index(self.stmts.len());
        self.stmts.push(stmt);
        id
    }

    /// Allocate a new pattern, returning its ID.
    pub fn alloc_pattern(&mut self, pat: Pattern) -> PatternId {
        let id = PatternId::from_index(self.patterns.len());
        self.patterns.push(pat);
        id
    }

    /// Allocate a new binding, returning its ID.
    pub fn alloc_binding(&mut self, binding: Binding) -> BindingId {
        let id = BindingId::from_index(self.bindings.len());
        self.bindings.push(binding);
        id
    }

    /// Allocate a new body, returning its ID.
    pub fn alloc_body(&mut self, body: Body) -> BodyId {
        let id = BodyId::from_index(self.bodies.len());
        self.bodies.push(body);
        id
    }

    /// Allocate a new item, returning its ID.
    pub fn alloc_item(&mut self, item: Item) -> ItemId {
        let id = ItemId::from_index(self.items.len());
        self.items.push(item);
        id
    }

    /// Allocate a new scope, returning its ID.
    pub fn alloc_scope(&mut self, scope: Scope) -> ScopeId {
        let id = ScopeId::from_index(self.scopes.len());
        self.scopes.push(scope);
        id
    }

    /// Allocate a new type reference, returning its ID.
    pub fn alloc_type_ref(&mut self, type_ref: TypeRef) -> TypeRefId {
        let id = TypeRefId::from_index(self.type_refs.len());
        self.type_refs.push(type_ref);
        id
    }

    /// Allocate a new source unit, returning its ID.
    pub fn alloc_source_unit(&mut self, source_unit: SourceUnit) -> SourceUnitId {
        let id = SourceUnitId::from_index(self.source_units.len());
        self.source_units.push(source_unit);
        id
    }

    // -----------------------------------------------------------------------
    // Convenience lookups
    // -----------------------------------------------------------------------

    /// Find the source unit for a given source file.
    pub fn source_unit_for(&self, source: SourceId) -> Option<(SourceUnitId, &SourceUnit)> {
        self.source_units
            .iter()
            .enumerate()
            .find(|(_, su)| su.source == source)
            .map(|(i, su)| (SourceUnitId::from_index(i), su))
    }

    /// Iterate all bodies that are children of the given body.
    pub fn child_bodies(&self, parent: BodyId) -> impl Iterator<Item = (BodyId, &Body)> {
        self.bodies
            .iter()
            .enumerate()
            .filter(move |(_, b)| b.parent == Some(parent))
            .map(|(i, b)| (BodyId::from_index(i), b))
    }

    /// Iterate all bindings in a given scope (filtering the unified entries list).
    pub fn bindings_in_scope(&self, scope: ScopeId) -> impl Iterator<Item = (BindingId, &Binding)> {
        self.scopes[scope.index()]
            .entries
            .iter()
            .filter_map(|entry| match entry.target {
                ScopeEntryTarget::Binding(id) => Some((id, &self.bindings[id.index()])),
                ScopeEntryTarget::Item(_) => None,
            })
    }

    /// Get the name of an item by its ID.
    ///
    /// Returns the item's declared name (e.g. function name, struct name).
    /// For error items, returns `"<error>"`.
    pub fn item_name(&self, id: ItemId) -> &str {
        match &self.items[id.index()].kind {
            ItemKind::Function(f) => &f.name,
            ItemKind::Struct(s) => &s.name,
            ItemKind::Enum(e) => &e.name,
            ItemKind::Newtype(n) => &n.name,
            ItemKind::TypeAlias(t) => &t.name,
            ItemKind::Namespace(ns) => &ns.name,
            ItemKind::Error => "<error>",
        }
    }
}

impl Default for VirArena {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Index trait implementations for ergonomic access: arena[id]
// ---------------------------------------------------------------------------

macro_rules! impl_index {
    ($id:ty, $field:ident, $output:ty) => {
        impl std::ops::Index<$id> for VirArena {
            type Output = $output;
            fn index(&self, id: $id) -> &$output {
                &self.$field[id.index()]
            }
        }

        impl std::ops::IndexMut<$id> for VirArena {
            fn index_mut(&mut self, id: $id) -> &mut $output {
                &mut self.$field[id.index()]
            }
        }
    };
}

impl_index!(ExprId, exprs, Expr);
impl_index!(StmtId, stmts, Stmt);
impl_index!(PatternId, patterns, Pattern);
impl_index!(BindingId, bindings, Binding);
impl_index!(BodyId, bodies, Body);
impl_index!(ItemId, items, Item);
impl_index!(ScopeId, scopes, Scope);
impl_index!(TypeRefId, type_refs, TypeRef);
impl_index!(SourceUnitId, source_units, SourceUnit);
