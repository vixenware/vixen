use super::*;

pub(super) struct EvalState<'source, 'scheduler, Runtime, Sink> {
    pub(super) program: &'source CompiledProgram,
    pub(super) current_source: SourceId,
    pub(super) branch_context: EvalBranchContext,
    pub(super) runtime: &'source Runtime,
    pub(super) place: &'source str,
    pub(super) sink: &'source mut Sink,
    pub(super) scheduler: &'scheduler mut EvalScheduler,
    pub(super) current_work: Option<EvalWorkId>,
    pub(super) queue_child_work: bool,
    pub(super) stack: Vec<EvalNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EvalSuspension {
    pub(super) resume_node: EvalNodeId,
    pub(super) pending: PendingRef,
    pub(super) branch_context: EvalBranchContext,
    pub(super) frames: Vec<ContinuationFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExprHandle {
    pub(super) key: ProgramNodeHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StmtHandle {
    pub(super) key: ProgramNodeHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContinuationFrame {
    FinishNode {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
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
    CallJoin {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        callee: Value,
        call_source: SourceSpan,
        type_args: Arc<[Ty]>,
        child_nodes: Vec<EvalNodeId>,
    },
    BlockAfterLet {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        scope: EvalEnv,
        last: Value,
        name: String,
        name_source: SourceSpan,
        remaining_statements: Vec<StmtHandle>,
    },
    BlockAfterExpr {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        scope: EvalEnv,
        remaining_statements: Vec<StmtHandle>,
    },
    CallAfterCallee {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        call_source: SourceSpan,
        type_args: Arc<[Ty]>,
        env: EvalEnv,
        args: Vec<Value>,
        remaining_args: Vec<ExprHandle>,
    },
    CallAfterArg {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        callee: Value,
        call_source: SourceSpan,
        type_args: Arc<[Ty]>,
        args: Vec<Value>,
        remaining_args: Vec<ExprHandle>,
        env: EvalEnv,
    },
    ArrayAfterItem {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        remaining_items: Vec<ExprHandle>,
        env: EvalEnv,
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
        env: EvalEnv,
        map_source: SourceSpan,
    },
    MapAfterValue {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        entries: Vec<(Value, Value)>,
        key: Value,
        remaining_entries: Vec<(ExprHandle, ExprHandle)>,
        env: EvalEnv,
        map_source: SourceSpan,
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
    IfCondition {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        then_branch: ExprHandle,
        else_branch: ExprHandle,
        env: EvalEnv,
    },
    BinaryLhs {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        operator: SyntaxKind,
        rhs: ExprHandle,
        env: EvalEnv,
    },
    BinaryRhs {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        lhs: Value,
        operator: SyntaxKind,
    },
    StructAfterField {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        type_name: Arc<str>,
        fields: Vec<(Arc<str>, Value)>,
        name: Arc<str>,
        remaining_fields: Vec<(Arc<str>, ExprHandle)>,
        env: EvalEnv,
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
    ArrayFindAfterPredicate {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        predicate: FunctionRef,
        index: usize,
    },
    ArrayFoldAfterStep {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        items: Vec<Value>,
        step: FunctionRef,
        index: usize,
    },
    FieldReceiver {
        node_id: EvalNodeId,
        parent_id: Option<EvalNodeId>,
        field_name: String,
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

pub(super) type EvalEnv = Arc<BTreeMap<String, Value>>;

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
        scheduler: &'scheduler mut EvalScheduler,
        current_work: Option<EvalWorkId>,
        queue_child_work: bool,
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
            stack: Vec::new(),
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

    pub(super) fn span_for_node<T: AstNode>(&self, node: &T) -> SourceSpan {
        self.program.span_for_node(self.current_source, node)
    }

    pub(super) fn span_for_token(&self, token: &vx_parser::SyntaxToken) -> SourceSpan {
        self.program.span_for_token(self.current_source, token)
    }

    pub(super) fn expr_handle(&self, expr: &Expr) -> ExprHandle {
        ExprHandle {
            key: ProgramNodeHandle::for_node(self.current_source, expr),
        }
    }

    pub(super) fn stmt_handle(&self, stmt: &Stmt) -> StmtHandle {
        StmtHandle {
            key: ProgramNodeHandle::for_node(self.current_source, stmt),
        }
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
    Arc::new(BTreeMap::new())
}

pub(super) fn env_mut(env: &mut EvalEnv) -> &mut BTreeMap<String, Value> {
    Arc::make_mut(env)
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
    let expr = state.program.resolve_expr(handle.key)?;
    state.with_source(handle.key.source(), |state| {
        eval_dependent_expr(state, node_id, &expr, env)
    })
}
