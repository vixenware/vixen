//! Bodies — scope-owning units (functions, closures, top-level thunks).
//!
//! Each body has its own parameter list and a root expression. Bodies form
//! a tree via [`parent`](Body::parent) for capture analysis — a closure
//! body's parent is the body it's lexically nested within.

use crate::{BindingId, BodyId, ExprId, PatternId, ScopeId, TypeRefId};
use vx_resolve::{DefId, SourceId, SourceSpan};

/// A body is a scope-owning unit: a function, closure, or top-level thunk.
#[derive(Debug, Clone)]
pub struct Body {
    /// The source span of the entire body.
    /// Populated during: CST walk.
    pub span: SourceSpan,

    /// What construct owns this body.
    /// Populated during: CST walk.
    pub owner: BodyOwner,

    /// The scope introduced by this body (holds parameters as bindings).
    /// Populated during: CST walk.
    pub scope: ScopeId,

    /// The parameters of this body (empty for top-level thunks).
    /// Populated during: CST walk.
    pub params: Vec<Param>,

    /// The root expression of this body. For functions, this is the body
    /// block or expression. For closures, this is the closure body.
    /// For top-level thunks, this is a synthetic block of all top-level statements.
    /// Populated during: CST walk.
    pub root_expr: ExprId,

    /// The parent body, for capture analysis. `None` for top-level functions
    /// and the top-level thunk itself.
    /// Populated during: CST walk.
    pub parent: Option<BodyId>,

    /// Optional return type annotation.
    /// Populated during: CST walk.
    pub return_ty: Option<TypeRefId>,

    /// Capability tags on this body (e.g. `#fs`, `#discover`).
    /// Only meaningful for function bodies.
    /// Populated during: CST walk.
    pub capabilities: Vec<String>,
}

/// What kind of construct owns a body.
#[derive(Debug, Clone)]
pub enum BodyOwner {
    /// A named function: `fn name(...) ( ... )`.
    Function {
        /// The [`DefId`] of the function definition from the resolver.
        def_id: DefId,
        /// The function name.
        name: String,
    },

    /// A closure expression: `(x, y) => body`.
    Closure,

    /// The implicit top-level body wrapping all top-level statements in
    /// a source file.
    TopLevel {
        /// The source file this top-level body belongs to.
        source: SourceId,
    },
}

/// A parameter in a function or closure.
///
/// `Param` is intentionally thin — it points to a [`Binding`](crate::Binding)
/// for name, kind, and type information. No duplication.
#[derive(Debug, Clone)]
pub struct Param {
    /// The binding introduced by this parameter.
    /// Populated during: CST walk.
    pub binding: BindingId,

    /// The pattern for this parameter (usually a simple binding pattern,
    /// but could be a destructuring pattern).
    /// Populated during: CST walk.
    pub pattern: PatternId,

    /// Optional type annotation on the parameter.
    /// Populated during: CST walk.
    pub ty: Option<TypeRefId>,

    /// Whether this is a keyword parameter (declared in keyword param list).
    pub is_keyword: bool,
}
