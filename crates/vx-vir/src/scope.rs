//! Lexical scopes.
//!
//! Scopes form a tree via `parent` pointers. Every construct that introduces
//! new name visibility gets a [`Scope`] with a unique [`ScopeId`](crate::ScopeId):
//! function bodies, blocks, match arms, and namespaces.
//!
//! Bindings are registered in the scope that introduces them. Name resolution
//! walks up the scope tree to find a binding or definition.

use crate::{BindingId, BodyId, ExprId, ItemId, ScopeId};

/// A lexical scope in the VIR.
///
/// Populated during: CST walk.
#[derive(Debug, Clone)]
pub struct Scope {
    /// The parent scope, or `None` for the root scope of a source file.
    pub parent: Option<ScopeId>,

    /// What syntactic construct owns this scope.
    pub owner: ScopeOwner,

    /// The bindings visible in this scope (not including parent scopes).
    /// Populated during: CST walk.
    pub bindings: Vec<BindingId>,
}

/// What syntactic construct introduced this scope.
#[derive(Debug, Clone)]
pub enum ScopeOwner {
    /// A function or closure body.
    Body(BodyId),

    /// A block expression: `{ ... }`.
    Block(ExprId),

    /// A match arm (the pattern introduces bindings visible in the arm body).
    MatchArm {
        /// The match expression this arm belongs to.
        match_expr: ExprId,
        /// The index of this arm within the match expression's arm list.
        arm_index: u32,
    },

    /// A namespace declaration.
    Namespace(ItemId),
}
