use super::*;
use crate::program::CallCalleePlan;

pub(super) struct EvalState<'source, 'scheduler, Runtime, Sink> {
    pub(super) program: &'source CompiledProgram,
    pub(super) current_source: SourceId,
    pub(super) branch_context: EvalBranchContext,
    pub(super) runtime: &'source Runtime,
    pub(super) place: &'source str,
    pub(super) sink: &'source mut Sink,
    pub(super) scheduler: &'scheduler mut dyn EvalWorkerScheduler,
    pub(super) current_work: Option<EvalWorkId>,
    pub(super) queue_child_work: bool,
    pub(super) track_calls: bool,
    pub(super) stack: Vec<EvalNodeId>,
    pub(super) call_stack: Vec<EvalAuthoredCallFrame>,
    pub(super) work_invocation: Option<EvalAuthoredCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EvalSuspension {
    pub(super) resume_node: EvalNodeId,
    pub(super) pending: PendingRef,
    pub(super) branch_context: EvalBranchContext,
    pub(super) frames: Vec<ContinuationFrame>,
    pub(super) call_stack: Vec<EvalAuthoredCallFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InterpolatedPart {
    Text(Arc<str>),
    Expr(ExprBodyRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ArrayEvalItem {
    Expr(ExprBodyRef),
    Spread(ExprBodyRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ArrayJoinItem {
    Expr(EvalNodeId),
    Spread(EvalNodeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StreamJoinItem {
    Pending {
        child_node: EvalNodeId,
        waiting_on: StreamPendingKind,
        remaining_stages: Vec<StreamStage>,
    },
    Ready(Value),
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StreamPendingKind {
    SourceCall,
    Map,
    Filter { input: Value },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StreamStage {
    Map(FunctionRef),
    Filter(FunctionRef),
    Flatten,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StructLiteralPart {
    Field { name: Arc<str>, expr: ExprBodyRef },
    Spread(ExprBodyRef),
}

pub(super) type LetPattern = crate::program::LetPattern;
pub(super) type MatchFieldPattern = crate::program::MatchFieldPattern;
pub(super) type MatchPattern = crate::program::MatchPattern;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BytecodeState {
    pub(super) scope: EvalEnv,
    pub(super) locals: Vec<Option<Value>>,
    pub(super) last: Value,
    pub(super) body: BytecodeBodyId,
    pub(super) pc: usize,
    pub(super) acc: Option<Value>,
    pub(super) temp: Option<Value>,
    pub(super) pending: Option<BytecodePendingState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlannedCallResumeKind {
    Callee,
    Arg,
    JoinArgs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedCallState {
    pub(super) plan_source: SourceId,
    pub(super) call_source: SourceSpan,
    pub(super) callee_plan: Option<CallCalleePlan>,
    pub(super) type_args: Arc<[Ty]>,
    pub(super) positional_args: Arc<[ExprBodyRef]>,
    pub(super) env: EvalEnv,
    pub(super) callee: Option<Value>,
    pub(super) args: Vec<Value>,
    pub(super) next_arg_index: usize,
    pub(super) queued_arg_nodes: Vec<EvalNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BytecodePendingState {
    Array {
        array: BytecodeArrayState,
    },
    ArrayJoin {
        child_nodes: Vec<ArrayJoinItem>,
        array_source: SourceSpan,
    },
    Tuple {
        tuple: BytecodeTupleState,
    },
    TupleJoin {
        child_nodes: Vec<EvalNodeId>,
        tuple_source: SourceSpan,
    },
    Range {
        range: BytecodeRangeState,
        pending: RangeResumeKind,
    },
    RangeJoin {
        start_child: Option<EvalNodeId>,
        end_child: Option<EvalNodeId>,
        range_source: SourceSpan,
        inclusive_end: bool,
    },
    Map {
        map: MapState,
        pending: MapResumeKind,
    },
    Struct {
        struct_state: StructState,
    },
    StructJoin {
        type_name: Arc<str>,
        field_names: Vec<Arc<str>>,
        child_nodes: Vec<EvalNodeId>,
        struct_source: SourceSpan,
    },
    InterpolatedString {
        string: BytecodeInterpolatedStringState,
    },
    InterpolatedStringJoin {
        parts: Vec<InterpolatedPart>,
        expr_nodes: Vec<EvalNodeId>,
        string_source: SourceSpan,
    },
    Fallible {
        fallible_source: SourceSpan,
    },
    Call {
        call: PlannedCallState,
        pending: PlannedCallResumeKind,
    },
    Match {
        match_state: MatchState,
    },
    Where {
        where_state: WhereState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BytecodeArrayState {
    pub(super) items: Vec<Value>,
    pub(super) spread: bool,
    pub(super) remaining_items: Vec<ArrayEvalItem>,
    pub(super) array_source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BytecodeTupleState {
    pub(super) items: Vec<Value>,
    pub(super) remaining_items: Vec<ExprBodyRef>,
    pub(super) tuple_source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BytecodeRangeState {
    pub(super) start: Option<Value>,
    pub(super) end: Option<ExprBodyRef>,
    pub(super) range_source: SourceSpan,
    pub(super) inclusive_end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BytecodeInterpolatedStringState {
    pub(super) segments: Vec<String>,
    pub(super) remaining_parts: Vec<InterpolatedPart>,
    pub(super) string_source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FieldState {
    pub(super) field_expr: ExprId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RangeResumeKind {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MatchState {
    pub(super) match_plan: MatchPlan,
    pub(super) env: EvalEnv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchAssumptionState {
    pub(super) name: Arc<str>,
    pub(super) candidates: Vec<Value>,
    pub(super) resolver: FunctionRef,
    pub(super) index: usize,
    pub(super) attempts: Vec<Value>,
    pub(super) root_search_start: usize,
    pub(super) search_start_index: usize,
    pub(super) prior_branch_context: EvalBranchContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WhereCallPlan {
    pub(super) call_source: SourceSpan,
    pub(super) callee: Value,
    pub(super) type_args: Arc<[Ty]>,
    pub(super) positional_args: Arc<[ExprBodyRef]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WhereState {
    pub(super) call: WhereCallPlan,
    pub(super) pending_keyword_name: Option<Arc<str>>,
    pub(super) keyword_bindings: Vec<(Arc<str>, Value)>,
    pub(super) remaining_keyword_bindings: Vec<(Arc<str>, ExprBodyRef)>,
    pub(super) env: EvalEnv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MapResumeKind {
    Key,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MapState {
    pub(super) entries: Vec<(Value, Value)>,
    pub(super) key: Option<Value>,
    pub(super) value_expr: Option<ExprBodyRef>,
    pub(super) remaining_entries: Vec<(ExprBodyRef, ExprBodyRef)>,
    pub(super) env: EvalEnv,
    pub(super) map_source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StructState {
    pub(super) type_name: Arc<str>,
    pub(super) fields: Vec<(Arc<str>, Value)>,
    pub(super) pending_part: Option<StructLiteralPart>,
    pub(super) remaining_parts: Vec<StructLiteralPart>,
    pub(super) env: EvalEnv,
    pub(super) struct_source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SlotState {
    pub(super) call_source: SourceSpan,
    pub(super) slot_id: Arc<str>,
    pub(super) witness: Value,
    pub(super) action: SlotAction<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayFindState {
    pub(super) items: Vec<Value>,
    pub(super) predicate: FunctionRef,
    pub(super) index: usize,
    pub(super) wrap_in_option: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayAnyState {
    pub(super) items: Vec<Value>,
    pub(super) predicate: FunctionRef,
    pub(super) index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayFoldState {
    pub(super) items: Arc<[Value]>,
    pub(super) step: FunctionRef,
    pub(super) index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayFilterMapState {
    pub(super) items: Arc<[Value]>,
    pub(super) mapper: FunctionRef,
    pub(super) mapped: Vec<Value>,
    pub(super) index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContinuationFrame {
    Fallible {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        fallible_source: SourceSpan,
    },
    RuntimeCall {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        call_source: SourceSpan,
    },
    Slot {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        slot: SlotState,
    },
    PlannedCall {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        call: PlannedCallState,
        pending: PlannedCallResumeKind,
    },
    Bytecode {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        bytecode: BytecodeState,
    },
    StreamToArrayJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<StreamJoinItem>,
        stream_provenance: Provenance,
    },
    StreamMergeJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<StreamJoinItem>,
        stream_provenance: Provenance,
    },
    StreamForEachJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<StreamJoinItem>,
        stream_provenance: Provenance,
    },
    StreamToMapJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<StreamJoinItem>,
        stream_provenance: Provenance,
    },
    Map {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        map: MapState,
        pending: MapResumeKind,
    },
    RestoreBranchContext {
        branch_context: EvalBranchContext,
    },
    SearchAssumption {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        search: SearchAssumptionState,
    },
    Struct {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        struct_state: StructState,
    },
    StructJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        type_name: Arc<str>,
        field_names: Vec<Arc<str>>,
        child_nodes: Vec<EvalNodeId>,
        struct_source: SourceSpan,
    },
    ArrayFind {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        find: ArrayFindState,
    },
    ArrayFold {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        fold: ArrayFoldState,
    },
    ArrayFilterMap {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        filter_map: ArrayFilterMapState,
    },
    ArrayAny {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        any: ArrayAnyState,
    },
    Field {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        field: FieldState,
    },
    Where {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        where_state: WhereState,
    },
}

#[derive(Debug, Clone)]
pub(super) enum EvalStep {
    Ready(Value),
    Suspended(EvalSuspension),
}

pub(super) enum ResumeOutcome {
    Continue(Result<Value, EvalError>),
    Suspended(EvalSuspension),
}

pub(super) type EvalEnv = OrdMap<String, Value>;

impl<'source, 'scheduler, Runtime: RuntimeHooks, Sink: EvalEventSink>
    EvalState<'source, 'scheduler, Runtime, Sink>
{
    pub(super) fn new(
        program: &'source CompiledProgram,
        current_source: SourceId,
        branch_context: EvalBranchContext,
        runtime: &'source Runtime,
        place: &'source str,
        sink: &'source mut Sink,
        scheduler: &'scheduler mut dyn EvalWorkerScheduler,
        current_work: Option<EvalWorkId>,
        queue_child_work: bool,
        track_calls: bool,
    ) -> Self {
        Self {
            program,
            current_source,
            branch_context,
            runtime,
            place,
            sink,
            scheduler,
            current_work,
            queue_child_work,
            track_calls,
            stack: Vec::new(),
            call_stack: Vec::new(),
            work_invocation: None,
        }
    }

    pub(super) fn alloc_node_id(&mut self) -> EvalNodeId {
        self.scheduler.alloc_node_id()
    }

    pub(super) fn current_parent(&self) -> Option<EvalNodeId> {
        self.stack.last().copied()
    }

    pub(super) fn current_source(&self) -> SourceId {
        self.current_source
    }

    pub(super) fn current_call(&self) -> Option<&EvalAuthoredCallFrame> {
        self.call_stack.last()
    }

    pub(super) fn current_invocation(&self) -> Option<EvalAuthoredCall> {
        let current = self.current_call()?;
        let caller = self.call_stack.iter().rev().nth(1);
        Some(EvalAuthoredCall {
            root_node_id: current.root_node_id,
            root_work_id: current.root_work_id,
            caller_node_id: caller.map(|frame| frame.root_node_id),
            caller_work_id: caller.and_then(|frame| frame.root_work_id),
            callee: current.callee.clone(),
            args_fingerprint: current.args_fingerprint.clone(),
            source: current.source.clone(),
        })
    }

    pub(super) fn with_source<T>(&mut self, source: SourceId, f: impl FnOnce(&mut Self) -> T) -> T {
        let previous = self.current_source;
        self.current_source = source;
        let result = f(self);
        self.current_source = previous;
        result
    }

    pub(super) fn branch_context(&self) -> &EvalBranchContext {
        &self.branch_context
    }

    pub(super) fn with_branch_context<T>(
        &mut self,
        branch_context: EvalBranchContext,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = self.branch_context.clone();
        self.branch_context = branch_context;
        let result = f(self);
        self.branch_context = previous;
        result
    }

    pub(super) fn expr_id_for(
        &self,
        handle: impl crate::program::IntoExprLookupId,
    ) -> Result<ExprId, EvalError> {
        self.program.expr_id(handle)
    }

    pub(super) fn emit(
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

    pub(super) fn emit_current_dependency(
        &mut self,
        dependency: EvalNodeId,
        source: Option<SourceSpan>,
    ) {
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

    pub(super) fn with_active_node<T>(
        &mut self,
        node_id: EvalNodeId,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.stack.push(node_id);
        let result = f(self);
        let popped = self.stack.pop();
        debug_assert_eq!(popped, Some(node_id));
        result
    }

    pub(super) fn with_call_stack<T>(
        &mut self,
        call_stack: Vec<EvalAuthoredCallFrame>,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.call_stack, call_stack);
        if let Some(invocation) = self.current_invocation() {
            self.work_invocation = Some(invocation);
        }
        let result = f(self);
        self.call_stack = previous;
        result
    }

    pub(super) fn with_authored_call<T>(
        &mut self,
        frame: EvalAuthoredCallFrame,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.call_stack.push(frame);
        self.work_invocation = self.current_invocation();
        let result = f(self);
        let popped = self.call_stack.pop();
        debug_assert!(popped.is_some());
        result
    }
}

pub(super) fn record_ready_step(
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

pub(super) fn record_pending_step(
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

pub(super) fn record_poison_step(
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

pub(super) fn wrap_suspended(
    state: &EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    branch_context: EvalBranchContext,
    pending: PendingRef,
    resume_node: EvalNodeId,
    frame: ContinuationFrame,
) -> EvalStep {
    EvalStep::Suspended(EvalSuspension {
        resume_node,
        pending,
        branch_context,
        frames: vec![frame],
        call_stack: state.call_stack.clone(),
    })
}

pub(super) fn extend_suspension(
    mut suspension: EvalSuspension,
    frame: ContinuationFrame,
) -> EvalStep {
    suspension.frames.push(frame);
    EvalStep::Suspended(suspension)
}

pub(super) fn conclude_resumed_node(
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

pub(super) fn empty_env() -> EvalEnv {
    OrdMap::new()
}

pub(super) fn env_mut(env: &mut EvalEnv) -> &mut OrdMap<String, Value> {
    env
}
