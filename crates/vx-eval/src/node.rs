//! The node state machine — the kernel of the evaluator.
//!
//! Each computation in the dataflow graph is a `Node`. Nodes transition
//! through states: Pending → Evaluating → Resolved | Poisoned.
//!
//! When a node resolves, it wakes all its waiters. When it poisons,
//! it propagates the poison to all waiters.

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::poison::PoisonError;
use crate::value::Value;

/// A unique identifier for a node in the dataflow graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u64);

static NEXT_NODE_ID: AtomicU64 = AtomicU64::new(0);

impl NodeId {
    pub fn next() -> Self {
        Self(NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// The key used to look up nodes in the memo table.
/// For 0.1, this is a function name + serialized arguments.
/// Real code identity comes later.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeKey {
    pub identity: String,
}

/// The outcome of a resolved node — either a value or a poison.
#[derive(Debug, Clone)]
pub enum Outcome {
    Value(Value),
    Poison(PoisonError),
}

/// A waiter handle: when a node resolves, it sends the outcome through
/// this channel to wake the waiting task.
pub struct Waiter {
    pub node_id: NodeId,
    pub sender: crossbeam_channel::Sender<(NodeId, Outcome)>,
}

/// The state of a node in the dataflow graph.
pub enum NodeState {
    /// Evaluation hasn't started yet.
    Pending,

    /// Evaluation is in progress. Other nodes waiting for this result
    /// register themselves here.
    Evaluating { waiters: Vec<Waiter> },

    /// The value is computed and cached.
    Resolved(Value),

    /// The node poisoned.
    Poisoned(PoisonError),
}

/// A node in the dataflow graph.
pub struct Node {
    pub id: NodeId,
    pub state: Mutex<NodeState>,
    /// Human-readable description for the demand chain.
    pub description: String,
}

impl Node {
    pub fn new(description: String) -> Self {
        Self {
            id: NodeId::next(),
            state: Mutex::new(NodeState::Pending),
            description,
        }
    }

    /// Attempt to start evaluating this node.
    /// Returns `Ok(())` if we transitioned from Pending → Evaluating.
    /// Returns `Err(existing_outcome)` if it was already resolved/poisoned.
    /// If it's currently being evaluated by someone else, registers the
    /// waiter and returns `Err(None)` — the caller should park.
    pub fn try_start_or_wait(&self, waiter: Option<Waiter>) -> Result<(), Option<Outcome>> {
        let mut state = self.state.lock();
        match &mut *state {
            NodeState::Pending => {
                *state = NodeState::Evaluating {
                    waiters: Vec::new(),
                };
                Ok(())
            }
            NodeState::Evaluating { waiters } => {
                if let Some(w) = waiter {
                    waiters.push(w);
                }
                Err(None) // caller should park
            }
            NodeState::Resolved(val) => Err(Some(Outcome::Value(val.clone()))),
            NodeState::Poisoned(err) => Err(Some(Outcome::Poison(err.clone()))),
        }
    }

    /// Resolve this node with a value. Wakes all waiters.
    pub fn resolve(&self, value: Value) {
        let waiters = {
            let mut state = self.state.lock();
            let waiters = match &mut *state {
                NodeState::Evaluating { waiters } => std::mem::take(waiters),
                _ => Vec::new(),
            };
            *state = NodeState::Resolved(value.clone());
            waiters
        };
        // Wake all waiters outside the lock
        for waiter in waiters {
            let _ = waiter.sender.send((self.id, Outcome::Value(value.clone())));
        }
    }

    /// Poison this node. Wakes all waiters with the poison.
    pub fn poison(&self, error: PoisonError) {
        let waiters = {
            let mut state = self.state.lock();
            let waiters = match &mut *state {
                NodeState::Evaluating { waiters } => std::mem::take(waiters),
                _ => Vec::new(),
            };
            *state = NodeState::Poisoned(error.clone());
            waiters
        };
        for waiter in waiters {
            let _ = waiter
                .sender
                .send((self.id, Outcome::Poison(error.clone())));
        }
    }

    /// Get the current outcome if already resolved/poisoned.
    pub fn outcome(&self) -> Option<Outcome> {
        let state = self.state.lock();
        match &*state {
            NodeState::Resolved(val) => Some(Outcome::Value(val.clone())),
            NodeState::Poisoned(err) => Some(Outcome::Poison(err.clone())),
            _ => None,
        }
    }
}
