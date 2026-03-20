//! Tiny parser-backed evaluator for early spec fixtures.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use vx_parser::{
    ArrayExpr, ArrayItem, AstNode, BlockExpr, CallExpr, Expr, FailExpr, FieldExpr, FnDecl, IfExpr,
    Item, LineIndex as ParserLineIndex, LiteralExpr, MapExpr, NameRefExpr, Parameter, SourceFile,
    Stmt, StructLiteralExpr, SyntaxToken, TextRange, TextSize, trimmed_range,
};

use crate::{
    ByteOffset, ColumnIndex, CrossingPolicy, EvalError, EvalErrorKind, EvalEvent, EvalEventKind,
    EvalEventSink, EvalExprKind, EvalNodeId, EvalTrace, LineIndex, NullEvalEventSink, PendingRef,
    ProjectionStep, Provenance, Resolution, ReuseClass, RuntimeInputKind, SourcePoint, SourceSpan,
    StructValue, Value, ValueMeta, ValueRep,
};

pub trait RuntimeHooks: Send + Sync {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCallOutcome {
    Unsupported,
    Ready(Value),
    Pending { description: Arc<str> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalNodeState {
    Ready(Value),
    Runnable {
        work: EvalWorkId,
        pending: Option<PendingRef>,
    },
    BlockedOnNode {
        dependency: EvalNodeId,
    },
    Pending(PendingRef),
    Poison(EvalError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EvalWorkId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalWorkKind {
    StartEntry,
    EvaluateExpr,
    ResumePending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalSessionStep {
    Progressed,
    Pending(PendingRef),
    Ready(Value),
    Poison(EvalError),
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalCompletedWorkOutcome {
    Progressed,
    BlockedOnNode(EvalNodeId),
    Pending(PendingRef),
    Ready(Value),
    Poison(EvalError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCompletedWork {
    pub work: EvalQueuedWork,
    pub outcome: EvalCompletedWorkOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalWorkState {
    Queued,
    Completed(EvalCompletedWorkOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalWorkRecord {
    pub work: EvalQueuedWork,
    pub state: EvalWorkState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalDependencyEdge {
    pub parent: EvalNodeId,
    pub child: EvalNodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalNodeKind {
    SyntheticRoot,
    ArgumentBinding { label: Arc<str> },
    Expr { kind: EvalExprKind },
    PendingToken { description: Arc<str> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalNodeRecord {
    pub node_id: EvalNodeId,
    pub parent_id: Option<EvalNodeId>,
    pub kind: EvalNodeKind,
    pub source: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalExplainTerminal {
    Runnable {
        node_id: EvalNodeId,
        work: EvalWorkId,
        pending: Option<PendingRef>,
    },
    Pending {
        node_id: EvalNodeId,
        pending: PendingRef,
    },
    Ready {
        node_id: EvalNodeId,
    },
    Poison {
        node_id: EvalNodeId,
        error: EvalError,
    },
    Unknown {
        node_id: EvalNodeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalNodeExplanation {
    pub chain: Vec<EvalNodeId>,
    pub terminal: EvalExplainTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalQueuedWork {
    pub id: EvalWorkId,
    pub kind: EvalWorkKind,
    pub node_id: EvalNodeId,
    pub pending: Option<PendingRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalRunnableNode {
    pub node_id: EvalNodeId,
    pub work_id: EvalWorkId,
    pub kind: EvalWorkKind,
    pub pending: Option<PendingRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalGraphNode {
    pub node_id: EvalNodeId,
    pub record: Option<EvalNodeRecord>,
    pub state: Option<EvalNodeState>,
    pub dependencies: Vec<EvalNodeId>,
    pub dependents: Vec<EvalNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalGraphSnapshot {
    pub nodes: Vec<EvalGraphNode>,
    pub work: Vec<EvalWorkRecord>,
    pub pending: Vec<PendingRef>,
    pub completed: Option<Result<Value, EvalError>>,
}

#[derive(Debug, Clone)]
pub struct EvalSessionCheckpoint {
    scheduler: EvalScheduler,
    trace: EvalTrace,
    completed_work: Vec<EvalCompletedWork>,
    completed: Option<Result<Value, EvalError>>,
}

pub struct EvalPreparedWork<'runtime, Runtime: RuntimeHooks> {
    source: String,
    source_id: String,
    entry: String,
    args: Vec<Value>,
    runtime: &'runtime Runtime,
    place: String,
    base_generation: u64,
    checkpoint: EvalSessionCheckpoint,
    record: ReadyTaskRecord,
}

pub struct EvalWorkCompletion {
    base_generation: u64,
    work: EvalQueuedWork,
    step: EvalSessionStep,
    checkpoint: EvalSessionCheckpoint,
}

pub struct EvalSession<'runtime, Runtime: RuntimeHooks> {
    source: String,
    source_id: String,
    entry: String,
    args: Vec<Value>,
    runtime: &'runtime Runtime,
    place: String,
    scheduler: EvalScheduler,
    trace: EvalTrace,
    completed_work: Vec<EvalCompletedWork>,
    completed: Option<Result<Value, EvalError>>,
    history: Vec<EvalSessionCheckpoint>,
    generation: u64,
}

impl<'runtime, Runtime: RuntimeHooks> EvalSession<'runtime, Runtime> {
    pub fn new(
        source: impl Into<String>,
        source_id: impl Into<String>,
        entry: impl Into<String>,
        args: Vec<Value>,
        runtime: &'runtime Runtime,
        place: impl Into<String>,
    ) -> Self {
        let mut scheduler = EvalScheduler::default();
        scheduler.enqueue_start_entry();
        Self {
            source: source.into(),
            source_id: source_id.into(),
            entry: entry.into(),
            args,
            runtime,
            place: place.into(),
            scheduler,
            trace: EvalTrace::default(),
            completed_work: Vec::new(),
            completed: None,
            history: Vec::new(),
            generation: 0,
        }
    }

    pub fn run(&mut self) -> Result<Resolution, EvalError> {
        loop {
            match self.step()? {
                EvalSessionStep::Progressed => {}
                EvalSessionStep::Pending(pending) => return Ok(Resolution::Pending(pending)),
                EvalSessionStep::Ready(value) => return Ok(Resolution::Ready(value)),
                EvalSessionStep::Poison(error) => return Ok(Resolution::Poison(error)),
                EvalSessionStep::Idle => {
                    return Ok(match self.scheduler.pending_nodes().into_iter().next() {
                        Some(pending) => Resolution::Pending(pending),
                        None => match &self.completed {
                            Some(Ok(value)) => Resolution::Ready(value.clone()),
                            Some(Err(error)) => Resolution::Poison(error.clone()),
                            None => Resolution::Poison(EvalError::new(
                                EvalErrorKind::Internal,
                                "eval session has no ready work and no pending nodes",
                            )),
                        },
                    });
                }
            }
        }
    }

    pub fn step(&mut self) -> Result<EvalSessionStep, EvalError> {
        if let Some(result) = &self.completed {
            return Ok(match result {
                Ok(value) => EvalSessionStep::Ready(value.clone()),
                Err(error) => EvalSessionStep::Poison(error.clone()),
            });
        }

        let snapshot = self.checkpoint();

        let Some(prepared) = self.prepare_step() else {
            return Ok(match self.scheduler.pending_nodes().into_iter().next() {
                Some(pending) => EvalSessionStep::Pending(pending),
                None => EvalSessionStep::Idle,
            });
        };
        let completion = prepared.execute()?;
        self.history.push(snapshot);
        self.apply_completion(completion)
    }

    pub fn resolve_pending(&mut self, pending: &PendingRef, value: Value) -> Result<(), EvalError> {
        self.scheduler.resolve_pending(pending, Ok(value))?;
        self.generation += 1;
        Ok(())
    }

    pub fn poison_pending(
        &mut self,
        pending: &PendingRef,
        error: EvalError,
    ) -> Result<(), EvalError> {
        self.scheduler.resolve_pending(pending, Err(error))?;
        self.generation += 1;
        Ok(())
    }

    pub fn pending_nodes(&self) -> Vec<PendingRef> {
        self.scheduler.pending_nodes()
    }

    pub fn can_step_back(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn step_back(&mut self) -> bool {
        let Some(snapshot) = self.history.pop() else {
            return false;
        };
        self.restore(snapshot);
        true
    }

    pub fn queued_work(&self) -> Vec<EvalQueuedWork> {
        self.scheduler.queued_work()
    }

    pub fn runnable_nodes(&self) -> Vec<EvalRunnableNode> {
        self.scheduler.runnable_nodes()
    }

    pub fn node_state(&self, node_id: EvalNodeId) -> Option<&EvalNodeState> {
        self.scheduler.node_state(node_id)
    }

    pub fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        self.scheduler.node_record(node_id)
    }

    pub fn node_records(&self) -> Vec<EvalNodeRecord> {
        self.scheduler.node_records()
    }

    pub fn dependency_edges(&self) -> &[EvalDependencyEdge] {
        self.scheduler.dependency_edges()
    }

    pub fn explain_node(&self, node_id: EvalNodeId) -> EvalNodeExplanation {
        self.scheduler.explain_node(node_id)
    }

    pub fn trace(&self) -> &EvalTrace {
        &self.trace
    }

    pub fn completed_work(&self) -> &[EvalCompletedWork] {
        &self.completed_work
    }

    pub fn checkpoint(&self) -> EvalSessionCheckpoint {
        EvalSessionCheckpoint {
            scheduler: self.scheduler.clone(),
            trace: self.trace.clone(),
            completed_work: self.completed_work.clone(),
            completed: self.completed.clone(),
        }
    }

    pub fn restore(&mut self, checkpoint: EvalSessionCheckpoint) {
        self.scheduler = checkpoint.scheduler;
        self.trace = checkpoint.trace;
        self.completed_work = checkpoint.completed_work;
        self.completed = checkpoint.completed;
        self.generation += 1;
    }

    pub fn prepare_step(&self) -> Option<EvalPreparedWork<'runtime, Runtime>> {
        if self.completed.is_some() {
            return None;
        }
        let record = self.scheduler.peek_ready_task()?;
        Some(EvalPreparedWork {
            source: self.source.clone(),
            source_id: self.source_id.clone(),
            entry: self.entry.clone(),
            args: self.args.clone(),
            runtime: self.runtime,
            place: self.place.clone(),
            base_generation: self.generation,
            checkpoint: self.checkpoint(),
            record,
        })
    }

    pub fn apply_completion(
        &mut self,
        completion: EvalWorkCompletion,
    ) -> Result<EvalSessionStep, EvalError> {
        if completion.base_generation != self.generation {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "stale eval work completion: expected generation {}, got {}",
                    self.generation, completion.base_generation
                ),
            ));
        }
        self.restore(completion.checkpoint);
        Ok(completion.step)
    }

    pub fn work_records(&self) -> Vec<EvalWorkRecord> {
        let mut records = self
            .completed_work
            .iter()
            .map(|completed| EvalWorkRecord {
                work: completed.work.clone(),
                state: EvalWorkState::Completed(completed.outcome.clone()),
            })
            .collect::<Vec<_>>();
        records.extend(
            self.scheduler
                .queued_work()
                .into_iter()
                .map(|work| EvalWorkRecord {
                    work,
                    state: EvalWorkState::Queued,
                }),
        );
        records.sort_by_key(|record| record.work.id);
        records
    }

    pub fn graph_snapshot(&self) -> EvalGraphSnapshot {
        let mut node_ids = self
            .scheduler
            .node_records
            .keys()
            .copied()
            .collect::<Vec<_>>();
        node_ids.extend(self.scheduler.node_states.keys().copied());
        for edge in &self.scheduler.dependency_edges {
            node_ids.push(edge.parent);
            node_ids.push(edge.child);
        }
        node_ids.sort();
        node_ids.dedup();

        let mut dependencies = BTreeMap::<EvalNodeId, Vec<EvalNodeId>>::new();
        let mut dependents = BTreeMap::<EvalNodeId, Vec<EvalNodeId>>::new();
        for edge in &self.scheduler.dependency_edges {
            dependencies
                .entry(edge.parent)
                .or_default()
                .push(edge.child);
            dependents.entry(edge.child).or_default().push(edge.parent);
        }

        EvalGraphSnapshot {
            nodes: node_ids
                .into_iter()
                .map(|node_id| EvalGraphNode {
                    node_id,
                    record: self.scheduler.node_record(node_id).cloned(),
                    state: self.scheduler.node_state(node_id).cloned(),
                    dependencies: dependencies.remove(&node_id).unwrap_or_default(),
                    dependents: dependents.remove(&node_id).unwrap_or_default(),
                })
                .collect(),
            work: self.work_records(),
            pending: self.pending_nodes(),
            completed: self.completed.clone(),
        }
    }
}

impl<'runtime, Runtime: RuntimeHooks> EvalPreparedWork<'runtime, Runtime> {
    pub fn work(&self) -> &EvalQueuedWork {
        &self.record.work
    }

    pub fn base_generation(&self) -> u64 {
        self.base_generation
    }

    pub fn execute(self) -> Result<EvalWorkCompletion, EvalError> {
        let EvalPreparedWork {
            source,
            source_id,
            entry,
            args,
            runtime,
            place,
            base_generation,
            checkpoint,
            record,
        } = self;
        let (checkpoint, step) = execute_ready_task(
            &source,
            &source_id,
            &entry,
            &args,
            runtime,
            &place,
            checkpoint,
            record.clone(),
        )?;
        Ok(EvalWorkCompletion {
            base_generation,
            work: record.work,
            step,
            checkpoint,
        })
    }
}

impl EvalWorkCompletion {
    pub fn work(&self) -> &EvalQueuedWork {
        &self.work
    }

    pub fn step(&self) -> &EvalSessionStep {
        &self.step
    }

    pub fn base_generation(&self) -> u64 {
        self.base_generation
    }
}

struct NoRuntime;

impl RuntimeHooks for NoRuntime {
    fn call(
        &self,
        _place: &str,
        _callee: &str,
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
    let mut scheduler = EvalScheduler::default();
    eval_source_entry_at_place_with_args_with_source_id_with_scheduler_and_sink(
        source,
        source_id,
        entry,
        args,
        runtime,
        place,
        &mut scheduler,
        sink,
    )
}

fn eval_source_entry_at_place_with_args_with_source_id_with_scheduler_and_sink(
    source_text: &str,
    source_id: impl Into<String>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
    scheduler: &mut EvalScheduler,
    sink: &mut impl EvalEventSink,
) -> Result<Resolution, EvalError> {
    let source = SourceContext::new(source_id, source_text);
    let mut state = EvalState::new(
        source_text,
        &source,
        runtime,
        place,
        sink,
        scheduler,
        None,
        false,
    );
    match evaluate_entry_once(&mut state, source_text, entry, args)? {
        EvalStep::Ready(value) => Ok(Resolution::Ready(value)),
        EvalStep::Suspended(suspension) => Ok(Resolution::Pending(suspension.pending)),
    }
}

struct SourceContext {
    source_id: String,
    line_index: ParserLineIndex,
}

struct EvalState<'source, 'scheduler, Runtime, Sink> {
    source_text: &'source str,
    source: &'source SourceContext,
    runtime: &'source Runtime,
    place: &'source str,
    sink: &'source mut Sink,
    scheduler: &'scheduler mut EvalScheduler,
    current_work: Option<EvalWorkId>,
    queue_child_work: bool,
    stack: Vec<EvalNodeId>,
}

#[derive(Debug, Clone)]
struct EvalScheduler {
    next_id: u64,
    next_work_id: u64,
    node_states: BTreeMap<EvalNodeId, EvalNodeState>,
    node_records: BTreeMap<EvalNodeId, EvalNodeRecord>,
    dependency_edges: Vec<EvalDependencyEdge>,
    ready_queue: VecDeque<EvalWorkId>,
    ready_tasks: BTreeMap<EvalWorkId, ReadyTaskRecord>,
    pending_resumes: BTreeMap<u64, EvalSuspension>,
    pending_kinds: BTreeMap<u64, PendingKind>,
}

const ENTRY_WORK_NODE_ID: EvalNodeId = EvalNodeId(0);

impl Default for EvalScheduler {
    fn default() -> Self {
        Self {
            next_id: 1,
            next_work_id: 1,
            node_states: BTreeMap::new(),
            node_records: BTreeMap::new(),
            dependency_edges: Vec::new(),
            ready_queue: VecDeque::new(),
            ready_tasks: BTreeMap::new(),
            pending_resumes: BTreeMap::new(),
            pending_kinds: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct ReadyTaskRecord {
    work: EvalQueuedWork,
    task: ReadyTask,
}

#[derive(Debug, Clone)]
struct ScheduledResume {
    suspension: EvalSuspension,
    result: Result<Value, EvalError>,
}

#[derive(Debug, Clone)]
struct ScheduledExpr {
    expr: ExprHandle,
    env: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
enum ReadyTask {
    StartEntry,
    EvaluateExpr(ScheduledExpr),
    Resume(ScheduledResume),
}

#[derive(Debug, Clone)]
struct EvalSuspension {
    resume_node: EvalNodeId,
    pending: PendingRef,
    frames: Vec<ContinuationFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExprHandle {
    range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StmtHandle {
    range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    External,
    ChildDependency { dependency: EvalNodeId },
}

#[derive(Debug, Clone)]
enum ContinuationFrame {
    FinishNode {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
    },
    RuntimeCall {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        call_source: SourceSpan,
    },
    CallJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        callee_name: String,
        call_source: SourceSpan,
        child_nodes: Vec<EvalNodeId>,
    },
    BlockAfterLet {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        scope: BTreeMap<String, Value>,
        last: Value,
        name: String,
        name_source: SourceSpan,
        remaining_statements: Vec<StmtHandle>,
    },
    BlockAfterExpr {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        scope: BTreeMap<String, Value>,
        remaining_statements: Vec<StmtHandle>,
    },
    CallAfterArg {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        callee_name: String,
        call_source: SourceSpan,
        args: Vec<Value>,
        remaining_args: Vec<ExprHandle>,
        env: BTreeMap<String, Value>,
    },
    ArrayAfterItem {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        remaining_items: Vec<ExprHandle>,
        env: BTreeMap<String, Value>,
        array_source: SourceSpan,
    },
    ArrayJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        child_nodes: Vec<EvalNodeId>,
        array_source: SourceSpan,
    },
    MapAfterKey {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        entries: Vec<(Value, Value)>,
        value_expr: ExprHandle,
        remaining_entries: Vec<(ExprHandle, ExprHandle)>,
        env: BTreeMap<String, Value>,
        map_source: SourceSpan,
    },
    MapAfterValue {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        entries: Vec<(Value, Value)>,
        key: Value,
        remaining_entries: Vec<(ExprHandle, ExprHandle)>,
        env: BTreeMap<String, Value>,
        map_source: SourceSpan,
    },
    FailMessage {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
    },
    IfCondition {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        then_branch: ExprHandle,
        else_branch: ExprHandle,
        env: BTreeMap<String, Value>,
    },
    StructAfterField {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        type_name: Arc<str>,
        fields: Vec<(Arc<str>, Value)>,
        name: Arc<str>,
        remaining_fields: Vec<(Arc<str>, ExprHandle)>,
        env: BTreeMap<String, Value>,
        struct_source: SourceSpan,
    },
    StructJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        type_name: Arc<str>,
        field_names: Vec<Arc<str>>,
        child_nodes: Vec<EvalNodeId>,
        struct_source: SourceSpan,
    },
    FieldReceiver {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        field_name: String,
    },
}

#[derive(Debug, Clone)]
enum EvalStep {
    Ready(Value),
    Suspended(EvalSuspension),
}

impl<'source, 'scheduler, Runtime: RuntimeHooks, Sink: EvalEventSink>
    EvalState<'source, 'scheduler, Runtime, Sink>
{
    fn new(
        source_text: &'source str,
        source: &'source SourceContext,
        runtime: &'source Runtime,
        place: &'source str,
        sink: &'source mut Sink,
        scheduler: &'scheduler mut EvalScheduler,
        current_work: Option<EvalWorkId>,
        queue_child_work: bool,
    ) -> Self {
        Self {
            source_text,
            source,
            runtime,
            place,
            sink,
            scheduler,
            current_work,
            queue_child_work,
            stack: Vec::new(),
        }
    }

    fn alloc_node_id(&mut self) -> EvalNodeId {
        self.scheduler.alloc_node_id()
    }

    fn current_parent(&self) -> Option<EvalNodeId> {
        self.stack.last().copied()
    }

    fn emit(
        &mut self,
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        source: Option<SourceSpan>,
        kind: EvalEventKind,
    ) {
        self.sink.push(EvalEvent {
            work_id: self.current_work,
            node_id,
            parent_id,
            place: Arc::from(self.place),
            source,
            kind,
        });
    }

    fn emit_current_dependency(&mut self, dependency: EvalNodeId, source: Option<SourceSpan>) {
        if let Some(node_id) = self.current_parent() {
            self.scheduler.record_dependency(node_id, dependency);
            let parent_id = self.stack.iter().rev().nth(1).copied();
            self.emit(
                node_id,
                parent_id,
                source,
                EvalEventKind::DependOn { dependency },
            );
        }
    }

    fn with_active_node<T>(&mut self, node_id: EvalNodeId, f: impl FnOnce(&mut Self) -> T) -> T {
        self.stack.push(node_id);
        let result = f(self);
        let popped = self.stack.pop();
        debug_assert_eq!(popped, Some(node_id));
        result
    }
}

enum ResumeOutcome {
    Continue(Result<Value, EvalError>),
    Suspended(EvalSuspension),
}

fn record_ready_step(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    value: &Value,
) {
    state
        .scheduler
        .record_node_state(node_id, EvalNodeState::Ready(value.clone()));
    state.emit(
        node_id,
        parent_id,
        None,
        EvalEventKind::ExitReady {
            reuse: value.meta.reuse.clone(),
            crossing: value.meta.crossing.clone(),
        },
    );
}

fn record_pending_step(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    pending: &PendingRef,
) {
    state
        .scheduler
        .record_node_state(node_id, EvalNodeState::Pending(pending.clone()));
    state.emit(
        node_id,
        parent_id,
        None,
        EvalEventKind::ExitPending {
            pending: pending.clone(),
        },
    );
}

fn record_poison_step(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    error: &EvalError,
) {
    state
        .scheduler
        .record_node_state(node_id, EvalNodeState::Poison(error.clone()));
    state.emit(
        node_id,
        parent_id,
        None,
        EvalEventKind::ExitPoison {
            kind: error.kind,
            message: error.message.clone(),
        },
    );
}

fn wrap_suspended(
    pending: PendingRef,
    resume_node: EvalNodeId,
    frame: ContinuationFrame,
) -> EvalStep {
    EvalStep::Suspended(EvalSuspension {
        resume_node,
        pending,
        frames: vec![frame],
    })
}

fn extend_suspension(mut suspension: EvalSuspension, frame: ContinuationFrame) -> EvalStep {
    suspension.frames.push(frame);
    EvalStep::Suspended(suspension)
}

fn conclude_resumed_node(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    result: Result<EvalStep, EvalError>,
) -> ResumeOutcome {
    match result {
        Ok(EvalStep::Ready(value)) => {
            record_ready_step(state, node_id, parent_id, &value);
            ResumeOutcome::Continue(Ok(value))
        }
        Ok(EvalStep::Suspended(suspension)) => {
            record_pending_step(state, node_id, parent_id, &suspension.pending);
            ResumeOutcome::Suspended(suspension)
        }
        Err(error) => {
            record_poison_step(state, node_id, parent_id, &error);
            ResumeOutcome::Continue(Err(error))
        }
    }
}

fn eval_dependent_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    if state.current_parent() == Some(node_id) {
        eval_child_expr(state, expr, env)
    } else {
        state.with_active_node(node_id, |state| eval_child_expr(state, expr, env))
    }
}

fn expr_handle(expr: &Expr) -> ExprHandle {
    ExprHandle {
        range: trimmed_range(expr.syntax()),
    }
}

fn stmt_handle(stmt: &Stmt) -> StmtHandle {
    StmtHandle {
        range: trimmed_range(stmt.syntax()),
    }
}

fn resolve_expr_handle(source_text: &str, handle: ExprHandle) -> Result<Expr, EvalError> {
    let parse = vx_parser::parse(source_text);
    if !parse.errors().is_empty() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("parse failed: {:?}", parse.errors()),
        ));
    }
    let root = SourceFile::cast_root(parse.syntax());
    root.syntax()
        .descendants()
        .filter_map(Expr::cast)
        .find(|expr| trimmed_range(expr.syntax()) == handle.range)
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("expression at range {:?} was not found", handle.range),
            )
        })
}

fn resolve_stmt_handle(source_text: &str, handle: StmtHandle) -> Result<Stmt, EvalError> {
    let parse = vx_parser::parse(source_text);
    if !parse.errors().is_empty() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("parse failed: {:?}", parse.errors()),
        ));
    }
    let root = SourceFile::cast_root(parse.syntax());
    root.syntax()
        .descendants()
        .filter_map(Stmt::cast)
        .find(|stmt| trimmed_range(stmt.syntax()) == handle.range)
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("statement at range {:?} was not found", handle.range),
            )
        })
}

impl EvalScheduler {
    fn dependency_edges(&self) -> &[EvalDependencyEdge] {
        &self.dependency_edges
    }

    fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        self.node_records.get(&node_id)
    }

    fn node_records(&self) -> Vec<EvalNodeRecord> {
        self.node_records.values().cloned().collect()
    }

    fn record_node(&mut self, record: EvalNodeRecord) {
        self.node_records.entry(record.node_id).or_insert(record);
    }

    fn record_dependency(&mut self, parent: EvalNodeId, child: EvalNodeId) {
        self.dependency_edges
            .push(EvalDependencyEdge { parent, child });
    }

    fn queued_work(&self) -> Vec<EvalQueuedWork> {
        self.ready_queue
            .iter()
            .filter_map(|id| self.ready_tasks.get(id).map(|record| record.work.clone()))
            .collect()
    }

    fn runnable_nodes(&self) -> Vec<EvalRunnableNode> {
        self.ready_queue
            .iter()
            .filter_map(|id| self.ready_tasks.get(id))
            .map(|record| EvalRunnableNode {
                node_id: record.work.node_id,
                work_id: record.work.id,
                kind: record.work.kind,
                pending: record.work.pending.clone(),
            })
            .collect()
    }

    fn enqueue_start_entry(&mut self) -> EvalQueuedWork {
        self.record_node(EvalNodeRecord {
            node_id: ENTRY_WORK_NODE_ID,
            parent_id: None,
            kind: EvalNodeKind::SyntheticRoot,
            source: None,
        });
        let work = self.enqueue_ready_task(
            ReadyTask::StartEntry,
            EvalWorkKind::StartEntry,
            ENTRY_WORK_NODE_ID,
            None,
        );
        self.record_node_state(
            ENTRY_WORK_NODE_ID,
            EvalNodeState::Runnable {
                work: work.id,
                pending: None,
            },
        );
        work
    }

    fn alloc_node_id(&mut self) -> EvalNodeId {
        let node_id = EvalNodeId(self.next_id);
        self.next_id += 1;
        node_id
    }

    fn alloc_pending(&mut self, description: impl Into<Arc<str>>) -> PendingRef {
        let pending_id = self.alloc_node_id();
        let description = description.into();
        let pending = PendingRef::new(pending_id.0, description.clone());
        self.pending_kinds
            .insert(pending.node_id, PendingKind::External);
        self.record_node(EvalNodeRecord {
            node_id: pending_id,
            parent_id: None,
            kind: EvalNodeKind::PendingToken { description },
            source: None,
        });
        pending
    }

    fn alloc_child_dependency_pending(&mut self, dependency: EvalNodeId) -> PendingRef {
        let pending = PendingRef::new(dependency.0, "waiting on child expression");
        self.pending_kinds
            .insert(pending.node_id, PendingKind::ChildDependency { dependency });
        pending
    }

    fn pending_nodes(&self) -> Vec<PendingRef> {
        self.pending_resumes
            .values()
            .filter(|suspension| {
                matches!(
                    self.pending_kinds.get(&suspension.pending.node_id),
                    Some(PendingKind::External)
                )
            })
            .map(|suspension| &suspension.pending)
            .cloned()
            .collect()
    }

    fn pending_kind(&self, pending: &PendingRef) -> Option<PendingKind> {
        self.pending_kinds.get(&pending.node_id).copied()
    }

    fn node_state(&self, node_id: EvalNodeId) -> Option<&EvalNodeState> {
        self.node_states.get(&node_id)
    }

    fn explain_node(&self, node_id: EvalNodeId) -> EvalNodeExplanation {
        let mut chain = vec![node_id];
        let mut current = node_id;
        loop {
            let terminal = match self.node_states.get(&current) {
                Some(EvalNodeState::Runnable { work, pending }) => EvalExplainTerminal::Runnable {
                    node_id: current,
                    work: *work,
                    pending: pending.clone(),
                },
                Some(EvalNodeState::BlockedOnNode { dependency }) => {
                    chain.push(*dependency);
                    current = *dependency;
                    continue;
                }
                Some(EvalNodeState::Pending(pending)) => EvalExplainTerminal::Pending {
                    node_id: current,
                    pending: pending.clone(),
                },
                Some(EvalNodeState::Ready(_)) => EvalExplainTerminal::Ready { node_id: current },
                Some(EvalNodeState::Poison(error)) => EvalExplainTerminal::Poison {
                    node_id: current,
                    error: error.clone(),
                },
                None => EvalExplainTerminal::Unknown { node_id: current },
            };
            return EvalNodeExplanation { chain, terminal };
        }
    }

    fn record_node_state(&mut self, node_id: EvalNodeId, state: EvalNodeState) {
        self.node_states.insert(node_id, state);
    }

    fn resolve_pending(
        &mut self,
        pending: &PendingRef,
        result: Result<Value, EvalError>,
    ) -> Result<(), EvalError> {
        match self.pending_kind(pending) {
            Some(PendingKind::External) => {}
            Some(PendingKind::ChildDependency { .. }) => {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!(
                        "pending node {} is internal to the evaluator",
                        pending.node_id
                    ),
                ));
            }
            None => {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!("pending node {} is not registered", pending.node_id),
                ));
            }
        }
        self.resolve_registered_suspension(pending.node_id, result)
    }

    fn resolve_child_dependency(
        &mut self,
        dependency: EvalNodeId,
        result: Result<Value, EvalError>,
    ) -> Result<bool, EvalError> {
        match self.pending_kinds.get(&dependency.0).copied() {
            Some(PendingKind::ChildDependency {
                dependency: registered,
            }) if registered == dependency => {
                self.resolve_registered_suspension(dependency.0, result)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn resolve_registered_suspension(
        &mut self,
        pending_id: u64,
        result: Result<Value, EvalError>,
    ) -> Result<(), EvalError> {
        let Some(suspension) = self.pending_resumes.remove(&pending_id) else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("pending node {pending_id} is not registered"),
            ));
        };
        self.pending_kinds.remove(&pending_id);
        let pending = suspension.pending.clone();
        let node_id = EvalNodeId(pending.node_id);
        let resume_node = suspension.resume_node;
        match &result {
            Ok(value) => self.record_node_state(node_id, EvalNodeState::Ready(value.clone())),
            Err(error) => self.record_node_state(node_id, EvalNodeState::Poison(error.clone())),
        }
        let work = self.enqueue_ready_task(
            ReadyTask::Resume(ScheduledResume { suspension, result }),
            EvalWorkKind::ResumePending,
            resume_node,
            Some(pending.clone()),
        );
        self.record_node_state(
            resume_node,
            EvalNodeState::Runnable {
                work: work.id,
                pending: Some(pending.clone()),
            },
        );
        Ok(())
    }

    fn register_suspension(&mut self, suspension: EvalSuspension) {
        self.pending_resumes
            .insert(suspension.pending.node_id, suspension);
    }

    fn peek_ready_task(&self) -> Option<ReadyTaskRecord> {
        let work_id = *self.ready_queue.front()?;
        self.ready_tasks.get(&work_id).cloned()
    }

    fn enqueue_ready_task(
        &mut self,
        task: ReadyTask,
        kind: EvalWorkKind,
        node_id: EvalNodeId,
        pending: Option<PendingRef>,
    ) -> EvalQueuedWork {
        let work = EvalQueuedWork {
            id: EvalWorkId(self.next_work_id),
            kind,
            node_id,
            pending,
        };
        self.next_work_id += 1;
        self.ready_queue.push_back(work.id);
        self.ready_tasks.insert(
            work.id,
            ReadyTaskRecord {
                work: work.clone(),
                task,
            },
        );
        work
    }
}

fn execute_ready_task(
    source_text: &str,
    source_id: &str,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
    checkpoint: EvalSessionCheckpoint,
    record: ReadyTaskRecord,
) -> Result<(EvalSessionCheckpoint, EvalSessionStep), EvalError> {
    let EvalSessionCheckpoint {
        mut scheduler,
        mut trace,
        mut completed_work,
        mut completed,
    } = checkpoint;

    let source = SourceContext::new(source_id.to_string(), source_text);
    let mut state = EvalState::new(
        source_text,
        &source,
        runtime,
        place,
        &mut trace,
        &mut scheduler,
        Some(record.work.id),
        true,
    );

    let step = match record.task {
        ReadyTask::StartEntry => {
            let prepared = prepare_entry(&mut state, source_text, entry, args)?;
            let node_id = state.alloc_node_id();
            let body_kind = expr_kind(&prepared.body);
            let body_source = state.source.span_for_node(&prepared.body);
            state
                .scheduler
                .record_dependency(ENTRY_WORK_NODE_ID, node_id);
            state.scheduler.record_node(EvalNodeRecord {
                node_id,
                parent_id: Some(ENTRY_WORK_NODE_ID),
                kind: EvalNodeKind::Expr { kind: body_kind },
                source: Some(body_source),
            });
            let work = state.scheduler.enqueue_ready_task(
                ReadyTask::EvaluateExpr(ScheduledExpr {
                    expr: expr_handle(&prepared.body),
                    env: prepared.env,
                }),
                EvalWorkKind::EvaluateExpr,
                node_id,
                None,
            );
            state.scheduler.record_node_state(
                node_id,
                EvalNodeState::Runnable {
                    work: work.id,
                    pending: None,
                },
            );
            state.scheduler.record_node_state(
                ENTRY_WORK_NODE_ID,
                EvalNodeState::Runnable {
                    work: work.id,
                    pending: None,
                },
            );
            completed_work.push(EvalCompletedWork {
                work: record.work,
                outcome: EvalCompletedWorkOutcome::Progressed,
            });
            return Ok((
                EvalSessionCheckpoint {
                    scheduler,
                    trace,
                    completed_work,
                    completed,
                },
                EvalSessionStep::Progressed,
            ));
        }
        ReadyTask::EvaluateExpr(scheduled) => {
            let mut env = scheduled.env;
            let expr = resolve_expr_handle(state.source_text, scheduled.expr)?;
            eval_expr_with_node_id(&mut state, record.work.node_id, &expr, &mut env)
        }
        ReadyTask::Resume(ready) => resume_suspension(&mut state, ready.suspension, ready.result),
    };

    let step = match step {
        Ok(EvalStep::Ready(value)) => {
            completed_work.push(EvalCompletedWork {
                work: record.work.clone(),
                outcome: EvalCompletedWorkOutcome::Ready(value.clone()),
            });
            if state
                .scheduler
                .resolve_child_dependency(record.work.node_id, Ok(value.clone()))?
            {
                EvalSessionStep::Progressed
            } else {
                state
                    .scheduler
                    .record_node_state(ENTRY_WORK_NODE_ID, EvalNodeState::Ready(value.clone()));
                completed = Some(Ok(value.clone()));
                EvalSessionStep::Ready(value)
            }
        }
        Ok(EvalStep::Suspended(suspension)) => {
            let pending = suspension.pending.clone();
            state.scheduler.register_suspension(suspension);
            match state.scheduler.pending_kind(&pending) {
                Some(PendingKind::External) => {
                    state.scheduler.record_node_state(
                        ENTRY_WORK_NODE_ID,
                        EvalNodeState::Pending(pending.clone()),
                    );
                    completed_work.push(EvalCompletedWork {
                        work: record.work,
                        outcome: EvalCompletedWorkOutcome::Pending(pending.clone()),
                    });
                    EvalSessionStep::Pending(pending)
                }
                Some(PendingKind::ChildDependency { dependency }) => {
                    state.scheduler.record_node_state(
                        record.work.node_id,
                        EvalNodeState::BlockedOnNode { dependency },
                    );
                    completed_work.push(EvalCompletedWork {
                        work: record.work,
                        outcome: EvalCompletedWorkOutcome::BlockedOnNode(dependency),
                    });
                    EvalSessionStep::Progressed
                }
                None => {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        format!("pending node {} has no registered kind", pending.node_id),
                    ));
                }
            }
        }
        Err(error) => {
            completed_work.push(EvalCompletedWork {
                work: record.work.clone(),
                outcome: EvalCompletedWorkOutcome::Poison(error.clone()),
            });
            if state
                .scheduler
                .resolve_child_dependency(record.work.node_id, Err(error.clone()))?
            {
                EvalSessionStep::Progressed
            } else {
                state
                    .scheduler
                    .record_node_state(ENTRY_WORK_NODE_ID, EvalNodeState::Poison(error.clone()));
                completed = Some(Err(error.clone()));
                EvalSessionStep::Poison(error)
            }
        }
    };

