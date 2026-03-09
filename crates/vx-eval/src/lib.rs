//! Demand-driven dataflow evaluator for the Vixen language.
//!
//! This crate implements Vixen's evaluation model: a dependency graph of
//! computation nodes that resolve on demand. Nothing blocks — when a node
//! needs a value that isn't ready, it parks itself and resumes when the
//! dependency resolves.

mod capability;
mod instr;
mod lower;
mod node;
mod poison;
mod value;
mod worker;

pub use capability::CapabilitySet;
pub use instr::{BinOp, Expr, FnDef, Module, Stmt, UnaryOp};
pub use node::{NodeId, NodeKey};
pub use poison::{DemandChain, PoisonError, PoisonKind};
pub use value::Value;
pub use worker::Engine;

#[cfg(test)]
mod tests;
