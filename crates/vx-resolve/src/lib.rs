//! Name resolution for Vixen source files.
//!
//! Builds a scope tree from a parsed CST and resolves every name reference
//! to its definition site. This is the foundation for go-to-definition,
//! find-references, rename, and type checking.

mod resolve;
mod scope;

pub use resolve::{NameResolution, ResolveError, resolve};
pub use scope::{DefKind, Definition, ScopeId, ScopeKind, ScopeTree};

#[cfg(test)]
mod tests;