    Ok((
        EvalSessionCheckpoint {
            scheduler,
            trace,
            completed_work,
            completed,
        },
        step,
    ))
}

impl SourceContext {
    fn new(source_id: impl Into<String>, source: &str) -> Self {
        Self {
            source_id: source_id.into(),
            line_index: ParserLineIndex::new(source),
        }
    }

    fn span_for_node<T: AstNode>(&self, node: &T) -> SourceSpan {
        self.span_for_range(trimmed_range(node.syntax()))
    }

    fn span_for_token(&self, token: &SyntaxToken) -> SourceSpan {
        self.span_for_range(token.text_range())
    }

    fn span_for_range(&self, range: TextRange) -> SourceSpan {
        SourceSpan::new(
            self.source_id.clone(),
            self.point_for_offset(range.start()),
            self.point_for_offset(range.end()),
        )
    }

    fn point_for_offset(&self, offset: TextSize) -> SourcePoint {
        let position = self.line_index.position(offset);
        SourcePoint::new(
            ByteOffset(u32::from(offset)),
            LineIndex(position.line),
            ColumnIndex(position.col),
        )
    }
}

fn find_function(root: &SourceFile, entry: &str) -> Result<FnDecl, EvalError> {
    root.items()
        .find_map(|item| match item {
            Item::FnDecl(function)
                if function
                    .name()
                    .as_ref()
                    .is_some_and(|name| name.text() == entry) =>
            {
                Some(function)
            }
            _ => None,
        })
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("function {entry} was not found"),
            )
        })
}

