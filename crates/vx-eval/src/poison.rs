//! Poison propagation.
//!
//! Poison is a value state, not an exception. When a node poisons, it
//! wakes all its waiters with the poison, and they propagate it to their
//! waiters in turn. Independent subgraphs are unaffected.

use std::fmt;

use crate::node::NodeId;

/// The error carried by a poisoned node.
#[derive(Debug, Clone)]
pub struct PoisonError {
    pub kind: PoisonKind,
    pub message: String,
    /// The chain of demands that led to this poison — who demanded what.
    /// Most recent demand is last.
    pub demand_chain: DemandChain,
}

/// What kind of failure caused the poison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoisonKind {
    /// A type error (wrong types for an operation).
    TypeError,
    /// An unresolved name reference.
    UnresolvedName,
    /// Division by zero or similar arithmetic failure.
    ArithmeticError,
    /// A capability violation (e.g., calling #fs without the capability).
    CapabilityViolation,
    /// An explicit `fail` in user code.
    UserFail,
    /// A verification failure from an `exec` verifier.
    VerificationFailure,
    /// Circular dependency detected.
    CircularDependency,
    /// Internal evaluator error (bug).
    Internal,
}

/// The demand chain: a trail of NodeIds showing who demanded what.
/// Used for debugging — this is Vixen's replacement for stack traces.
#[derive(Debug, Clone, Default)]
pub struct DemandChain {
    pub entries: Vec<DemandEntry>,
}

/// One entry in the demand chain.
#[derive(Debug, Clone)]
pub struct DemandEntry {
    pub node_id: NodeId,
    pub description: String,
}

impl DemandChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, node_id: NodeId, description: String) {
        self.entries.push(DemandEntry {
            node_id,
            description,
        });
    }
}

impl fmt::Display for PoisonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        if !self.demand_chain.entries.is_empty() {
            write!(f, "\n  demand chain:")?;
            for entry in &self.demand_chain.entries {
                write!(f, "\n    -> {}", entry.description)?;
            }
        }
        Ok(())
    }
}
