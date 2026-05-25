//! Bindings — named slots that hold values at runtime.
//!
//! Every binder site in Vixen syntax creates a [`Binding`] with a unique
//! [`BindingId`](crate::BindingId). This includes `let` patterns, function
//! parameters, closure parameters, match pattern bindings, `@`-pattern names,
//! and array rest names.
//!
//! Name references resolve to a `BindingId` (for locals) or a
//! [`DefId`](vx_resolve::DefId) (for top-level definitions). There is exactly
//! one canonical `Binding` per introduced name — no second lookup mechanism.

use crate::{BodyId, ExprId, ScopeId};
use vx_resolve::SourceSpan;
use vx_types::Ty;

/// A binding is a named slot that holds a value at runtime.
///
/// Bindings are the atoms of name resolution. Every `let x`, every parameter,
/// every match pattern binding gets a unique [`BindingId`](crate::BindingId).
#[derive(Debug, Clone)]
pub struct Binding {
    /// The name of this binding as written in source.
    pub name: String,

    /// Where the binding name appears in source.
    /// Populated during: CST walk.
    pub name_span: SourceSpan,

    /// How this binding was introduced.
    /// Populated during: CST walk.
    pub kind: BindingKind,

    /// The body this binding belongs to.
    /// Populated during: CST walk.
    pub body: BodyId,

    /// The scope this binding is registered in.
    /// Populated during: CST walk.
    pub scope: ScopeId,

    /// The inferred or declared type of this binding.
    /// Populated during: type checking pass. `None` before that.
    pub ty: Option<Ty>,

    /// The initializer expression for this binding, if it's a simple `let`
    /// binding (not destructuring). `None` for parameters, match bindings,
    /// destructuring patterns, and other non-let bindings.
    /// Populated during: CST walk (back-patched after pattern is built).
    pub init_expr: Option<ExprId>,

    /// Whether this binding is the target of an assignment statement.
    /// Populated during: name resolution pass (detected when `=` statement
    /// resolves to this binding). The CST walk initializes this to `false`;
    /// the resolver upgrades it to `true` for bindings that are assigned to.
    pub mutable: bool,
}

/// How a binding was introduced in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// Introduced by a `let` statement pattern.
    Let,

    /// Introduced as a function or closure parameter.
    Parameter,

    /// Introduced by a match arm pattern binding.
    PatternMatch,

    /// Introduced by an `@`-pattern: `name @ pattern`.
    AtPattern,

    /// Introduced by an array rest pattern: `..name`.
    ArrayRest,
}
