//! Source-location primitives, shared across the compiler and **rowan-free**.
//!
//! The CST (rowan) owns `TextRange`/`TextSize`; the parser converts those into these
//! at the CST→VIR boundary. Everything above the parser — the VIR, diagnostics — carries
//! spans as plain `u32` byte offsets, so the IR stays decoupled from the CST library and
//! the whole IR can derive `Facet` for over-the-wire shipping.

use facet::Facet;

/// Identifies a source file. Allocated by the parser as files are loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Facet)]
pub struct SourceId(pub u32);

/// A half-open byte span `[start, end)` within a source file.
///
/// Byte offsets are plain `u32` (no rowan `TextSize`), so a span ships and the IR that
/// carries it never reaches for the CST library. The parser builds these from rowan
/// `TextRange`s; consumers that need a rowan range reconstruct it from the offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Facet)]
pub struct SourceSpan {
    /// The source file this span points into.
    pub source: SourceId,
    /// Start byte offset, inclusive.
    pub start: u32,
    /// End byte offset, exclusive.
    pub end: u32,
}

impl SourceSpan {
    pub fn new(source: SourceId, start: u32, end: u32) -> Self {
        Self { source, start, end }
    }

    /// Start byte offset. (Method form kept so existing `.start()` call sites survive.)
    pub fn start(self) -> u32 {
        self.start
    }

    /// End byte offset.
    pub fn end(self) -> u32 {
        self.end
    }

    /// Span length in bytes.
    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers zero bytes.
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// Whether `offset` lies within `[start, end]` — inclusive of `end`, matching the
    /// rowan `contains_inclusive` semantics the parser relied on.
    pub fn contains_inclusive(self, offset: u32) -> bool {
        self.start <= offset && offset <= self.end
    }
}
