//! Source identification, spans, and the import graph for multi-file programs.

use std::collections::HashMap;

use rowan::{TextRange, TextSize};

use crate::SourceFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SourceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SourceSpan {
    pub source: SourceId,
    pub range: TextRange,
}

impl SourceSpan {
    pub fn new(source: SourceId, range: TextRange) -> Self {
        Self { source, range }
    }

    pub fn start(self) -> TextSize {
        self.range.start()
    }

    pub fn end(self) -> TextSize {
        self.range.end()
    }

    pub fn contains_inclusive(self, offset: TextSize) -> bool {
        self.range.contains_inclusive(offset)
    }
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
