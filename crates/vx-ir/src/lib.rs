//! Vixen Intermediate Representation (VIR).
//!
//! VIR is the typed, scoped, identified IR that sits between parsing (CST) and
//! lowering (KernelOp bytecode). It replaces the old bridge/analysis pipeline's
//! span-based reconstruction with a proper tree-structured representation where:
//!
//! - Parent-child relationships are **explicit** (IDs reference other IDs)
//! - Types live **on expressions** (`Expr.ty`), not in a span-keyed HashMap
//! - Name resolution lives **on references** (`NameRefExpr.resolved`)
//! - Scopes are **modeled explicitly** (ScopeId + Scope)
//! - No span joins, no spatial containment queries, no flat enumeration
//!
//! ## Architecture
//!
//! VIR is built by a single CST-to-VIR walk (work item 001). Later passes
//! populate additional fields:
//!
//! | Pass | Populates |
//! |------|-----------|
//! | CST walk | All structural fields: kind, span, children, bindings, scopes |
//! | Name resolution | `NameRefExpr.resolved`, `AssignStmt.resolved` |
//! | Type checking | `Expr.ty`, `Pattern.ty`, `Binding.ty` |
//!
//! ## Crate contents
//!
//! This crate contains the VIR data types and the CST-to-VIR builder.
//! Name resolution and type checking passes live elsewhere.

pub mod ids;

pub mod arena;
pub mod binding;
pub mod body;
pub mod build;
pub mod bundle;
pub mod capture;
pub mod compile;
pub mod def;
pub mod diagnostic;
pub mod diagnostic_report;
pub mod error_union;
pub mod expr;
pub mod item;
pub mod pattern;
pub mod resolve;
pub mod scope;
pub mod source_unit;
pub mod span_index;
pub mod stmt;
pub mod type_check;
pub mod type_collect;
pub mod type_collection;
pub mod type_ref;
pub mod visitor;

// Re-export all ID types at crate root for convenience.
pub use ids::*;

// Re-export the arena and visitor trait.
pub use arena::VirArena;
pub use visitor::VirVisitor;

// The exec bundle: the static reachable closure of an observer body.
pub use bundle::{Bundle, reachable};

// Re-export moved semantic types so external users can write `vx_ir::Definition`
// etc. instead of `vx_ir::def::Definition`.
pub use def::{DefId, DefKind, DefSite, Definition};
pub use diagnostic::{
    CliDiagnosticKind, CliHoleExpect, IrDiagnostic, MethodCandidate, RelatedKind, RelatedSpan,
};
pub use type_collection::{TypeCollection, TypeKind};
