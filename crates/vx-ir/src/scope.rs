//! Lexical scopes and name lookup.
//!
//! Scopes form a tree via `parent` pointers. Every construct that introduces
//! new name visibility gets a [`Scope`] with a unique [`ScopeId`](crate::ScopeId):
//! module roots, function bodies, blocks, match arms, and namespaces.
//!
//! Each scope holds a unified list of [`ScopeEntry`] values — both local
//! bindings and item definitions in a single stream. Visibility depends on
//! scope kind and declaration order (see [`lookup_at`]).

use crate::arena::VirArena;
use crate::{BindingId, BodyId, ExprId, ItemId, ScopeId, SourceUnitId};
use vx_parser::SourceSpan;

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// A lexical scope in the VIR.
///
/// Populated during: CST walk (bindings) and scope-building pass (item entries).
#[derive(Debug, Clone)]
pub struct Scope {
    /// The parent scope, or `None` for the root module scope.
    pub parent: Option<ScopeId>,

    /// What syntactic construct owns this scope.
    pub owner: ScopeOwner,

    /// All named entries in this scope — bindings and item definitions
    /// in a single list. NOT necessarily in `decl_order` order (predeclared
    /// items may be pushed before later bindings). Use `decl_order` for
    /// visibility checks, not vec index.
    pub entries: Vec<ScopeEntry>,

    /// Enum variants promoted to this scope (legacy Option compatibility).
    /// Always visible regardless of ordering. Regular entries beat promoted
    /// variants — `lookup_at` checks `entries` first.
    pub promoted_variants: Vec<PromotedVariant>,

    /// When climbing from this scope to its parent during lookup, only
    /// parent entries with `decl_order <= visibility_cutoff` (for items) or
    /// `decl_order < visibility_cutoff` (for bindings) are visible.
    ///
    /// This is the `decl_order` of the construct that introduced this scope
    /// in the parent block.
    ///
    /// `None` for root/module scopes and for scopes whose parent uses
    /// order-independent visibility (module, namespace).
    pub visibility_cutoff: Option<u32>,
}

/// What syntactic construct introduced this scope.
#[derive(Debug, Clone)]
pub enum ScopeOwner {
    /// The per-file module scope. Contains only item definitions
    /// (order-independent). Top-level function bodies parent to this scope.
    ///
    /// The top-level thunk's block scope is a *child* of this scope, so
    /// top-level `let` bindings live one level below module items.
    /// This is a deliberate semantic carve-out: top-level bindings always
    /// shadow top-level items (regardless of source order) because the
    /// thunk block is checked before the module scope during parent-chain
    /// lookup. This matches the old scope builder's behavior.
    Module(SourceUnitId),

    /// A function or closure body.
    Body(BodyId),

    /// A block expression: `( ... )`.
    Block(ExprId),

    /// A match arm (the pattern introduces bindings visible in the arm body).
    MatchArm {
        /// The match expression this arm belongs to.
        match_expr: ExprId,
        /// The index of this arm within the match expression's arm list.
        arm_index: u32,
    },

    /// A namespace declaration.
    Namespace(ItemId),
}

// ---------------------------------------------------------------------------
// Scope entries
// ---------------------------------------------------------------------------

/// What a scope entry points to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeEntryTarget {
    /// A local binding (let, parameter, pattern match, etc.).
    Binding(BindingId),
    /// An item definition (fn, struct, enum, newtype, type alias, namespace).
    Item(ItemId),
}

/// A single named entry in a scope — either a local binding or an item
/// definition. Bindings and items share a namespace in Vixen, so they live
/// in one unified list for correct shadowing.
#[derive(Debug, Clone)]
pub struct ScopeEntry {
    /// What this entry points to.
    pub target: ScopeEntryTarget,

    /// Source-order position of this declaration among its scope's children.
    ///
    /// - Module/namespace scopes: position in the interleaved top-level child list.
    /// - Block scopes: position in the statement list.
    /// - Body scopes: parameter index.
    ///
    /// Used by [`lookup_at`] for sequential visibility checks.
    pub decl_order: u32,
}

impl ScopeEntry {
    /// Get the name of this entry by looking up the binding or item in the arena.
    pub fn name<'a>(&self, arena: &'a VirArena) -> &'a str {
        match self.target {
            ScopeEntryTarget::Binding(id) => &arena[id].name,
            ScopeEntryTarget::Item(id) => arena.item_name(id),
        }
    }
}

// ---------------------------------------------------------------------------
// Promoted variants (legacy Option compatibility)
// ---------------------------------------------------------------------------

