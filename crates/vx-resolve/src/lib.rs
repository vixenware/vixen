//! Name resolution for Vixen source files.
//!
//! Builds a scope tree from a parsed CST and resolves every name reference
//! to its definition site. This is the foundation for go-to-definition,
//! find-references, rename, and type checking.

mod cap_check;
pub use cap_check::{CapCheckResult, CapError, CapErrorKind, CapSet, check_capabilities};

mod resolve;
pub use resolve::{NameResolution, ResolveError, resolve};

mod scope;
pub use scope::{DefKind, Definition, ScopeId, ScopeKind, ScopeTree};

mod type_collect;
pub use type_collect::{TypeCollection, collect_types};

#[cfg(test)]
mod tests;
