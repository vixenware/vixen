//! Source identification, spans, and the import graph for multi-file programs.

use std::collections::HashMap;

use rowan::TextRange;

use crate::SourceFile;

/// Re-exported from `vx-span` so existing `vx_parser::{SourceId, SourceSpan}` paths keep
/// resolving. Now rowan-free (u32 byte offsets); build one from a rowan range with
/// [`span_from_range`].
pub use vx_span::{SourceId, SourceSpan};

/// Convert a rowan `TextRange` into a [`SourceSpan`] — the CST→span boundary conversion.
pub fn span_from_range(source: SourceId, range: TextRange) -> SourceSpan {
    SourceSpan::new(source, range.start().into(), range.end().into())
}

#[derive(Debug, Clone)]
pub struct SourceFileRef {
    pub id: SourceId,
    pub file: SourceFile,
}

#[derive(Debug, Clone, Default)]
pub struct ImportGraph {
    edges: HashMap<SourceId, Vec<SourceId>>,
}

impl ImportGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_import(&mut self, from: SourceId, to: SourceId) {
        self.edges.entry(from).or_default().push(to);
    }

    pub fn imports_from(&self, source: SourceId) -> &[SourceId] {
        self.edges.get(&source).map(Vec::as_slice).unwrap_or(&[])
    }
}
