//! Tiny parser-backed evaluator for early spec fixtures.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, mpsc};
use std::thread;

use im::{OrdMap, Vector};
use vx_parser::{
    ArrayExpr, ArrayItem, AstNode, BinaryExpr, BlockExpr, CallExpr, ClosureExpr, Expr, FailExpr,
    FieldExpr, IfExpr, InterpolatedStringExpr, LiteralExpr, MapExpr, NameRefExpr, Pattern,
    PrefixExpr, RangeExpr, Stmt, StringPart, StructLiteralExpr, SyntaxKind, TupleExpr,
};
use vx_resolve::SourceId;
use vx_types::Ty;

use crate::{
    CrossingPolicy, EvalError, EvalErrorKind, EvalEvent, EvalEventKind, EvalEventSink,
    EvalExprKind, EvalNodeId, EvalTrace, FunctionRef, NullEvalEventSink, PendingRef,
    ProjectionStep, Provenance, RangeValue, Resolution, ReuseClass, RuntimeInputKind, SlotAction,
    SourceSpan, StructValue, Value, ValueMeta, ValueRep,
    decode::{StructuredFormat, parse_blob_as_value},
    program::{CompiledParameter, CompiledProgram, EvalProgram, ProgramNodeHandle},
};

mod graph;
pub use graph::*;

mod session;
pub use session::*;

mod state;
use state::*;

mod work;
use work::*;

mod eval;
use eval::*;

pub trait RuntimeHooks: Send + Sync {
    fn call(
        &self,
        place: &str,
        callee: &FunctionRef,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError>;

    fn exec(
        &self,
        _place: &str,
        _request: &RuntimeExecRequest,
        _source: &SourceSpan,
    ) -> Result<RuntimeExecReady, EvalError> {
        Err(EvalError::new(
            EvalErrorKind::Internal,
            "exec is not supported by this runtime",
        ))
    }

    fn prepare_slot(
        &self,
        _place: &str,
        _slot_id: &str,
        _witness: &Value,
        _action: &SlotAction<Value>,
    ) -> Result<RuntimeSlotPrepare, EvalError> {
        Err(EvalError::new(
            EvalErrorKind::Internal,
            "slot operations are not supported by this runtime",
        ))
    }

    fn store_slot(
        &self,
        _place: &str,
        _slot_id: &str,
        _witness: &Value,
        _action: &SlotAction<Value>,
        _resolved: &Value,
    ) -> Result<RuntimeReady, EvalError> {
        Err(EvalError::new(
            EvalErrorKind::Internal,
            "slot operations are not supported by this runtime",
        ))
    }
}

trait EvalWorkerScheduler {
    fn alloc_node_id(&mut self) -> EvalNodeId;
    fn record_dependency(&mut self, parent: EvalNodeId, child: EvalNodeId);
    fn record_node(&mut self, record: EvalNodeRecord);
    fn record_node_state(&mut self, node_id: EvalNodeId, state: EvalNodeState);
    fn alloc_pending(&mut self, description: Arc<str>) -> PendingRef;
    fn alloc_child_dependency_pending(&mut self, dependency: EvalNodeId) -> PendingRef;
    fn pending_kind(&self, pending: &PendingRef) -> Option<PendingKind>;
    fn register_suspension(&mut self, suspension: EvalSuspension);
    fn remember_expr_task(
        &mut self,
        node_id: EvalNodeId,
        scheduled: ScheduledExpr,
        branch_context: EvalBranchContext,
    );
    fn demand_keyed_node(
        &mut self,
        resolver: FunctionRef,
        key: Value,
        joined: Value,
        branch_context: EvalBranchContext,
        source: SourceSpan,
    ) -> Result<EvalNodeId, EvalError>;
    fn finalize_keyed_completion(&mut self, node_id: EvalNodeId) -> Result<bool, EvalError>;
    fn keyed_resolution(&self, node_id: EvalNodeId) -> Option<Result<Value, EvalError>>;
    fn keyed_node(&self, node_id: EvalNodeId) -> Option<&KeyedNodeMemo>;
    fn node_state(&self, node_id: EvalNodeId) -> Option<&EvalNodeState>;
    fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord>;
    fn learned_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        index: usize,
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError>;
    fn learned_search_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError>;
    fn record_learned_no_good(&mut self, learned: LearnedNoGood) -> Result<(), EvalError>;
    fn record_learned_search_no_good(
        &mut self,
        learned: LearnedSearchNoGood,
    ) -> Result<(), EvalError>;
    fn enqueue_ready_task(
        &mut self,
        task: ReadyTask,
        kind: EvalWorkKind,
        node_id: EvalNodeId,
        pending: Option<PendingRef>,
        branch_context: EvalBranchContext,
    ) -> EvalQueuedWork;
}

struct NoRuntime;

impl RuntimeHooks for NoRuntime {
    fn call(
        &self,
        _place: &str,
        _callee: &FunctionRef,
        _args: &[Value],
        _source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        Ok(RuntimeCallOutcome::Unsupported)
    }
}

pub fn eval_source_entry(source: &str, entry: &str) -> Result<Resolution, EvalError> {
    eval_source_entry_with_source_id(source, "<memory>", entry)
}

pub fn eval_source_entry_with_source_id_traced(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
) -> Result<(Resolution, EvalTrace), EvalError> {
    eval_source_entry_with_args_with_source_id_traced(source, source_id, entry, &[])
}

pub fn eval_source_entry_with_args(
    source: &str,
    entry: &str,
    args: &[Value],
) -> Result<Resolution, EvalError> {
    eval_source_entry_with_args_with_source_id(source, "<memory>", entry, args)
}

pub fn eval_source_entry_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
) -> Result<Resolution, EvalError> {
    eval_source_entry_with_args_with_source_id(source, source_id, entry, &[])
}

