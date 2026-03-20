//! Execution trace events for eval2.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::{CrossingPolicy, EvalErrorKind, EvalWorkId, PendingRef, ReuseClass, SourceSpan, Value};

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
pub enum SlotEventReason {
    Missing,
    WitnessChanged,
    ClaimsInvalid { reason: Arc<str> },
    Refresh { intent: Value },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalEventKind {
    BindArgument {
        label: Arc<str>,
    },
    DependOn {
        dependency: EvalNodeId,
    },
    EnterExpr {
        kind: EvalExprKind,
    },
    ExitReady {
        reuse: ReuseClass,
        crossing: CrossingPolicy,
    },
    ExitPending {
        pending: PendingRef,
    },
    ExitPoison {
        kind: EvalErrorKind,
        message: Arc<str>,
    },
    RegisterPending {
        pending: PendingRef,
    },
    RuntimeCallStarted {
        callee: Arc<str>,
        argc: u32,
    },
    SlotPopulate {
        slot: Arc<str>,
        reason: SlotEventReason,
    },
    SlotReuse {
        slot: Arc<str>,
    },
    SlotRewrite {
        slot: Arc<str>,
        reason: SlotEventReason,
    },
    RuntimeCallFinishedReady {
        reuse: ReuseClass,
        crossing: CrossingPolicy,
    },
    RuntimeCallFinishedPending {
        pending: PendingRef,
    },
    RuntimeCallFinishedPoison {
        kind: EvalErrorKind,
        message: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalEvent {
    pub work_id: Option<EvalWorkId>,
    pub node_id: EvalNodeId,
    pub parent_id: Option<EvalNodeId>,
    pub place: Arc<str>,
    pub source: Option<SourceSpan>,
    pub kind: EvalEventKind,
}

pub trait EvalEventSink: Send {
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

pub struct ChannelEvalEventSink {
    sender: Sender<EvalEvent>,
}

impl ChannelEvalEventSink {
    pub fn new(sender: Sender<EvalEvent>) -> Self {
        Self { sender }
    }
}

impl EvalEventSink for ChannelEvalEventSink {
    fn push(&mut self, event: EvalEvent) {
        let _ = self.sender.send(event);
    }
}

pub struct NullEvalEventSink;

impl EvalEventSink for NullEvalEventSink {
    fn push(&mut self, _event: EvalEvent) {}
}

pub fn channel_trace() -> (ChannelEvalEventSink, Receiver<EvalEvent>) {
    let (sender, receiver) = channel();
    (ChannelEvalEventSink::new(sender), receiver)
}
