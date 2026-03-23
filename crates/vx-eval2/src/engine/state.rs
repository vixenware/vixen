use super::*;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExprHandle {
    pub(super) key: ExprId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InterpolatedPart {
    Text(Arc<str>),
    Expr(ExprHandle),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ArrayEvalItem {
    Expr(ExprHandle),
    Spread(ExprHandle),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayState {
    pub(super) items: Vec<Value>,
    pub(super) spread: bool,
    pub(super) remaining_items: Vec<ArrayEvalItem>,
    pub(super) env: EvalEnv,
    pub(super) array_source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TupleState {
    pub(super) items: Vec<Value>,
    pub(super) remaining_items: Vec<ExprHandle>,
    pub(super) env: EvalEnv,
    pub(super) tuple_source: SourceSpan,
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
    Field { name: Arc<str>, expr: ExprHandle },
    Spread(ExprHandle),
}

pub(super) type MatchArmPlan = crate::program::MatchArmPlan;
pub(super) type LetPattern = crate::program::LetPattern;
pub(super) type MatchFieldPattern = crate::program::MatchFieldPattern;
pub(super) type MatchPattern = crate::program::MatchPattern;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BlockResumeKind {
    Let { pattern: LetPattern },
    Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CallResumeKind {
    Callee,
    Arg,
    JoinArgs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CallState {
    pub(super) call_source: SourceSpan,
    pub(super) type_args: Arc<[Ty]>,
    pub(super) env: EvalEnv,
    pub(super) callee: Option<Value>,
    pub(super) args: Vec<Value>,
    pub(super) remaining_args: Arc<[ExprId]>,
    pub(super) next_arg_index: usize,
    pub(super) queued_arg_nodes: Vec<EvalNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FieldState {
    pub(super) field_name: Arc<str>,
    pub(super) field_source: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BinaryResumeKind {
    Lhs,
    Rhs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BinaryState {
    pub(super) operator: SyntaxKind,
    pub(super) rhs: ExprHandle,
    pub(super) env: EvalEnv,
    pub(super) lhs: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IfState {
    pub(super) then_branch: ExprHandle,
    pub(super) else_branch: ExprHandle,
    pub(super) env: EvalEnv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WhereState {
    pub(super) call: ExprHandle,
    pub(super) pending_keyword_name: Option<Arc<str>>,
    pub(super) keyword_bindings: Vec<(Arc<str>, Value)>,
    pub(super) remaining_keyword_bindings: Vec<(Arc<str>, ExprHandle)>,
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
    pub(super) value_expr: Option<ExprHandle>,
    pub(super) remaining_entries: Vec<(ExprHandle, ExprHandle)>,
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
pub(super) enum ContinuationFrame {
    FinishNode {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
    },
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
    SlotAfterResolver {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        call_source: SourceSpan,
        slot_id: Arc<str>,
        witness: Value,
        action: SlotAction<Value>,
    },
    Call {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        call: CallState,
        pending: CallResumeKind,
    },
    BlockBody {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        scope: EvalEnv,
        last: Value,
        body: BlockBodyId,
        next_step_index: usize,
        pending: BlockResumeKind,
    },
    Array {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        array: ArrayState,
    },
    ArrayJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        child_nodes: Vec<ArrayJoinItem>,
        array_source: SourceSpan,
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
    Tuple {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        tuple: TupleState,
    },
    TupleJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        child_nodes: Vec<EvalNodeId>,
        tuple_source: SourceSpan,
    },
    InterpolatedStringAfterExpr {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        segments: Vec<String>,
        remaining_parts: Vec<InterpolatedPart>,
        env: EvalEnv,
        string_source: SourceSpan,
    },
    InterpolatedStringJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        parts: Vec<InterpolatedPart>,
        expr_nodes: Vec<EvalNodeId>,
        string_source: SourceSpan,
    },
    RangeAfterStart {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        end: Option<ExprHandle>,
        env: EvalEnv,
        range_source: SourceSpan,
        inclusive_end: bool,
    },
    RangeAfterEnd {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        start: Option<Value>,
        range_source: SourceSpan,
        inclusive_end: bool,
    },
    RangeJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        start_child: Option<EvalNodeId>,
        end_child: Option<EvalNodeId>,
        range_source: SourceSpan,
        inclusive_end: bool,
    },
    Map {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        map: MapState,
        pending: MapResumeKind,
    },
    FailMessage {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
    },
    RestoreBranchContext {
        branch_context: EvalBranchContext,
    },
    SearchAssumptionAfterBranch {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        name: Arc<str>,
        candidates: Vec<Value>,
        resolver: FunctionRef,
        index: usize,
        attempts: Vec<Value>,
        root_search_start: usize,
        search_start_index: usize,
        prior_branch_context: EvalBranchContext,
    },
    MatchScrutinee {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        arms: Arc<[MatchArmPlan]>,
        env: EvalEnv,
    },
    PrefixOperand {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        operator: SyntaxKind,
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
    ArrayFindAfterPredicate {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        predicate: FunctionRef,
        index: usize,
        wrap_in_option: bool,
    },
    ArrayFoldAfterStep {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        step: FunctionRef,
        index: usize,
    },
    ArrayFilterMapAfterMapper {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        mapper: FunctionRef,
        mapped: Vec<Value>,
        index: usize,
    },
    ArrayAnyAfterPredicate {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        predicate: FunctionRef,
        index: usize,
    },
    Field {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        field: FieldState,
    },
    Binary {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        binary: BinaryState,
        pending: BinaryResumeKind,
    },
    If {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        if_state: IfState,
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

    pub(super) fn span_for_node<T: crate::program::HasHandle>(&self, node: &T) -> SourceSpan {
        self.program.span_for_handle(node.handle())
    }

    pub(super) fn span_for_token(&self, token: &Token) -> SourceSpan {
        self.program.span_for_token_ref(token)
    }

    pub(super) fn expr_handle(&self, expr: &Expr) -> ExprHandle {
        ExprHandle {
            key: self
                .program
                .expr_id(expr.handle())
                .expect("expression handle should resolve to an expression id"),
        }
    }

    pub(super) fn expr_handle_for(
        &self,
        handle: impl crate::program::IntoExprLookupId,
    ) -> Result<ExprHandle, EvalError> {
        Ok(ExprHandle {
            key: self.program.expr_id(handle)?,
        })
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

pub(super) fn eval_dependent_expr(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    expr: &Expr,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    if state.current_parent() == Some(node_id) {
        eval_child_expr(state, expr, env)
    } else {
        state.with_active_node(node_id, |state| eval_child_expr(state, expr, env))
    }
}

pub(super) fn eval_dependent_expr_handle(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    handle: ExprHandle,
    env: &mut EvalEnv,
) -> Result<EvalStep, EvalError> {
    let expr = state.program.resolve_expr_id(handle.key)?;
    state.with_source(handle.key.source(), |state| {
        eval_dependent_expr(state, node_id, &expr, env)
    })
}
