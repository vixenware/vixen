//! `SourceSpan` -> IR node lookup.
//!
//! The LSP receives queries at a source position. To answer them with the IR
//! it needs to find the IR node that came from a given span. This module owns
//! that lookup.
//!
//! Build it once after the arena is fully constructed; later semantic passes
//! mutate fields on existing nodes (e.g. `Expr.ty`) but don't add new ones,
//! so the index stays valid.

use std::collections::HashMap;

use vx_parser::SourceSpan;

use crate::arena::VirArena;
use crate::expr::ExprKind;
use crate::{BindingId, ExprId, ItemId, PatternId, TypeRefId};

/// `SourceSpan` -> IR node lookup tables.
#[derive(Debug, Default, Clone)]
pub struct SpanIndex {
    /// All expressions at a given span. Multiple entries are possible when
    /// the CST walk emits synthesized nodes that share the user-visible span
    /// of the surrounding expression. Stored in arena (allocation) order.
    exprs: HashMap<SourceSpan, Vec<ExprId>>,
    /// Bindings keyed by their declaration `name_span`.
    bindings_by_name_span: HashMap<SourceSpan, BindingId>,
    /// Items keyed by their full `span` (the whole declaration).
    items_by_span: HashMap<SourceSpan, ItemId>,
    /// Items keyed by their `name_span` (just the identifier token).
    items_by_name_span: HashMap<SourceSpan, ItemId>,
    /// Patterns keyed by their span.
    patterns: HashMap<SourceSpan, PatternId>,
    /// Type references keyed by their span.
    type_refs: HashMap<SourceSpan, TypeRefId>,
}

impl SpanIndex {
    /// Walk every node in the arena and populate the lookup tables.
    pub fn build(arena: &VirArena) -> Self {
        let mut idx = Self::default();

        for (i, expr) in arena.exprs.iter().enumerate() {
            idx.exprs
                .entry(expr.span)
                .or_default()
                .push(ExprId::from_index(i));
        }
        for (i, binding) in arena.bindings.iter().enumerate() {
            idx.bindings_by_name_span
                .insert(binding.name_span, BindingId::from_index(i));
        }
        for (i, item) in arena.items.iter().enumerate() {
            let id = ItemId::from_index(i);
            idx.items_by_span.insert(item.span, id);
            if let Some(name_span) = item.name_span() {
                idx.items_by_name_span.insert(name_span, id);
            }
        }
        for (i, pat) in arena.patterns.iter().enumerate() {
            idx.patterns.insert(pat.span, PatternId::from_index(i));
        }
        for (i, ty) in arena.type_refs.iter().enumerate() {
            idx.type_refs.insert(ty.span, TypeRefId::from_index(i));
        }

        idx
    }

    /// All expressions whose span exactly matches.
    pub fn exprs_at(&self, span: SourceSpan) -> &[ExprId] {
        self.exprs.get(&span).map(Vec::as_slice).unwrap_or(&[])
    }

    /// First expression at a span (most common case — outermost-allocated).
    pub fn expr_at(&self, span: SourceSpan) -> Option<ExprId> {
        self.exprs.get(&span).and_then(|v| v.first().copied())
    }

    /// The `NameRef` expression at this span, if any. Filters by kind so
    /// callers can answer "what name is being referenced here?" directly.
    pub fn name_ref_at(&self, span: SourceSpan, arena: &VirArena) -> Option<ExprId> {
        self.exprs
            .get(&span)?
            .iter()
            .copied()
            .find(|id| matches!(arena[*id].kind, ExprKind::NameRef(_)))
    }

    /// Binding declared at this `name_span`.
    pub fn binding_at_name_span(&self, span: SourceSpan) -> Option<BindingId> {
        self.bindings_by_name_span.get(&span).copied()
    }

    /// Item whose full span matches.
    pub fn item_at(&self, span: SourceSpan) -> Option<ItemId> {
        self.items_by_span.get(&span).copied()
    }

    /// Item whose name token span matches.
    pub fn item_at_name_span(&self, span: SourceSpan) -> Option<ItemId> {
        self.items_by_name_span.get(&span).copied()
    }

    /// Pattern at this span.
    pub fn pattern_at(&self, span: SourceSpan) -> Option<PatternId> {
        self.patterns.get(&span).copied()
    }

    /// Type reference at this span.
    pub fn type_ref_at(&self, span: SourceSpan) -> Option<TypeRefId> {
        self.type_refs.get(&span).copied()
    }
}
