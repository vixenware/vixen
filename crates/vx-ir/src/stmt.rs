//! Statement nodes.
//!
//! Statements appear inside blocks and at the top level. Each gets a unique
//! [`StmtId`](crate::StmtId).

use crate::expr::UpdateField;
use crate::{BindingId, BodyId, ExprId, ItemId, PatternId, ScopeId, TypeRefId};
use vx_parser::SourceSpan;

/// A statement node in the VIR.
#[derive(facet::Facet, Debug, Clone)]
pub struct Stmt {
    /// The source location of this statement.
    /// Populated during: CST walk.
    pub span: SourceSpan,

    /// The body this statement belongs to.
    /// Populated during: CST walk.
    pub body: BodyId,

    /// The lexical scope containing this statement.
    /// Populated during: CST walk.
    pub scope: ScopeId,

    /// The syntactic form of this statement.
    /// Populated during: CST walk.
    pub kind: StmtKind,
}

/// Discriminant for all statement forms.
#[derive(facet::Facet, Debug, Clone)]
#[repr(u8)]
pub enum StmtKind {
    /// `let pattern = expr` or `let name: Type = expr`.
    Let(LetStmt),

    /// `name = expr` — reassignment to a mutable binding.
    Assign(AssignStmt),

    /// `name with= { field: value }` — compound update in place.
    CompoundUpdate(CompoundUpdateStmt),

    /// `target.field = expr` — field assignment on a mutable binding.
    FieldAssign(FieldAssignStmt),

    /// `return expr` or `return` (returns unit).
    Return(ReturnStmt),

    /// A declaration used as a statement (fn, struct, etc. inside a block).
    Item(ItemId),

    /// An expression used as a statement (its value is discarded).
    Expr(ExprId),

    /// An error recovery node. Produced when the CST contains a broken statement.
    Error,
}

/// A let statement: `let pattern = expr`.
#[derive(facet::Facet, Debug, Clone)]
pub struct LetStmt {
    /// The pattern being bound. For simple `let x = ...` this is a
    /// [`PatternKind::Binding`](crate::PatternKind::Binding).
    pub pattern: PatternId,

    /// Optional type annotation: `let x: U64 = ...`.
    pub ty: Option<TypeRefId>,

    /// The initializer expression.
    pub init: ExprId,
}

/// An assignment statement: `name = expr`.
#[derive(facet::Facet, Debug, Clone)]
pub struct AssignStmt {
    /// The name being assigned to.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The new value expression.
    pub value: ExprId,
    /// The binding being assigned to.
    /// Populated during: name resolution pass.
    pub resolved: Option<BindingId>,
}

/// A compound update statement: `name with= { field: value }`.
#[derive(facet::Facet, Debug, Clone)]
pub struct CompoundUpdateStmt {
    /// The name being updated.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The update fields.
    pub updates: Vec<UpdateField>,
    /// The binding being updated.
    /// Populated during: name resolution pass.
    pub resolved: Option<BindingId>,
}

/// A field assignment statement: `a.b.c = expr`.
#[derive(facet::Facet, Debug, Clone)]
pub struct FieldAssignStmt {
    /// The base name being assigned through (the `a` in `a.b.c = ...`).
    pub base: String,
    /// Span of the base name token.
    pub base_span: SourceSpan,
    /// The field path after the base (e.g. `["b", "c"]` for `a.b.c = ...`).
    pub field_path: Vec<String>,
    /// Span of the entire target path.
    pub target_span: SourceSpan,
    /// The new value expression.
    pub value: ExprId,
    /// The binding being assigned through (resolves `base`).
    /// Populated during: name resolution pass.
    pub resolved: Option<BindingId>,
}

/// A return statement: `return expr` or bare `return`.
#[derive(facet::Facet, Debug, Clone)]
pub struct ReturnStmt {
    /// The returned expression. `None` for bare `return` (returns unit).
    pub value: Option<ExprId>,
}