struct PreparedEntry {
    body: Expr,
    env: BTreeMap<String, Value>,
}

fn prepare_entry(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    source_text: &str,
    entry: &str,
    args: &[Value],
) -> Result<PreparedEntry, EvalError> {
    let parse = vx_parser::parse(source_text);
    if !parse.errors().is_empty() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("parse failed: {:?}", parse.errors()),
        ));
    }

    let root = SourceFile::cast_root(parse.syntax());
    let function = find_function(&root, entry)?;
    if function.keyword_parameters().next().is_some() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("functions with keyword parameters are not supported yet: {entry}"),
        ));
    }
    let parameters = function.parameters().collect::<Vec<_>>();
    if parameters.len() != args.len() {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "function {entry} expects {} positional argument(s), got {}",
                parameters.len(),
                args.len()
            ),
        ));
    }
    let body = function.body_expr().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            format!("function {entry} has no body expression"),
        )
    })?;
    let env = bind_arguments(state, &parameters, args)?;
    Ok(PreparedEntry { body, env })
}

fn evaluate_entry_once(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    source_text: &str,
    entry: &str,
    args: &[Value],
) -> Result<EvalStep, EvalError> {
    let prepared = prepare_entry(state, source_text, entry, args)?;
    let mut env = prepared.env;
    eval_expr(state, &prepared.body, &mut env)
}

