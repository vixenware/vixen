//! Evaluation result state.

use std::sync::Arc;

use crate::{Provenance, Value};

/// A successful value, a pending computation, or a poison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Ready(Value),
    Pending(PendingRef),
    Waiting,
    Poison(EvalError),
}

/// A lazily computed result that is not ready yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingRef {
    pub node_id: u64,
    pub description: Arc<str>,
}

impl PendingRef {
    pub fn new(node_id: u64, description: impl Into<Arc<str>>) -> Self {
        Self {
            node_id,
            description: description.into(),
        }
    }
}

/// A failing evaluation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    pub kind: EvalErrorKind,
    pub message: Arc<str>,
    pub detail: Option<Value>,
    pub demand_chain: Arc<[DemandFrame]>,
}

impl EvalError {
    pub fn new(kind: EvalErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            demand_chain: Arc::from([]),
        }
    }

    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }

    pub fn with_demand_frame(mut self, frame: DemandFrame) -> Self {
        let mut frames = self.demand_chain.iter().cloned().collect::<Vec<_>>();
        frames.push(frame);
        self.demand_chain = Arc::from(frames);
        self
    }
}

/// What kind of failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvalErrorKind {
    TypeError,
    CapabilityViolation,
    VerificationFailure,
    UserFail,
    Internal,
}

/// One frame in the demand chain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DemandFrame {
    pub description: Arc<str>,
    pub provenance: Option<Provenance>,
}

impl DemandFrame {
    pub fn new(description: impl Into<Arc<str>>) -> Self {
        Self {
            description: description.into(),
            provenance: None,
        }
    }

    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = Some(provenance);
        self
    }
}