pub fn eval_source_entry_with_args_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_with_args_with_source_id_with_sink(source, source_id, entry, args, &mut sink)
}

pub fn eval_source_entry_with_args_with_source_id_traced(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
) -> Result<(Resolution, EvalTrace), EvalError> {
    let mut trace = EvalTrace::default();
    let resolution = eval_source_entry_with_args_with_source_id_with_sink(
        source, source_id, entry, args, &mut trace,
    )
    .unwrap_or_else(Resolution::Poison);
    Ok((resolution, trace))
}

pub fn eval_source_entry_at_place_with_args_with_source_id_traced(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<(Resolution, EvalTrace), EvalError> {
    let mut trace = EvalTrace::default();
    let resolution = eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, source_id, entry, args, runtime, place, &mut trace,
    )
    .unwrap_or_else(Resolution::Poison);
    Ok((resolution, trace))
}

pub fn eval_source_entry_at_place(
    source: &str,
    entry: &str,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source,
        "<memory>",
        entry,
        &[],
        runtime,
        place,
        &mut sink,
    )
}

pub fn eval_source_entry_at_place_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source,
        source_id,
        entry,
        &[],
        runtime,
        place,
        &mut sink,
    )
}

pub fn eval_source_entry_at_place_with_args(
    source: &str,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, "<memory>", entry, args, runtime, place, &mut sink,
    )
}

pub fn eval_source_entry_at_place_with_args_with_source_id(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
) -> Result<Resolution, EvalError> {
    let mut sink = NullEvalEventSink;
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, source_id, entry, args, runtime, place, &mut sink,
    )
}

pub fn eval_source_entry_with_args_with_source_id_with_sink(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    sink: &mut impl EvalEventSink,
) -> Result<Resolution, EvalError> {
    eval_source_entry_at_place_with_args_with_source_id_with_sink(
        source, source_id, entry, args, &NoRuntime, "local", sink,
    )
}

pub fn eval_source_entry_at_place_with_args_with_source_id_with_sink(
    source: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
    sink: &mut impl EvalEventSink,
) -> Result<Resolution, EvalError> {
    let mut session = EvalSession::new(source, source_id, entry, args.to_vec(), runtime, place);
    match session.run_with_event_sink(sink, true)? {
        Resolution::Poison(error) => Err(error),
        resolution => Ok(resolution),
    }
}