fn bind_arguments(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    parameters: &[Parameter],
    args: &[Value],
) -> Result<BTreeMap<String, Value>, EvalError> {
    let mut env = BTreeMap::new();
    for (parameter, arg) in parameters.iter().zip(args.iter()) {
        let name = parameter.name().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                "function parameter has no name token",
            )
        })?;
        let label = name.text().to_string();
        let node_id = state.alloc_node_id();
        let parent_id = state.current_parent();
        state.emit(
            node_id,
            parent_id,
            Some(state.source.span_for_token(&name)),
            EvalEventKind::BindArgument {
                label: Arc::from(label.as_str()),
            },
        );
        state.scheduler.record_node(EvalNodeRecord {
            node_id,
            parent_id,
            kind: EvalNodeKind::ArgumentBinding {
                label: Arc::from(label.as_str()),
            },
            source: Some(state.source.span_for_token(&name)),
        });
        let bound = arg.clone().with_provenance(Provenance::runtime_input(
            state.source.span_for_token(&name),
            RuntimeInputKind::Argument,
            label.clone(),
        ));
        env.insert(label, bound);
    }
    Ok(env)
}

fn eval_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state.alloc_node_id();
    eval_expr_with_node_id(state, node_id, expr, env)
}

fn eval_expr_with_node_id(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let parent_id = state.current_parent();
    let kind = expr_kind(expr);
    let source = state.source.span_for_node(expr);
    state.scheduler.record_node(EvalNodeRecord {
        node_id,
        parent_id,
        kind: EvalNodeKind::Expr { kind },
        source: Some(source.clone()),
    });
    state.emit(
        node_id,
        parent_id,
        Some(source),
        EvalEventKind::EnterExpr { kind },
    );
    state.stack.push(node_id);

    let result = match expr {
        Expr::Block(block) => eval_block(state, block, env),
        Expr::Literal(literal) => eval_literal(state.source, literal).map(EvalStep::Ready),
        Expr::Paren(paren) => match paren.expr() {
            Some(inner) => match eval_child_expr(state, &inner, env)? {
                EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
                EvalStep::Suspended(suspension) => Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::FinishNode { node_id, parent_id },
                )),
            },
            None => Ok(EvalStep::Ready(Value::unit())),
        },
        Expr::NameRef(name_ref) => eval_name_ref(name_ref, env).map(EvalStep::Ready),
        Expr::Call(call) => eval_call(state, call, env),
        Expr::Array(array) => eval_array(state, array, env),
        Expr::Map(map) => eval_map(state, map, env),
        Expr::Fail(fail) => eval_fail(state, fail, env),
        Expr::If(if_expr) => eval_if(state, if_expr, env),
        Expr::StructLiteral(struct_literal) => eval_struct_literal(state, struct_literal, env),
        Expr::Field(field) => eval_field(state, field, env),
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported expression: {other:?}"),
        )),
    };

    let popped = state.stack.pop();
    debug_assert_eq!(popped, Some(node_id));
    match &result {
        Ok(EvalStep::Ready(value)) => {
            record_ready_step(state, node_id, parent_id, value);
        }
        Ok(EvalStep::Suspended(suspension)) => {
            record_pending_step(state, node_id, parent_id, &suspension.pending);
        }
        Err(error) => {
            record_poison_step(state, node_id, parent_id, error);
        }
    }

    result
}

