//! Identifier types for all VIR nodes.
//!
//! Each ID is a newtype over `u32`, allocated sequentially during the CST-to-VIR
//! walk. IDs index into the corresponding `Vec` in [`VirArena`](crate::VirArena).

/// Defines a newtype ID with standard derives, indexing helpers, and Display.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, facet::Facet)]
        pub struct $name(pub u32);

        impl $name {
            /// Convert to a `usize` for indexing into a `Vec`.
            pub fn index(self) -> usize {
                self.0 as usize
            }

            /// Create from a `usize` index.
            pub fn from_index(index: usize) -> Self {
                Self(index as u32)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

define_id! {
    /// Identifies an expression node in the VIR arena.
    ExprId
}

define_id! {
    /// Identifies a statement node in the VIR arena.
    StmtId
}

define_id! {
    /// Identifies a pattern node in the VIR arena.
    PatternId
}

define_id! {
    /// Identifies a binding — a named slot introduced by a let, parameter,
    /// pattern binder, `@`-pattern, or array rest pattern.
    ///
    /// Every binder site in syntax creates exactly one `BindingId`. Name
    /// references resolve to a `BindingId` (for locals) or a `DefId` (for
    /// top-level definitions).
    BindingId
}

define_id! {
    /// Identifies a body — a scope-owning unit (function, closure, or
    /// top-level thunk). Each body has its own parameter list and a root
    /// expression.
    BodyId
}

define_id! {
    /// Identifies a top-level declaration item (function, struct, enum,
    /// newtype, type alias, or namespace).
    ItemId
}

define_id! {
    /// Identifies a lexical scope.
    ///
    /// Scopes form a tree via `parent` pointers. Every construct that introduces
    /// a new lexical scope (function body, block, match arm, namespace) gets a
    /// `ScopeId`. Bindings are registered in the scope that introduces them.
    ScopeId
}

define_id! {
    /// Identifies a source-level type annotation (e.g. `Foo[Bar]`, `Fn(A) -> B`).
    ///
    /// Type annotations are arena-allocated because they appear in many positions
    /// (parameters, return types, let annotations, type operations) and may need
    /// to be visited during type resolution.
    TypeRefId
}

define_id! {
    /// Identifies a source file's VIR root.
    ///
    /// Each parsed source file becomes one `SourceUnit` in the arena, which holds
    /// the file's top-level items and its implicit top-level body.
    SourceUnitId
}
