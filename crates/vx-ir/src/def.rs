//! Semantic definitions: kinds, IDs, sites, and the `Definition` record.
//!
//! A `Definition` captures everything the rest of the pipeline needs about a
//! named declaration site: its identity, name, kind, and source spans.

use vx_parser::{SourceId, SourceSpan};

/// What kind of definition a name refers to.
#[derive(facet::Facet, Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DefKind {
    /// `fn name(...) { ... }`
    Function,
    /// `namespace Name { ... }`
    Namespace,
    /// `struct Name { ... }`
    Struct,
    /// `enum Name { ... }`
    Enum,
    /// `newtype Name of T`
    Newtype,
    /// `type Name = ...`
    TypeAlias,
    /// `let name = ...` or pattern binding
    Binding,
    /// Function parameter
    Parameter,
    /// Enum variant (accessed as `Enum.Variant`)
    Variant,
    /// Struct field
    Field,
}

pub type DefSite = SourceSpan;

#[derive(facet::Facet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DefId {
    Source { source: SourceId, local: u32 },
}

/// A name definition in a scope.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// Stable identity for this definition.
    pub id: DefId,
    /// The name being defined.
    pub name: String,
    /// What kind of definition this is.
    pub kind: DefKind,
    /// Text range of the name token (for go-to-definition).
    pub name_span: SourceSpan,
    /// Text range of the entire declaration (for highlight).
    pub full_span: SourceSpan,
}

impl Definition {
    pub fn source(&self) -> SourceId {
        self.name_span.source
    }

    pub fn site(&self) -> DefSite {
        self.name_span
    }
}