fn eval_child_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    if !state.queue_child_work {
        let child_id = state.alloc_node_id();
        state.emit_current_dependency(child_id, Some(state.source.span_for_node(expr)));
        return eval_expr_with_node_id(state, child_id, expr, env);
    }
    let child_id = queue_child_expr(state, expr, env);
    let resume_node = state
        .current_parent()
        .expect("child expression should have an active parent node");
    Ok(wait_on_child_dependency(
        state,
        resume_node,
        child_id,
        Vec::new(),
    ))
}

fn eval_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    call: &CallExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let callee = call
        .callee()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "call expression has no callee"))?;
    let Expr::NameRef(name_ref) = callee else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "only direct name calls are supported yet",
        ));
    };
    let callee_name = name_ref
        .token()
        .map(|token| token.text().to_string())
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "callee name has no token"))?;
    let call_source = state.source.span_for_node(call);
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("call expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let positional_args = call.positional_args().into_iter().collect::<Vec<_>>();
    if state.queue_child_work && !positional_args.is_empty() {
        let child_nodes = positional_args
            .iter()
            .map(|arg| queue_child_expr(state, arg, env))
            .collect::<Vec<_>>();
        return Ok(wait_on_child_dependency(
            state,
            node_id,
            child_nodes[0],
            vec![ContinuationFrame::CallJoin {
                node_id,
                parent_id,
                callee_name,
                call_source,
                child_nodes,
            }],
        ));
    }
    continue_call_from_args(
        state,
        node_id,
        parent_id,
        callee_name,
        call_source,
        Vec::new(),
        positional_args.iter().map(expr_handle).collect(),
        env.clone(),
    )
}

fn eval_name_ref(
    name_ref: &NameRefExpr,
    env: &BTreeMap<String, Value>,
) -> Result<Value, EvalError> {
    let name = name_ref
        .token()
        .map(|token| token.text().to_string())
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "name reference has no token"))?;
    env.get(&name).cloned().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            format!("name {name} is not bound in this scope"),
        )
    })
}

