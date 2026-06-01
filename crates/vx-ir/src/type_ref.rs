//! Source-level type annotations.
//!
//! [`TypeRef`] represents type syntax as written by the user (e.g. `Foo[Bar]`,
//! `Fn(A, B) -> C`, `T?`). This is distinct from [`vx_types::Ty`], which is the
//! resolved/checked type. The separation is necessary because type checking needs
//! the source-level syntax to resolve paths, apply generic substitution, and
//! produce diagnostics that reference what the user actually wrote.
//!
//! TypeRefs are arena-allocated with [`TypeRefId`](crate::TypeRefId).

use crate::TypeRefId;
use vx_parser::SourceSpan;

/// A source-level type annotation.
///
/// Populated during: CST walk.
#[derive(Debug, Clone)]
pub struct TypeRef {
    /// The source span of this type annotation.
    pub span: SourceSpan,

    /// The syntactic form of this type reference.
    pub kind: TypeRefKind,
}

/// The syntactic form of a type annotation.
#[derive(Debug, Clone)]
pub enum TypeRefKind {
    /// A path type: `Foo`, `Foo.Bar`, `Foo[T, U]`.
    Path(PathTypeRef),

    /// A function type: `Fn(A, B) -> C`.
    Function(FunctionTypeRef),

    /// A tuple type: `(A, B, C)`.
    Tuple(TupleTypeRef),

    /// A sum type: `A | B | C`.
    Sum(SumTypeRef),

    /// An error recovery node. Produced when the CST contains a broken type annotation.
    Error,
}

/// A path type reference: `Foo`, `Foo.Bar`, `Foo[T, U]`.
#[derive(Debug, Clone)]
pub struct PathTypeRef {
    /// The path segments (e.g. `["Foo", "Bar"]` for `Foo.Bar`).
    pub segments: Vec<String>,

    /// Generic type arguments, if any (e.g. `[T, U]` in `Foo[T, U]`).
    pub type_args: Vec<TypeRefId>,
}

/// A function type reference: `Fn(A, B) -> C`.
#[derive(Debug, Clone)]
pub struct FunctionTypeRef {
    /// The parameter types.
    pub params: Vec<TypeRefId>,

    /// The return type.
    pub ret: TypeRefId,

    /// Capability tags on this function type (e.g. `#fs`, `#discover`).
    pub capabilities: Vec<crate::body::CapabilityTag>,
}

/// A tuple type reference: `(A, B, C)`.
#[derive(Debug, Clone)]
pub struct TupleTypeRef {
    /// The element types.
    pub elements: Vec<TypeRefId>,
}

/// A sum type reference: `A | B | C`.
#[derive(Debug, Clone)]
pub struct SumTypeRef {
    /// The alternative types (at least two).
    pub alternatives: Vec<TypeRefId>,
}
