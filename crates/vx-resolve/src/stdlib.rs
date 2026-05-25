//! Real stdlib source files that are implicitly imported by user code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rowan::{TextRange, TextSize};
use vx_parser::SourceFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy)]
pub struct StdlibSource {
    pub logical_path: &'static str,
    pub text: &'static str,
}

const STD_CORE: StdlibSource = StdlibSource {
    // TODO: what is the logical path meant to be exactly?
    logical_path: "packages/std/core.vx",
    text: include_str!("../../packages/std/core.vx"),
};

const STD_BUILD: StdlibSource = StdlibSource {
    logical_path: "packages/std/build.vx",
    text: include_str!("../../packages/std/build.vx"),
};

pub fn stdlib_sources() -> &'static [StdlibSource; 2] {
    &[STD_CORE, STD_BUILD]
}

pub fn source_disk_path(relative: &str) -> PathBuf {
    workspace_root().join(relative)
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
}