fn continue_call_from_args(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    callee_name: String,
    call_source: SourceSpan,
    mut args: Vec<Value>,
    remaining_args: Vec<ExprHandle>,
    mut env: BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_args.into_iter();
    while let Some(arg_handle) = remaining.next() {
        let arg = resolve_expr_handle(state.source_text, arg_handle)?;
        match eval_dependent_expr(state, node_id, &arg, &mut env)? {
            EvalStep::Ready(value) => args.push(value),
            EvalStep::Suspended(suspension) => {
                let remaining_args = remaining.collect::<Vec<_>>();
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::CallAfterArg {
                        node_id,
                        parent_id,
                        callee_name,
                        call_source,
                        args,
                        remaining_args,
                        env,
                    },
                ));
            }
        }
    }
    state.emit(
        node_id,
        parent_id,
        Some(call_source.clone()),
        EvalEventKind::RuntimeCallStarted {
            callee: Arc::from(callee_name.as_str()),
            argc: args.len() as u32,
        },
    );
    match state
        .runtime
        .call(state.place, &callee_name, &args, &call_source)?
    {
        RuntimeCallOutcome::Ready(value) => {
            state.emit(
                node_id,
                parent_id,
                Some(call_source.clone()),
                EvalEventKind::RuntimeCallFinishedReady {
                    reuse: value.meta.reuse.clone(),
                    crossing: value.meta.crossing.clone(),
                },
            );
            Ok(EvalStep::Ready(value))
        }
        RuntimeCallOutcome::Pending { description } => {
            let pending = state.scheduler.alloc_pending(description);
            state.scheduler.record_node_state(
                EvalNodeId(pending.node_id),
                EvalNodeState::Pending(pending.clone()),
            );
            state.emit(
                node_id,
                parent_id,
                Some(call_source.clone()),
                EvalEventKind::RegisterPending {
                    pending: pending.clone(),
                },
            );
            state.emit(
                node_id,
                parent_id,
                Some(call_source.clone()),
                EvalEventKind::RuntimeCallFinishedPending {
                    pending: pending.clone(),
                },
            );
            Ok(wrap_suspended(
                pending,
                node_id,
                ContinuationFrame::RuntimeCall {
                    node_id,
                    parent_id,
                    call_source,
                },
            ))
        }
        RuntimeCallOutcome::Unsupported => {
            state.emit(
                node_id,
                parent_id,
                Some(call_source),
                EvalEventKind::RuntimeCallFinishedPoison {
                    kind: EvalErrorKind::Internal,
                    message: Arc::from(format!("call target {callee_name} is not supported yet")),
                },
            );
            Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("call target {callee_name} is not supported yet"),
            ))
        }
    }
}

fn eval_struct_literal(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    struct_literal: &StructLiteralExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let type_name = struct_literal
        .name()
        .map(|name| Arc::<str>::from(name.text()))
        .ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "struct literal has no type name")
        })?;
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("struct literal should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let struct_source = state.source.span_for_node(struct_literal);
    let remaining_fields = struct_literal
        .fields()
        .map(|field| {
            let name = Arc::<str>::from(
                field
                    .name()
                    .map(|name| name.text().to_string())
                    .ok_or_else(|| {
                        EvalError::new(EvalErrorKind::Internal, "struct field has no name")
                    })?,
            );
            let expr = field.expr().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "struct field has no value")
            })?;
            Ok((name, expr_handle(&expr)))
        })
        .collect::<Result<Vec<_>, EvalError>>()?;
    if state.queue_child_work && !remaining_fields.is_empty() {
        let field_names = remaining_fields
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let child_nodes = remaining_fields
            .iter()
            .map(|(_, expr)| queue_child_expr_handle(state, *expr, env))
            .collect::<Vec<_>>();
        return Ok(wait_on_child_dependency(
            state,
            node_id,
            child_nodes[0],
            vec![ContinuationFrame::StructJoin {
                node_id,
                parent_id,
                type_name,
                field_names,
                child_nodes,
                struct_source,
            }],
        ));
    }
    continue_struct_fields(
        state,
        node_id,
        parent_id,
        type_name,
        Vec::new(),
        remaining_fields,
        env.clone(),
        struct_source,
    )
}

fn eval_block(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    block: &BlockExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("block should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    continue_block_statements(
        state,
        node_id,
        parent_id,
        env.clone(),
        Value::unit(),
        block.statements().map(|stmt| stmt_handle(&stmt)).collect(),
    )
}

fn eval_array(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    array: &ArrayExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("array should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    if state.queue_child_work {
        let mut child_nodes = Vec::new();
        for item in array.items() {
            match item {
                ArrayItem::Expr(expr) => child_nodes.push(queue_child_expr(state, &expr, env)),
                ArrayItem::Spread(_) => {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        "array spread is not supported yet",
                    ));
                }
            }
        }
        if child_nodes.is_empty() {
            let provenance = Provenance::literal(state.source.span_for_node(array));
            return Ok(EvalStep::Ready(Value::array(Vec::new()).with_meta(
                ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue),
            )));
        }
        return Ok(wait_on_child_dependency(
            state,
            node_id,
            child_nodes[0],
            vec![ContinuationFrame::ArrayJoin {
                node_id,
                parent_id,
                child_nodes,
                array_source: state.source.span_for_node(array),
            }],
        ));
    }
    continue_array_items(
        state,
        node_id,
        parent_id,
        Vec::new(),
        array
            .items()
            .map(|item| match item {
                ArrayItem::Expr(expr) => Ok(expr_handle(&expr)),
                ArrayItem::Spread(_) => Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "array spread is not supported yet",
                )),
            })
            .collect::<Result<Vec<_>, EvalError>>()?,
        env.clone(),
        state.source.span_for_node(array),
    )
}

fn eval_map(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    map: &MapExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("map should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let entries = map
        .entries()
        .map(|entry| {
            let key_expr = entry.key().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "map entry has no explicit key")
            })?;
            let value_expr = entry
                .value()
                .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "map entry has no value"))?;
            Ok((expr_handle(&key_expr), expr_handle(&value_expr)))
        })
        .collect::<Result<Vec<_>, EvalError>>()?;
    continue_map_entries(
        state,
        node_id,
        parent_id,
        Vec::new(),
        entries,
        env.clone(),
        state.source.span_for_node(map),
    )
}

fn eval_fail(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    fail: &FailExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("fail expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let message = match eval_child_expr(
        state,
        &fail.message().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "fail expression has no message")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::FailMessage { node_id, parent_id },
            ));
        }
    };
    let ValueRep::String(message) = message.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "fail message must evaluate to String",
        ));
    };
    Err(EvalError::new(EvalErrorKind::UserFail, message.to_string()))
}

fn eval_if(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    if_expr: &IfExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("if expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let condition = match eval_child_expr(
        state,
        &if_expr.condition().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "if expression has no condition")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::IfCondition {
                    node_id,
                    parent_id,
                    then_branch: if_expr
                        .then_branch()
                        .ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "if branch is missing")
                        })
                        .map(|expr| expr_handle(&expr))?,
                    else_branch: if_expr
                        .else_branch()
                        .ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "if branch is missing")
                        })
                        .map(|expr| expr_handle(&expr))?,
                    env: env.clone(),
                },
            ));
        }
    };
    let ValueRep::Bool(value) = condition.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "if condition must evaluate to Bool",
        ));
    };
    let branch = if value {
        if_expr.then_branch()
    } else {
        if_expr.else_branch()
    }
    .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "if branch is missing"))?;
    match eval_child_expr(state, &branch, env)? {
        EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
        EvalStep::Suspended(suspension) => Ok(extend_suspension(
            suspension,
            ContinuationFrame::FinishNode { node_id, parent_id },
        )),
    }
}

fn eval_field(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    field: &FieldExpr,
    env: &mut BTreeMap<String, Value>,
) -> Result<EvalStep, EvalError> {
    let node_id = state
        .stack
        .last()
        .copied()
        .expect("field expression should have an active trace node");
    let parent_id = state.stack.iter().rev().nth(1).copied();
    let receiver = match eval_child_expr(
        state,
        &field.receiver().ok_or_else(|| {
            EvalError::new(EvalErrorKind::Internal, "field expression has no receiver")
        })?,
        env,
    )? {
        EvalStep::Ready(value) => value,
        EvalStep::Suspended(suspension) => {
            let field_name = field
                .field_name()
                .map(|name| name.text().to_string())
                .ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "field expression has no field name",
                    )
                })?;
            return Ok(extend_suspension(
                suspension,
                ContinuationFrame::FieldReceiver {
                    node_id,
                    parent_id,
                    field_name,
                },
            ));
        }
    };
    project_struct_field(
        receiver,
        field
            .field_name()
            .map(|name| name.text().to_string())
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "field expression has no field name",
                )
            })?,
    )
    .map(EvalStep::Ready)
}

