//! Execution trace events for eval2.

use std::sync::Arc;

use crate::{CrossingPolicy, EvalErrorKind, ReuseClass, SourceSpan};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EvalNodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvalExprKind {
    Block,
    Literal,
    Paren,
    NameRef,
    Call,
    Array,
    Map,
    Fail,
    If,
    StructLiteral,
    Field,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalEventKind {
    BindArgument {
        label: Arc<str>,
    },
    EnterExpr {
        kind: EvalExprKind,
    },
    ExitReady {
        reuse: ReuseClass,
        crossing: CrossingPolicy,
    },
    ExitPoison {
        kind: EvalErrorKind,
        message: Arc<str>,
    },
    RuntimeCallStarted {
        callee: Arc<str>,
        argc: u32,
    },
    RuntimeCallFinishedReady {
        reuse: ReuseClass,
        crossing: CrossingPolicy,
    },
    RuntimeCallFinishedPoison {
        kind: EvalErrorKind,
        message: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalEvent {
    pub node_id: EvalNodeId,
    pub parent_id: Option<EvalNodeId>,
    pub place: Arc<str>,
    pub source: Option<SourceSpan>,
    pub kind: EvalEventKind,
}

pub trait EvalEventSink {
    fn push(&mut self, event: EvalEvent);
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvalTrace {
    pub events: Vec<EvalEvent>,
}

impl EvalEventSink for EvalTrace {
    fn push(&mut self, event: EvalEvent) {
        self.events.push(event);
    }
}

pub struct NullEvalEventSink;

impl EvalEventSink for NullEvalEventSink {
    fn push(&mut self, _event: EvalEvent) {}
}
