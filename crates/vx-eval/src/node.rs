//! The node state machine for demand-driven evaluation.
//!
//! Nodes represent lazy computations. They transition through
//! Pending -> Evaluating -> Resolved | Poisoned. Waiters can subscribe to an
//! in-flight node and wake up when it finishes.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::instr::{self, BinOp, UnaryOp};
use crate::poison::PoisonError;
use crate::value::{FunctionValue, Value};

#[derive(Debug, Clone, Copy)]
pub enum TemplateKind {
    String,
    Path,
}

#[derive(Debug, Clone)]
pub enum TemplatePartValue {
    Text(Arc<str>),
    Expr(Value),
}

/// A unique identifier for a node in the dataflow graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u64);

static NEXT_NODE_ID: AtomicU64 = AtomicU64::new(0);

impl NodeId {
    pub fn next() -> Self {
        Self(NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed))
    }
}

// r[impl eval.memoization.pending]
/// The key used to look up memoized nodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeKey {
    pub identity: String,
}

// r[impl eval.demand]
/// A lazy computation that can produce a `Value`.
#[derive(Debug, Clone)]
pub enum NodeJob {
    Binary {
        op: BinOp,
        left: Value,
        right: Value,
    },
    Unary {
        op: UnaryOp,
        operand: Value,
    },
    Template {
        kind: TemplateKind,
        parts: Arc<[TemplatePartValue]>,
    },
    If {
        cond: Value,
        then_branch: instr::Block,
        else_branch: instr::Block,
        bindings: Arc<HashMap<String, Value>>,
        scope_identity: Arc<str>,
    },
    Call {
        function: Value,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
        kwargs_expr: Option<Value>,
    },
    Invoke {
        function: FunctionValue,
        args: Arc<[Value]>,
    },
    Field {
        receiver: Value,
        field: Arc<str>,
    },
    Fail {
        message: Value,
    },
    #[cfg(test)]
    Sleep {
        millis: u64,
        value: Value,
    },
    #[cfg(test)]
    Poison(PoisonError),
}

impl NodeJob {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pending_deps(&self) -> Vec<NodeId> {
        let mut deps = Vec::new();
        match self {
            NodeJob::Binary { left, right, .. } => {
                left.collect_pending(&mut deps);
                right.collect_pending(&mut deps);
            }
            NodeJob::Unary { operand, .. } => operand.collect_pending(&mut deps),
            NodeJob::Template { parts, .. } => {
                for part in parts.iter() {
                    if let TemplatePartValue::Expr(value) = part {
                        value.collect_pending(&mut deps);
                    }
                }
            }
            NodeJob::If { cond, .. } => cond.collect_pending(&mut deps),
            NodeJob::Call {
                function,
                args,
                kwargs,
                kwargs_expr,
            } => {
                function.collect_pending(&mut deps);
                for arg in args.iter() {
                    arg.collect_pending(&mut deps);
                }
                for (_, value) in kwargs.iter() {
                    value.collect_pending(&mut deps);
                }
                if let Some(kwargs_expr) = kwargs_expr {
                    kwargs_expr.collect_pending(&mut deps);
                }
            }
            NodeJob::Invoke { function, args } => {
                Value::Function(function.clone()).collect_pending(&mut deps);
                for arg in args.iter() {
                    arg.collect_pending(&mut deps);
                }
            }
            NodeJob::Field { receiver, .. } => receiver.collect_pending(&mut deps),
            NodeJob::Fail { message } => message.collect_pending(&mut deps),
            #[cfg(test)]
            NodeJob::Sleep { value, .. } => value.collect_pending(&mut deps),
            #[cfg(test)]
            NodeJob::Poison(_) => {}
        }
        deps
    }
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
    pub sender: crossbeam_channel::Sender<(NodeId, Outcome)>,
}

// i[impl impl.persistent]
/// The state of a node in the dataflow graph.
pub enum NodeState {
    /// Evaluation hasn't started yet.
    Pending,

    /// Evaluation is in progress. Other nodes waiting for this result
    /// register themselves here.
    Evaluating { waiters: Vec<Waiter> },

    /// The value graph is built and cached.
    Resolved(Value),

    /// The node poisoned.
    Poisoned(PoisonError),
}

/// A node in the dataflow graph.
pub struct Node {
    pub id: NodeId,
    pub state: Mutex<NodeState>,
    pub job: NodeJob,
    /// Human-readable description for diagnostics.
    pub description: String,
    pub provenance: Option<instr::SourceProvenance>,
}

impl Node {
    pub fn new(
        job: NodeJob,
        description: String,
        provenance: Option<instr::SourceProvenance>,
    ) -> Self {
        Self {
            id: NodeId::next(),
            state: Mutex::new(NodeState::Pending),
            job,
            description,
            provenance,
        }
    }

    /// Attempt to start evaluating this node.
    /// Returns `Ok(())` if we transitioned from Pending -> Evaluating.
    /// Returns `Err(existing_outcome)` if it was already resolved/poisoned.
    /// If it's currently being evaluated by someone else, registers the
    /// waiter and returns `Err(None)` — the caller should wait.
    pub fn try_start_or_wait(&self, waiter: Waiter) -> Result<(), Option<Outcome>> {
        let mut state = self.state.lock();
        match &mut *state {
            NodeState::Pending => {
                *state = NodeState::Evaluating {
                    waiters: Vec::new(),
                };
                Ok(())
            }
            NodeState::Evaluating { waiters } => {
                waiters.push(waiter);
                Err(None)
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
}