fn continue_block_statements(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut scope: BTreeMap<String, Value>,
    mut last: Value,
    statements: Vec<StmtHandle>,
) -> Result<EvalStep, EvalError> {
    let mut remaining = statements.into_iter();
    while let Some(statement_handle) = remaining.next() {
        let statement = resolve_stmt_handle(state.source_text, statement_handle)?;
        match statement {
            Stmt::Let(let_stmt) => {
                let name_token = let_stmt.name().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no name")
                })?;
                let name = name_token.text().to_string();
                let name_source = state.source.span_for_token(&name_token);
                let expr = let_stmt.expr().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no value")
                })?;
                match eval_dependent_expr(state, node_id, &expr, &mut scope)? {
                    EvalStep::Ready(value) => {
                        scope.insert(
                            name.clone(),
                            value.with_provenance(Provenance::binding(name_source.clone(), name)),
                        );
                    }
                    EvalStep::Suspended(suspension) => {
                        return Ok(extend_suspension(
                            suspension,
                            ContinuationFrame::BlockAfterLet {
                                node_id,
                                parent_id,
                                scope,
                                last,
                                name,
                                name_source,
                                remaining_statements: remaining.collect(),
                            },
                        ));
                    }
                }
            }
            Stmt::Expr(expr) => match eval_dependent_expr(state, node_id, &expr, &mut scope)? {
                EvalStep::Ready(value) => last = value,
                EvalStep::Suspended(suspension) => {
                    return Ok(extend_suspension(
                        suspension,
                        ContinuationFrame::BlockAfterExpr {
                            node_id,
                            parent_id,
                            scope,
                            remaining_statements: remaining.collect(),
                        },
                    ));
                }
            },
            other => {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!("unsupported block statement: {other:?}"),
                ));
            }
        }
    }
    Ok(EvalStep::Ready(last))
}

fn continue_array_items(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut items: Vec<Value>,
    remaining_items: Vec<ExprHandle>,
    mut env: BTreeMap<String, Value>,
    array_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_items.into_iter();
    while let Some(item_handle) = remaining.next() {
        let expr = resolve_expr_handle(state.source_text, item_handle)?;
        match eval_dependent_expr(state, node_id, &expr, &mut env)? {
            EvalStep::Ready(value) => items.push(value),
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayAfterItem {
                        node_id,
                        parent_id,
                        items,
                        remaining_items: remaining.collect(),
                        env,
                        array_source,
                    },
                ));
            }
        }
    }
    let provenance = Provenance::literal(array_source);
    let meta = aggregate_child_meta(provenance, items.iter());
    Ok(EvalStep::Ready(Value::array(items).with_meta(meta)))
}

fn queue_child_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: &Expr,
    env: &BTreeMap<String, Value>,
) -> EvalNodeId {
    queue_child_expr_handle(state, expr_handle(expr), env)
}

fn queue_child_expr_handle(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    expr: ExprHandle,
    env: &BTreeMap<String, Value>,
) -> EvalNodeId {
    let child_id = state.alloc_node_id();
    let resolved = resolve_expr_handle(state.source_text, expr)
        .expect("queued child expression handle should resolve");
    state.emit_current_dependency(child_id, Some(state.source.span_for_node(&resolved)));
    state.scheduler.record_node(EvalNodeRecord {
        node_id: child_id,
        parent_id: state.current_parent(),
        kind: EvalNodeKind::Expr {
            kind: expr_kind(&resolved),
        },
        source: Some(state.source.span_for_node(&resolved)),
    });
    let work = state.scheduler.enqueue_ready_task(
        ReadyTask::EvaluateExpr(ScheduledExpr {
            expr,
            env: env.clone(),
        }),
        EvalWorkKind::EvaluateExpr,
        child_id,
        None,
    );
    state.scheduler.record_node_state(
        child_id,
        EvalNodeState::Runnable {
            work: work.id,
            pending: None,
        },
    );
    child_id
}

fn wait_on_child_dependency(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    resume_node: EvalNodeId,
    dependency: EvalNodeId,
    frames: Vec<ContinuationFrame>,
) -> EvalStep {
    let pending = state.scheduler.alloc_child_dependency_pending(dependency);
    EvalStep::Suspended(EvalSuspension {
        resume_node,
        pending,
        frames,
    })
}

fn continue_map_entries(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut entries: Vec<(Value, Value)>,
    remaining_entries: Vec<(ExprHandle, ExprHandle)>,
    mut env: BTreeMap<String, Value>,
    map_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_entries.into_iter();
    while let Some((key_handle, value_handle)) = remaining.next() {
        let key_expr = resolve_expr_handle(state.source_text, key_handle)?;
        let key = match eval_dependent_expr(state, node_id, &key_expr, &mut env)? {
            EvalStep::Ready(value) => value,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::MapAfterKey {
                        node_id,
                        parent_id,
                        entries,
                        value_expr: value_handle,
                        remaining_entries: remaining.collect(),
                        env,
                        map_source,
                    },
                ));
            }
        };
        let value_expr = resolve_expr_handle(state.source_text, value_handle)?;
        let value = match eval_dependent_expr(state, node_id, &value_expr, &mut env)? {
            EvalStep::Ready(value) => value,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::MapAfterValue {
                        node_id,
                        parent_id,
                        entries,
                        key,
                        remaining_entries: remaining.collect(),
                        env,
                        map_source,
                    },
                ));
            }
        };
        entries.push((key, value));
    }
    let provenance = Provenance::literal(map_source);
    let meta = aggregate_child_meta(
        provenance,
        entries.iter().flat_map(|(key, value)| [key, value]),
    );
    Ok(EvalStep::Ready(Value::map(entries).with_meta(meta)))
}

fn continue_struct_fields(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    type_name: Arc<str>,
    mut fields: Vec<(Arc<str>, Value)>,
    remaining_fields: Vec<(Arc<str>, ExprHandle)>,
    mut env: BTreeMap<String, Value>,
    struct_source: SourceSpan,
) -> Result<EvalStep, EvalError> {
    let mut remaining = remaining_fields.into_iter();
    while let Some((name, expr_handle)) = remaining.next() {
        let expr = resolve_expr_handle(state.source_text, expr_handle)?;
        match eval_dependent_expr(state, node_id, &expr, &mut env)? {
            EvalStep::Ready(value) => fields.push((name, value)),
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::StructAfterField {
                        node_id,
                        parent_id,
                        type_name,
                        fields,
                        name,
                        remaining_fields: remaining.collect(),
                        env,
                        struct_source,
                    },
                ));
            }
        }
    }
    let provenance = Provenance::literal(struct_source);
    let meta = aggregate_child_meta(provenance, fields.iter().map(|(_, value)| value));
    Ok(EvalStep::Ready(
        Value::structure(type_name, fields).with_meta(meta),
    ))
}

fn project_struct_field(receiver: Value, field_name: String) -> Result<Value, EvalError> {
    let ValueRep::Struct(StructValue { fields, .. }) = &receiver.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("cannot project field {field_name} from non-struct value"),
        ));
    };
    let value = fields
        .iter()
        .find_map(|(name, value)| (name.as_ref() == field_name).then(|| value.clone()))
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::TypeError,
                format!("struct field {field_name} was not found"),
            )
        })?;
    Ok(value.project(ProjectionStep::Field(field_name.into())))
}

fn resume_suspension(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    suspension: EvalSuspension,
    result: Result<Value, EvalError>,
) -> Result<EvalStep, EvalError> {
    let mut current = result;
    let frames = suspension.frames;
    let mut index = 0;
    while index < frames.len() {
        match resume_frame(state, frames[index].clone(), current)? {
            ResumeOutcome::Continue(next) => current = next,
            ResumeOutcome::Suspended(mut new_suspension) => {
                new_suspension
                    .frames
                    .extend(frames[index + 1..].iter().cloned());
                return Ok(EvalStep::Suspended(new_suspension));
            }
        }
        index += 1;
    }
    match current {
        Ok(value) => Ok(EvalStep::Ready(value)),
        Err(error) => Err(error),
    }
}

