//! Source file roots.
//!
//! Each parsed source file becomes a [`SourceUnit`] in the VIR arena. This
//! provides a concrete per-file root so that "what items belong to this file?"
//! is a direct lookup, not a scan-and-filter-by-span operation.

use crate::{BodyId, ItemId};
use vx_parser::SourceId;

/// A source file's VIR root.
///
/// Populated during: CST walk.
#[derive(Debug, Clone)]
pub struct SourceUnit {
    /// The source file this unit represents.
    pub source: SourceId,

    /// The top-level items declared in this file, in source order.
    pub items: Vec<ItemId>,

    /// The implicit top-level body that wraps all top-level statements.
    /// This body's root expression is a synthetic block containing the
    /// file's top-level code.
    pub top_level_body: BodyId,
}
