//! Experimental evaluator v2 core model.
//!
//! This crate is intentionally narrow. It is not a second production
//! evaluator yet. It exists to let us settle the core semantic data model
//! without inheriting `vx-eval`'s current layering problems.
//!
//! The key differences from v1 are:
//! - payload and metadata are separate (`ValueRep` vs `ValueMeta`)
//! - readiness is not a value (`Resolution` is separate)
//! - claims live next to any value, not in a wrapper enum case
//! - provenance is always available, even on successful scalar values
//! - boundary crossing policy is explicit metadata, not an implicit side effect

mod program;
pub use program::{EvalProgram, EvalProgramImport, EvalProgramSource};

mod decode;

mod engine;
pub use engine::{
    EvalActiveWave, EvalAuthoredCall, EvalAuthoredCallFrame, EvalBranchContext, EvalBufferedWork,
    EvalCallProfileRun, EvalCallProfileSummaryEntry, EvalCallTreeEntry, EvalCompletedWork,
    EvalCompletedWorkOutcome, EvalDependencyEdge, EvalExplainTerminal, EvalGraphNode,
    EvalGraphSnapshot, EvalKeyedNode, EvalKeyedNodeUpdateReason, EvalLeasedWork,
    EvalLeasedWorkState, EvalNodeExplanation, EvalNodeKind, EvalNodeRecord, EvalNodeState,
    EvalPreparedWork, EvalProfileEntry, EvalProfileSummaryEntry, EvalQueuedWork, EvalRunnableNode,
    EvalSession, EvalSessionCallProfile, EvalSessionCheckpoint, EvalSessionProfile,
    EvalSessionProfileSummary, EvalSessionStats, EvalSessionStep, EvalWaveId, EvalWorkCompletion,
    EvalWorkId, EvalWorkKind, EvalWorkRecord, EvalWorkState, RuntimeCallOutcome, RuntimeHooks,
    RuntimeReady, RuntimeSlotPrepare, eval_source_entry, eval_source_entry_at_place,
    eval_source_entry_at_place_with_args, eval_source_entry_at_place_with_args_with_source_id,
    eval_source_entry_at_place_with_args_with_source_id_traced,
    eval_source_entry_at_place_with_args_with_source_id_with_sink,
    eval_source_entry_at_place_with_source_id, eval_source_entry_with_args,
    eval_source_entry_with_args_with_source_id, eval_source_entry_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id_with_sink, eval_source_entry_with_source_id,
    eval_source_entry_with_source_id_traced,
};

mod result;
pub use result::{DemandFrame, EvalError, EvalErrorKind, PendingRef, Resolution};

mod slot;
pub use slot::{
    SlotAction, SlotDecision, SlotId, SlotRecord, SlotRecordMeta, SlotResolution,
    SlotResolutionKind, SlotResolveRequest, SlotResolved, SlotStore, SlotStoreReason,
    SlotVerification,
};

mod trace;
pub use trace::{
    ChannelEvalEventSink, EvalEvent, EvalEventKind, EvalEventSink, EvalExprKind, EvalNodeId,
    EvalTrace, NullEvalEventSink, SlotEventReason, channel_trace,
};

mod value;
pub use value::{ExternalValue, FunctionRef, MapValue, RangeValue, StructValue, Value, ValueRep};

mod wire;
pub use vx_eval2_proto::{
    ByteOffset, Claim, ColumnIndex, CrossingPolicy, LineIndex, PlaceRef, ProjectionStep,
    Provenance, ProvenanceRoot, ReuseClass, RuntimeInputKind, SourcePoint, SourceSpan, ValueMeta,
};
pub use wire::{envelope_to_value, resolution_to_outcome, value_to_envelope};

#[cfg(test)]
mod tests;