fn resume_frame(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    frame: ContinuationFrame,
    input: Result<Value, EvalError>,
) -> Result<ResumeOutcome, EvalError> {
    match frame {
        ContinuationFrame::FinishNode { node_id, parent_id } => Ok(match input {
            Ok(value) => {
                record_ready_step(state, node_id, parent_id, &value);
                ResumeOutcome::Continue(Ok(value))
            }
            Err(error) => {
                record_poison_step(state, node_id, parent_id, &error);
                ResumeOutcome::Continue(Err(error))
            }
        }),
        ContinuationFrame::RuntimeCall {
            node_id,
            parent_id,
            call_source,
        } => Ok(match input {
            Ok(value) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(call_source),
                    EvalEventKind::RuntimeCallFinishedReady {
                        reuse: value.meta.reuse.clone(),
                        crossing: value.meta.crossing.clone(),
                    },
                );
                record_ready_step(state, node_id, parent_id, &value);
                ResumeOutcome::Continue(Ok(value))
            }
            Err(error) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(call_source),
                    EvalEventKind::RuntimeCallFinishedPoison {
                        kind: error.kind,
                        message: error.message.clone(),
                    },
                );
                record_poison_step(state, node_id, parent_id, &error);
                ResumeOutcome::Continue(Err(error))
            }
        }),
        ContinuationFrame::CallJoin {
            node_id,
            parent_id,
            callee_name,
            call_source,
            child_nodes,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut args = Vec::with_capacity(child_nodes.len());
                    for child_node in &child_nodes {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => args.push(value.clone()),
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(*child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    frames: vec![ContinuationFrame::CallJoin {
                                        node_id,
                                        parent_id,
                                        callee_name,
                                        call_source,
                                        child_nodes: child_nodes.clone(),
                                    }],
                                }));
                            }
                        }
                    }
                    continue_call_from_args(
                        state,
                        node_id,
                        parent_id,
                        callee_name,
                        call_source,
                        args,
                        Vec::new(),
                        BTreeMap::new(),
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BlockAfterLet {
            node_id,
            parent_id,
            mut scope,
            last,
            name,
            name_source,
            remaining_statements,
        } => {
            let result = match input {
                Ok(value) => {
                    scope.insert(
                        name.clone(),
                        value.with_provenance(Provenance::binding(name_source, name)),
                    );
                    continue_block_statements(
                        state,
                        node_id,
                        parent_id,
                        scope,
                        last,
                        remaining_statements,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BlockAfterExpr {
            node_id,
            parent_id,
            scope,
            remaining_statements,
        } => {
            let result = match input {
                Ok(value) => continue_block_statements(
                    state,
                    node_id,
                    parent_id,
                    scope,
                    value,
                    remaining_statements,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::CallAfterArg {
            node_id,
            parent_id,
            callee_name,
            call_source,
            mut args,
            remaining_args,
            env,
        } => {
            let result = match input {
                Ok(value) => {
                    args.push(value);
                    continue_call_from_args(
                        state,
                        node_id,
                        parent_id,
                        callee_name,
                        call_source,
                        args,
                        remaining_args,
                        env,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayAfterItem {
            node_id,
            parent_id,
            mut items,
            remaining_items,
            env,
            array_source,
        } => {
            let result = match input {
                Ok(value) => {
                    items.push(value);
                    continue_array_items(
                        state,
                        node_id,
                        parent_id,
                        items,
                        remaining_items,
                        env,
                        array_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayJoin {
            node_id,
            parent_id,
            child_nodes,
            array_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut items = Vec::with_capacity(child_nodes.len());
                    for child_node in &child_nodes {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => items.push(value.clone()),
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(*child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    frames: vec![ContinuationFrame::ArrayJoin {
                                        node_id,
                                        parent_id,
                                        child_nodes: child_nodes.clone(),
                                        array_source: array_source.clone(),
                                    }],
                                }));
                            }
                        }
                    }
                    let provenance = Provenance::literal(array_source);
                    let meta = aggregate_child_meta(provenance, items.iter());
                    Ok(EvalStep::Ready(Value::array(items).with_meta(meta)))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::MapAfterKey {
            node_id,
            parent_id,
            entries,
            value_expr,
            remaining_entries,
            mut env,
            map_source,
        } => {
            let result = match input {
                Ok(key) => {
                    let value_expr = resolve_expr_handle(state.source_text, value_expr)?;
                    let value = match eval_dependent_expr(state, node_id, &value_expr, &mut env)? {
                        EvalStep::Ready(value) => value,
                        EvalStep::Suspended(suspension) => {
                            return Ok(ResumeOutcome::Suspended(extend_resume(
                                suspension,
                                ContinuationFrame::MapAfterValue {
                                    node_id,
                                    parent_id,
                                    entries,
                                    key,
                                    remaining_entries,
                                    env,
                                    map_source,
                                },
                            )));
                        }
                    };
                    let mut entries = entries;
                    entries.push((key, value));
                    continue_map_entries(
                        state,
                        node_id,
                        parent_id,
                        entries,
                        remaining_entries,
                        env,
                        map_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::MapAfterValue {
            node_id,
            parent_id,
            mut entries,
            key,
            remaining_entries,
            env,
            map_source,
        } => {
            let result = match input {
                Ok(value) => {
                    entries.push((key, value));
                    continue_map_entries(
                        state,
                        node_id,
                        parent_id,
                        entries,
                        remaining_entries,
                        env,
                        map_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::FailMessage { node_id, parent_id } => {
            let result = match input {
                Ok(message) => {
                    let ValueRep::String(message) = message.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "fail message must evaluate to String",
                        ));
                    };
                    Err(EvalError::new(EvalErrorKind::UserFail, message.to_string()))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::IfCondition {
            node_id,
            parent_id,
            then_branch,
            else_branch,
            mut env,
        } => {
            let result = match input {
                Ok(condition) => {
                    let ValueRep::Bool(value) = condition.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "if condition must evaluate to Bool",
                        ));
                    };
                    let branch = resolve_expr_handle(
                        state.source_text,
                        if value { then_branch } else { else_branch },
                    )?;
                    match eval_dependent_expr(state, node_id, &branch, &mut env)? {
                        EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
                        EvalStep::Suspended(suspension) => Ok(extend_suspension(
                            suspension,
                            ContinuationFrame::FinishNode { node_id, parent_id },
                        )),
                    }
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StructAfterField {
            node_id,
            parent_id,
            type_name,
            mut fields,
            name,
            remaining_fields,
            env,
            struct_source,
        } => {
            let result = match input {
                Ok(value) => {
                    fields.push((name, value));
                    continue_struct_fields(
                        state,
                        node_id,
                        parent_id,
                        type_name,
                        fields,
                        remaining_fields,
                        env,
                        struct_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StructJoin {
            node_id,
            parent_id,
            type_name,
            field_names,
            child_nodes,
            struct_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut fields = Vec::with_capacity(child_nodes.len());
                    for (name, child_node) in field_names.iter().zip(child_nodes.iter()) {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => {
                                fields.push((name.clone(), value.clone()))
                            }
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(*child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    frames: vec![ContinuationFrame::StructJoin {
                                        node_id,
                                        parent_id,
                                        type_name,
                                        field_names: field_names.clone(),
                                        child_nodes: child_nodes.clone(),
                                        struct_source: struct_source.clone(),
                                    }],
                                }));
                            }
                        }
                    }
                    let provenance = Provenance::literal(struct_source);
                    let meta =
                        aggregate_child_meta(provenance, fields.iter().map(|(_, value)| value));
                    Ok(EvalStep::Ready(
                        Value::structure(type_name, fields).with_meta(meta),
                    ))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::FieldReceiver {
            node_id,
            parent_id,
            field_name,
        } => {
            let result = input.and_then(|receiver| project_struct_field(receiver, field_name));
            Ok(match result {
                Ok(value) => {
                    record_ready_step(state, node_id, parent_id, &value);
                    ResumeOutcome::Continue(Ok(value))
                }
                Err(error) => {
                    record_poison_step(state, node_id, parent_id, &error);
                    ResumeOutcome::Continue(Err(error))
                }
            })
        }
    }
}

fn extend_resume(mut suspension: EvalSuspension, frame: ContinuationFrame) -> EvalSuspension {
    suspension.frames.push(frame);
    suspension
}

fn expr_kind(expr: &Expr) -> EvalExprKind {
    match expr {
        Expr::Block(_) => EvalExprKind::Block,
        Expr::Literal(_) => EvalExprKind::Literal,
        Expr::Paren(_) => EvalExprKind::Paren,
        Expr::NameRef(_) => EvalExprKind::NameRef,
        Expr::Call(_) => EvalExprKind::Call,
        Expr::Array(_) => EvalExprKind::Array,
        Expr::Map(_) => EvalExprKind::Map,
        Expr::Fail(_) => EvalExprKind::Fail,
        Expr::If(_) => EvalExprKind::If,
        Expr::StructLiteral(_) => EvalExprKind::StructLiteral,
        Expr::Field(_) => EvalExprKind::Field,
        _ => EvalExprKind::Unsupported,
    }
}

fn aggregate_child_meta<'a>(
    provenance: Provenance,
    children: impl IntoIterator<Item = &'a Value>,
) -> ValueMeta {
    let mut meta = ValueMeta::new(provenance, ReuseClass::Stable, CrossingPolicy::ByValue);
    let mut saw_blocked = false;

    for child in children {
        meta.reuse = join_reuse(meta.reuse.clone(), child.meta.reuse.clone());
        meta = meta.with_claims(child.meta.claims.iter().cloned());
        saw_blocked |= child.meta.crossing == CrossingPolicy::Blocked;
    }

    if saw_blocked {
        meta.crossing = CrossingPolicy::Blocked;
    }

    meta
}

fn join_reuse(left: ReuseClass, right: ReuseClass) -> ReuseClass {
    match (left, right) {
        (ReuseClass::Live, _) | (_, ReuseClass::Live) => ReuseClass::Live,
        (ReuseClass::Observed, _) | (_, ReuseClass::Observed) => ReuseClass::Observed,
        (ReuseClass::Stable, ReuseClass::Stable) => ReuseClass::Stable,
    }
}

fn eval_literal(source: &SourceContext, literal: &LiteralExpr) -> Result<Value, EvalError> {
    let token = literal
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "literal has no token"))?;
    let literal_source = source.span_for_node(literal);
    match token.kind() {
        vx_parser::SyntaxKind::INT => {
            let value = token.text().parse::<u64>().map_err(|source| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("invalid integer literal {}: {source}", token.text()),
                )
            })?;
            Ok(Value::u64(value).with_provenance(Provenance::literal(literal_source)))
        }
        vx_parser::SyntaxKind::TRUE_KW => {
            Ok(Value::bool(true).with_provenance(Provenance::literal(literal_source)))
        }
        vx_parser::SyntaxKind::FALSE_KW => {
            Ok(Value::bool(false).with_provenance(Provenance::literal(literal_source)))
        }
        vx_parser::SyntaxKind::STRING => {
            let text = token.text();
            Ok(Value::string(&text[1..text.len() - 1])
                .with_provenance(Provenance::literal(literal_source)))
        }
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported literal token: {other:?}"),
        )),
    }
}
