//! Bodies — scope-owning units (functions, closures, top-level thunks).
//!
//! Each body has its own parameter list and a root expression. Bodies form
//! a tree via [`parent`](Body::parent) for capture analysis — a closure
//! body's parent is the body it's lexically nested within.

use crate::{BindingId, BodyId, ExprId, ItemId, PatternId, ScopeId, TypeRefId};
use vx_parser::{SourceId, SourceSpan};

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

    /// Bindings from ancestor bodies referenced inside this body.
    /// Populated during: capture analysis pass. Empty for bodies with
    /// `parent: None`. Sorted by `BindingId` for deterministic output.
    pub captures: Vec<BindingId>,
}

/// What kind of construct owns a body.
#[derive(Debug, Clone)]
pub enum BodyOwner {
    /// A named function: `fn name(...) ( ... )`.
    /// Links to the function's [`Item`](crate::item::Item) rather than
    /// duplicating name/def_id — the item is the source of truth.
    Function {
        /// The [`ItemId`] of the function declaration item.
        item: ItemId,
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
/// `Param` does NOT carry a direct `BindingId` — bindings come from the
/// pattern tree. A destructuring parameter like `(a, b)` introduces
/// multiple bindings through its pattern. For simple identifier params,
/// the pattern is a `PatternKind::Binding` which holds the `BindingId`.
#[derive(Debug, Clone)]
pub struct Param {
    /// The pattern for this parameter. Usually a simple binding pattern
    /// (`PatternKind::Binding`), but may be a destructuring pattern that
    /// introduces multiple bindings.
    /// Populated during: CST walk.
    pub pattern: PatternId,

    /// Optional type annotation on the parameter.
    /// Populated during: CST walk.
    pub ty: Option<TypeRefId>,

    /// Whether this is a keyword parameter (declared in keyword param list).
    pub is_keyword: bool,

    /// Whether this parameter has a default value expression.
    /// Only meaningful for keyword parameters.
    /// Populated during: CST walk.
    pub has_default: bool,

    /// Doc comment on this parameter, if any.
    /// Populated during: CST walk.
    pub doc: Option<String>,
}