/// An enum variant promoted to its parent scope.
///
/// This is a **legacy compatibility hack** for Vixen's `Option` type. It is
/// NOT a general property of enum scopes. The `Option` enum's `Some` and
/// `None` variants are promoted so they can be used without qualification.
///
/// When/if Vixen gets a proper prelude or import system, this should be
/// replaced with explicit imports.
#[derive(Debug, Clone)]
pub struct PromotedVariant {
    /// The variant name (e.g. `"Some"`, `"None"`).
    pub name: String,
    /// Span of the variant name in source.
    pub name_span: SourceSpan,
    /// The parent enum's ItemId.
    pub enum_item: ItemId,
    /// Index of this variant within the enum's variants vec.
    pub variant_index: u32,
}

// ---------------------------------------------------------------------------
// Lookup result
// ---------------------------------------------------------------------------

/// The result of name lookup — what a name resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefTarget {
    /// A local binding (let, parameter, match pattern, etc.).
    Binding(BindingId),
    /// An item definition (fn, struct, enum, etc.).
    Item(ItemId),
    /// A promoted enum variant (legacy Option compatibility).
    Variant {
        /// The parent enum's ItemId.
        enum_item: ItemId,
        /// Index of this variant within the enum's variants vec.
        variant_index: u32,
    },
}

// ---------------------------------------------------------------------------
// Name lookup
// ---------------------------------------------------------------------------

/// Look up a name starting from a scope, walking up the parent chain.
///
/// `reference_order` is the position of the reference site within `from`.
/// The resolver (003) computes this during its top-down walk:
/// - Block scope: statement index of the containing statement.
/// - Block tail expression: `stmt_count` (after all statements).
/// - Let initializer: the let's own `decl_order` (makes binding invisible
///   in its own init via the `<` check).
/// - Function/closure body scope: `u32::MAX` (all params visible).
/// - Match arm scope: `u32::MAX` (all pattern bindings visible).
/// - Module scope: `u32::MAX` (all items visible).
///
/// Within a scope, the **latest visible** entry wins (highest `decl_order`).
/// This gives correct shadowing when a binding and item share a name.
///
/// When climbing to a parent scope, the child scope's `visibility_cutoff`
/// replaces `reference_order`. This ensures nested functions/closures only
/// see parent bindings declared before the function/closure.
///
/// Items use `decl_order <= cutoff` (self-recursion for block-local functions).
/// Bindings use `decl_order < cutoff` (not visible in own initializer).
pub fn lookup_at(
    arena: &VirArena,
    name: &str,
    from: ScopeId,
    reference_order: u32,
) -> Option<DefTarget> {
    let mut scope_id = from;
    let mut cutoff = reference_order;

    loop {
        let scope = &arena[scope_id];
        let mut best: Option<&ScopeEntry> = None;

        for entry in &scope.entries {
            if entry.name(arena) != name {
                continue;
            }

            let visible = match &scope.owner {
                // Module/Namespace: all items always visible (order-independent).
                // Module scopes should not contain bindings. Namespace scopes
                // should not contain bindings either.
                ScopeOwner::Module(_) | ScopeOwner::Namespace(_) => true,

                // Block: items at decl_order <= cutoff (enables self-recursion),
                //        bindings at decl_order < cutoff (not visible in own init).
                ScopeOwner::Block(_) => match entry.target {
                    ScopeEntryTarget::Item(_) => entry.decl_order <= cutoff,
                    ScopeEntryTarget::Binding(_) => entry.decl_order < cutoff,
                },

                // Function/Closure/MatchArm body: all params always visible.
                ScopeOwner::Body(_) | ScopeOwner::MatchArm { .. } => true,
            };

            if visible {
                // Keep the LATEST visible entry (highest decl_order).
                match best {
                    None => best = Some(entry),
                    Some(prev) if entry.decl_order > prev.decl_order => best = Some(entry),
                    _ => {}
                }
            }
        }

        if let Some(entry) = best {
            return Some(match entry.target {
                ScopeEntryTarget::Binding(id) => DefTarget::Binding(id),
                ScopeEntryTarget::Item(id) => DefTarget::Item(id),
            });
        }

        // Regular entries beat promoted variants — only check these after
        // entries have been exhausted for this scope.
        for pv in &scope.promoted_variants {
            if pv.name == name {
                return Some(DefTarget::Variant {
                    enum_item: pv.enum_item,
                    variant_index: pv.variant_index,
                });
            }
        }

        // Climb to parent scope.
        let parent = scope.parent?;
        cutoff = scope.visibility_cutoff.unwrap_or(u32::MAX);
        scope_id = parent;
    }
}
