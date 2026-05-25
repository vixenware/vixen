//! Pattern nodes.
//!
//! Patterns appear in `let` bindings, match arms, and `@`-expressions. Each
//! gets a unique [`PatternId`](crate::PatternId). Paren patterns are eliminated
//! during the CST walk.

use crate::expr::LitKind;
use crate::{BindingId, BodyId, ItemId, PatternId, ScopeId};
use vx_resolve::SourceSpan;
use vx_types::Ty;

/// A pattern node in the VIR.
#[derive(Debug, Clone)]
pub struct Pattern {
    /// Source location of this pattern.
    /// Populated during: CST walk.
    pub span: SourceSpan,

    /// The body this pattern belongs to.
    /// Populated during: CST walk.
    pub body: BodyId,

    /// The lexical scope containing this pattern.
    /// Populated during: CST walk.
    pub scope: ScopeId,

    /// The syntactic form of this pattern.
    /// Populated during: CST walk.
    pub kind: PatternKind,

    /// The type this pattern matches against.
    /// Populated during: type checking pass. `None` before that.
    pub ty: Option<Ty>,
}

/// Discriminant for all pattern forms.
///
/// Paren patterns are NOT represented — they are unwrapped during the CST walk.
#[derive(Debug, Clone)]
pub enum PatternKind {
    /// A binding pattern: `x`. Introduces a new binding.
    Binding(PatternBinding),

    /// A wildcard pattern: `_`. Matches anything, binds nothing.
    Wildcard,

    /// A literal pattern: `42`, `"hello"`, `true`.
    Literal(PatternLiteral),

    /// A tuple pattern: `(a, b, c)`.
    Tuple(TuplePattern),

    /// A struct destructuring pattern: `{ x, y: pat }`.
    Struct(StructPattern),

    /// An array pattern: `[a, b, ..rest]`.
    Array(ArrayPattern),

    /// A rest pattern within an array: `..name` or bare `..`.
    ArrayRest(ArrayRestPattern),

    /// The unit pattern: `()`.
    Unit,

    /// A variant pattern: `Some { value }` or `None`.
    Variant(VariantPattern),

    /// An at-pattern: `name @ pattern`. Binds the whole matched value
    /// to `name` while also destructuring via `pattern`.
    At(AtPattern),

    /// An or-pattern: `pat1 | pat2`. Matches if either alternative matches.
    Or(OrPattern),

    /// An error recovery node. Produced when the CST contains a broken pattern.
    /// The type of an error pattern is always `Ty::Poison`.
    Error,
}

/// A binding pattern: `x`.
#[derive(Debug, Clone)]
pub struct PatternBinding {
    /// The name being bound.
    pub name: String,
    /// The [`BindingId`] allocated for this binding.
    /// Populated during: CST walk.
    pub binding: BindingId,
}

/// A literal pattern: `42`, `"hello"`, `true`.
#[derive(Debug, Clone)]
pub struct PatternLiteral {
    /// The literal value to match against.
    pub value: LitKind,
}

/// A tuple pattern: `(a, b, c)`.
#[derive(Debug, Clone)]
pub struct TuplePattern {
    /// The element patterns.
    pub elements: Vec<PatternId>,
}

/// A field in a struct destructuring pattern: `name: pattern`.
#[derive(Debug, Clone)]
pub struct StructPatternField {
    /// The field name.
    pub name: String,
    /// Span of the field name token.
    pub name_span: SourceSpan,
    /// The pattern for this field. For shorthand `{ x }`, this is a
    /// Binding pattern with the same name.
    pub pattern: PatternId,
}

/// A struct destructuring pattern: `{ x, y: pat }`.
#[derive(Debug, Clone)]
pub struct StructPattern {
    /// The fields being destructured.
    pub fields: Vec<StructPatternField>,
}

/// An array pattern: `[a, b, ..rest]`.
#[derive(Debug, Clone)]
pub struct ArrayPattern {
    /// The element patterns (may include an ArrayRest pattern).
    pub elements: Vec<PatternId>,
}

/// A rest pattern in an array: `..name` or bare `..`.
#[derive(Debug, Clone)]
pub struct ArrayRestPattern {
    /// The binding name for the rest elements. `None` for bare `..`.
    pub name: Option<String>,
    /// The [`BindingId`] for the rest binding, if named.
    /// Populated during: CST walk.
    pub binding: Option<BindingId>,
}

/// A variant pattern: `Some { value }` or bare `None`.
#[derive(Debug, Clone)]
pub struct VariantPattern {
    /// The variant name (e.g. `"Some"`, `"None"`).
    pub name: String,
    /// Span of the variant name.
    pub name_span: SourceSpan,
    /// The resolved target of the variant name. Points to the parent enum's
    /// `ItemId` — VIR does not have a separate VariantId, so the resolver
    /// pairs this ItemId with the variant name to identify the specific variant.
    /// Populated during: name resolution pass.
    pub resolved: Option<ItemId>,
    /// Optional struct destructuring of the variant's fields.
    /// `None` for unit variants like `None`.
    pub fields: Option<Vec<StructPatternField>>,
}

/// An at-pattern: `name @ pattern`.
#[derive(Debug, Clone)]
pub struct AtPattern {
    /// The name being bound to the whole value.
    pub name: String,
    /// The [`BindingId`] for the at-binding.
    /// Populated during: CST walk.
    pub binding: BindingId,
    /// The inner pattern for further destructuring.
    pub inner: PatternId,
}

/// An or-pattern: `pat1 | pat2`.
#[derive(Debug, Clone)]
pub struct OrPattern {
    /// The alternative patterns. At least two.
    pub alternatives: Vec<PatternId>,
}
