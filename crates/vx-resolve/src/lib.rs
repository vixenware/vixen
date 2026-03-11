//! Name resolution for Vixen source files.
//!
//! Builds a scope tree from a parsed CST and resolves every name reference
//! to its definition site. This is the foundation for go-to-definition,
//! find-references, rename, and type checking.

mod cap_check;
pub use cap_check::{CapCheckResult, CapError, CapErrorKind, CapSet, check_capabilities};

pub mod builtins;

mod stdlib;
pub use stdlib::{ImportGraph, SourceFileRef, SourceId, source_disk_path, stdlib_sources};

mod resolve;
pub use resolve::{NameResolution, ResolveError, resolve, resolve_in_program};

mod scope;
pub use scope::{
    DefKind, DefSite, Definition, ProgramScopes, ScopeId, ScopeKind, ScopeTree,
    top_level_definitions,
};

mod type_collect;
pub use type_collect::{TypeCollection, collect_types, collect_types_for_sources};

mod type_check;
pub use type_check::{TypeCheckResult, TypeError, TypeErrorKind, type_check};

#[cfg(test)]
mod tests;
