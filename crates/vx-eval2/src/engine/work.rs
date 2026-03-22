use super::*;

#[derive(Debug, Clone)]
pub(super) struct EvalScheduler {
    pub(super) next_wave_id: u64,
    pub(super) next_lease_namespace: u64,
    pub(super) next_lease_sequence: u64,
    pub(super) active_wave: Option<EvalWaveId>,
    pub(super) base: EvalSchedulerBase,
    pub(super) ready_queue: VecDeque<EvalWorkId>,
    pub(super) ready_tasks: BTreeMap<EvalWorkId, ReadyTaskRecord>,
    pub(super) in_flight_tasks: BTreeMap<EvalWorkId, InFlightTaskRecord>,
    pub(super) buffered_completions: BTreeMap<u64, EvalWorkCompletion>,
}

pub(super) const ENTRY_WORK_NODE_ID: EvalNodeId = EvalNodeId(0);

impl Default for EvalScheduler {
    fn default() -> Self {
        Self {
            next_wave_id: 1,
            next_lease_namespace: 1,
            next_lease_sequence: 1,
            active_wave: None,
            base: EvalSchedulerBase::default(),
            ready_queue: VecDeque::new(),
            ready_tasks: BTreeMap::new(),
            in_flight_tasks: BTreeMap::new(),
            buffered_completions: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetainedExprTask {
    pub(super) scheduled: ScheduledExpr,
    pub(super) branch_context: EvalBranchContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadyTaskRecord {
    pub(super) work: EvalQueuedWork,
    pub(super) task: ReadyTask,
    pub(super) branch_context: EvalBranchContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InFlightTaskRecord {
    pub(super) wave_id: EvalWaveId,
    pub(super) lease_sequence: u64,
    pub(super) record: ReadyTaskRecord,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LeaseNamespace {
    pub(super) id: u64,
    pub(super) next_node_local: u64,
    pub(super) next_work_local: u64,
}

#[derive(Debug, Clone)]
pub(super) struct ReadyTaskLease {
    pub(super) wave_id: EvalWaveId,
    pub(super) lease_sequence: u64,
    pub(super) namespace: Option<u64>,
    pub(super) record: ReadyTaskRecord,
}

#[derive(Debug, Clone, Copy)]
enum EvalPreparedIdAllocator {
    Direct {
        next_node_id: u64,
        next_work_id: u64,
    },
    Scoped {
        namespace: LeaseNamespace,
    },
}

impl EvalPreparedIdAllocator {
    fn from_base(base: &EvalPreparedSnapshot) -> Self {
        match base.lease_namespace {
            Some(id) => Self::Scoped {
                namespace: LeaseNamespace {
                    id,
                    next_node_local: 1,
                    next_work_local: 1,
                },
            },
            None => Self::Direct {
                next_node_id: base.base.next_id,
                next_work_id: base.base.next_work_id,
            },
        }
    }

    fn alloc_node_id(&mut self) -> EvalNodeId {
        match self {
            Self::Direct { next_node_id, .. } => {
                let node_id = EvalNodeId(*next_node_id);
                *next_node_id += 1;
                node_id
            }
            Self::Scoped { namespace } => {
                let node_id = EvalNodeId(lease_scoped_id(namespace.id, namespace.next_node_local));
                namespace.next_node_local += 1;
                node_id
            }
        }
    }

    fn alloc_work_id(&mut self) -> EvalWorkId {
        match self {
            Self::Direct { next_work_id, .. } => {
                let work_id = EvalWorkId(*next_work_id);
                *next_work_id += 1;
                work_id
            }
            Self::Scoped { namespace } => {
                let work_id = EvalWorkId(lease_scoped_id(namespace.id, namespace.next_work_local));
                namespace.next_work_local += 1;
                work_id
            }
        }
    }

    fn owner_next_id(&self, base: &EvalPreparedSnapshot) -> u64 {
        match self {
            Self::Direct { next_node_id, .. } => *next_node_id,
            Self::Scoped { .. } => base.base.next_id,
        }
    }

    fn owner_next_work_id(&self, base: &EvalPreparedSnapshot) -> u64 {
        match self {
            Self::Direct { next_work_id, .. } => *next_work_id,
            Self::Scoped { .. } => base.base.next_work_id,
        }
    }
}

struct EvalPreparedScheduler<'a> {
    base: &'a EvalPreparedSnapshot,
    allocator: EvalPreparedIdAllocator,
    delta: RecordedSchedulerDelta,
    queued: Vec<ReadyTaskRecord>,
    completed_invalidated: bool,
}

impl<'a> EvalPreparedScheduler<'a> {
    fn new(base: &'a EvalPreparedSnapshot) -> Self {
        Self {
            base,
            allocator: EvalPreparedIdAllocator::from_base(base),
            delta: RecordedSchedulerDelta::default(),
            queued: Vec::new(),
            completed_invalidated: false,
        }
    }

    fn finish(self) -> (EvalSchedulerDelta, bool) {
        let mut delta = self.delta.into_delta();
        delta.next_id = self.allocator.owner_next_id(self.base);
        delta.next_work_id = self.allocator.owner_next_work_id(self.base);
        delta.queued = self.queued;
        (delta, self.completed_invalidated)
    }

    fn node_record_local(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        self.delta
            .node_records
            .get(&node_id)
            .or_else(|| self.base.node_record(node_id))
    }

    fn node_state_local(&self, node_id: EvalNodeId) -> Option<&EvalNodeState> {
        self.delta
            .node_states
            .get(&node_id)
            .or_else(|| self.base.node_state(node_id))
    }

    fn pending_kind_local(&self, pending_id: u64) -> Option<PendingKind> {
        self.delta
            .pending_kinds
            .get(&pending_id)
            .copied()
            .or_else(|| self.base.base.pending_kinds.get(&pending_id).copied())
    }

    fn keyed_node_local(&self, node_id: EvalNodeId) -> Option<&KeyedNodeMemo> {
        self.delta
            .keyed_nodes
            .get(&node_id)
            .or_else(|| self.base.base.keyed_nodes.get(&node_id))
    }

    fn keyed_node_mut(&mut self, node_id: EvalNodeId) -> Option<&mut KeyedNodeMemo> {
        if !self.delta.keyed_nodes.contains_key(&node_id) {
            let base = self.base.base.keyed_nodes.get(&node_id)?.clone();
            self.delta.keyed_nodes.insert(node_id, base);
        }
        self.delta.keyed_nodes.get_mut(&node_id)
    }

    fn retained_expr_local(&self, node_id: EvalNodeId) -> Option<&RetainedExprTask> {
        self.delta
            .retained_exprs
            .get(&node_id)
            .or_else(|| self.base.base.retained_exprs.get(&node_id))
    }

    fn work_is_queued(&self, work_id: EvalWorkId) -> bool {
        self.queued.iter().any(|record| record.work.id == work_id)
            || self.base.baseline_ready_tasks.contains(&work_id)
    }

    fn find_keyed_node(
        &self,
        resolver: &FunctionRef,
        key: &Value,
        branch_context: &EvalBranchContext,
    ) -> Result<Option<EvalNodeId>, EvalError> {
        for memo in self.delta.keyed_nodes.values().chain(
            self.base
                .base
                .keyed_nodes
                .iter()
                .filter(|(node_id, _)| !self.delta.keyed_nodes.contains_key(node_id))
                .map(|(_, memo)| memo),
        ) {
            if memo.resolver == *resolver && memo.branch_context == *branch_context {
                if values_equal(&memo.key.rep, &key.rep)? {
                    return Ok(Some(memo.node_id));
                }
            }
        }
        Ok(None)
    }

    fn dependent_nodes(&self, dependency: EvalNodeId) -> Vec<EvalNodeId> {
        self.base
            .base
            .dependency_edges
            .iter()
            .chain(self.delta.dependency_edges.iter())
            .filter_map(|edge| (edge.child == dependency).then_some(edge.parent))
            .collect()
    }

    fn enqueue_expr_work(&mut self, node_id: EvalNodeId) -> Result<(), EvalError> {
        let retained = self.retained_expr_local(node_id).cloned().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("expression node {:?} has no retained replay task", node_id),
            )
        })?;
        let work = self.enqueue_ready_task(
            ReadyTask::EvaluateExpr(retained.scheduled),
            EvalWorkKind::EvaluateExpr,
            node_id,
            None,
            retained.branch_context,
        );
        self.record_node_state(
            node_id,
            EvalNodeState::Runnable {
                work: work.id,
                pending: None,
            },
        );
        if self
            .node_record_local(node_id)
            .and_then(|record| record.parent_id)
            == Some(ENTRY_WORK_NODE_ID)
        {
            self.record_node_state(
                ENTRY_WORK_NODE_ID,
                EvalNodeState::Runnable {
                    work: work.id,
                    pending: None,
                },
            );
        }
        Ok(())
    }

    fn enqueue_keyed_work(&mut self, node_id: EvalNodeId) -> Result<(), EvalError> {
        let branch_context = self
            .keyed_node_local(node_id)
            .map(|memo| memo.branch_context.clone())
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("keyed node {:?} is not registered", node_id),
                )
            })?;
        let work = self.enqueue_ready_task(
            ReadyTask::EvaluateKeyed(node_id),
            EvalWorkKind::EvaluateKeyed,
            node_id,
            None,
            branch_context,
        );
        self.record_node_state(
            node_id,
            EvalNodeState::Runnable {
                work: work.id,
                pending: None,
            },
        );
        let keyed = self
            .keyed_node_mut(node_id)
            .expect("keyed node must still exist");
        keyed.active_revision = Some(keyed.revision);
        keyed.evaluated_revision = None;
        Ok(())
    }

    fn invalidate_completed_dependents(
        &mut self,
        dependency: EvalNodeId,
    ) -> Result<bool, EvalError> {
        let mut invalidated = false;
        let mut stack = self.dependent_nodes(dependency);
        let mut seen = BTreeSet::new();
        while let Some(node_id) = stack.pop() {
            if !seen.insert(node_id) {
                continue;
            }
            let state = self.node_state_local(node_id).cloned();
            let reran = match state {
                Some(EvalNodeState::Ready(_))
                | Some(EvalNodeState::Poison(_))
                | Some(EvalNodeState::Runnable { .. })
                | Some(EvalNodeState::BlockedOnNode { .. })
                | Some(EvalNodeState::Pending(_))
                | None => {
                    if self.keyed_node_local(node_id).is_some() {
                        self.enqueue_keyed_work(node_id)?;
                        true
                    } else if self.retained_expr_local(node_id).is_some() {
                        self.enqueue_expr_work(node_id)?;
                        true
                    } else {
                        false
                    }
                }
            };
            if reran {
                invalidated = true;
                stack.extend(self.dependent_nodes(node_id));
            }
        }
        if invalidated {
            self.completed_invalidated = true;
        }
        Ok(invalidated)
    }

    fn learned_no_goods<'b>(&'b self) -> impl Iterator<Item = &'b LearnedNoGood> + 'b {
        self.delta
            .learned_no_goods
            .iter()
            .chain(self.base.base.learned_no_goods.iter())
    }

    fn learned_search_no_goods<'b>(&'b self) -> impl Iterator<Item = &'b LearnedSearchNoGood> + 'b {
        self.delta
            .learned_search_no_goods
            .iter()
            .chain(self.base.base.learned_search_no_goods.iter())
    }

    fn covers_learned_no_good(&self, learned: &LearnedNoGood) -> Result<bool, EvalError> {
        Ok(self.learned_no_goods().any(|existing| {
            existing.branch_context == learned.branch_context
                && existing.assumption == learned.assumption
                && existing.resolver == learned.resolver
                && existing.start_index <= learned.start_index
                && learned.end_index <= existing.end_index
                && candidate_lists_equal(existing.candidates.as_ref(), learned.candidates.as_ref())
                    .is_ok_and(|equal| equal)
                && errors_equal_for_learning(&existing.error, &learned.error)
                    .is_ok_and(|equal| equal)
        }))
    }
}

impl EvalWorkerScheduler for EvalPreparedScheduler<'_> {
    fn alloc_node_id(&mut self) -> EvalNodeId {
        self.allocator.alloc_node_id()
    }

    fn record_dependency(&mut self, parent: EvalNodeId, child: EvalNodeId) {
        self.delta
            .dependency_edges
            .push(EvalDependencyEdge { parent, child });
    }

    fn record_node(&mut self, record: EvalNodeRecord) {
        let node_id = record.node_id;
        if self.node_record_local(node_id).is_none() {
            self.delta.node_records.insert(node_id, record);
        }
    }

    fn record_node_state(&mut self, node_id: EvalNodeId, state: EvalNodeState) {
        self.delta.node_states.insert(node_id, state);
    }

    fn alloc_pending(&mut self, description: Arc<str>) -> PendingRef {
        let pending_id = self.alloc_node_id();
        let pending = PendingRef::new(pending_id.0, description.clone());
        self.delta
            .pending_kinds
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
        self.delta
            .pending_kinds
            .insert(pending.node_id, PendingKind::ChildDependency { dependency });
        pending
    }

    fn pending_kind(&self, pending: &PendingRef) -> Option<PendingKind> {
        self.pending_kind_local(pending.node_id)
    }

    fn register_suspension(&mut self, suspension: EvalSuspension) {
        self.delta
            .pending_resumes
            .entry(suspension.pending.node_id)
            .or_default()
            .push(suspension);
    }

    fn remember_expr_task(
        &mut self,
        node_id: EvalNodeId,
        scheduled: ScheduledExpr,
        branch_context: EvalBranchContext,
    ) {
        if self.retained_expr_local(node_id).is_none() {
            self.delta.retained_exprs.insert(
                node_id,
                RetainedExprTask {
                    scheduled,
                    branch_context,
                },
            );
        }
    }

    fn demand_keyed_node(
        &mut self,
        resolver: FunctionRef,
        key: Value,
        joined: Value,
        branch_context: EvalBranchContext,
        source: SourceSpan,
    ) -> Result<EvalNodeId, EvalError> {
        if let Some(node_id) = self.find_keyed_node(&resolver, &key, &branch_context)? {
            let mut widened = false;
            {
                let memo = self.keyed_node_mut(node_id).expect("keyed node must exist");
                let (next_joined, did_widen) = join_keyed_input(&memo.joined, &joined)?;
                if did_widen {
                    memo.joined = next_joined;
                    memo.revision += 1;
                    memo.last_update = EvalKeyedNodeUpdateReason::JoinedWidened;
                    widened = true;
                }
            }

            if widened {
                match self.node_state_local(node_id).cloned() {
                    Some(EvalNodeState::Runnable { work, .. }) if self.work_is_queued(work) => {
                        let memo = self.keyed_node_mut(node_id).expect("keyed node must exist");
                        memo.active_revision = Some(memo.revision);
                    }
                    Some(EvalNodeState::Ready(_)) | Some(EvalNodeState::Poison(_)) | None => {
                        self.enqueue_keyed_work(node_id)?;
                        let _ = self.invalidate_completed_dependents(node_id)?;
                    }
                    _ => {}
                }
            }
            return Ok(node_id);
        }

        let node_id = self.alloc_node_id();
        self.record_node(EvalNodeRecord {
            node_id,
            parent_id: None,
            kind: EvalNodeKind::Keyed {
                resolver: resolver.name.clone(),
                key: key.clone(),
            },
            source: Some(source),
        });
        self.delta.keyed_nodes.insert(
            node_id,
            KeyedNodeMemo {
                node_id,
                resolver,
                key,
                joined,
                branch_context,
                revision: 1,
                active_revision: None,
                evaluated_revision: None,
                last_update: EvalKeyedNodeUpdateReason::Created,
            },
        );
        self.enqueue_keyed_work(node_id)?;
        Ok(node_id)
    }

    fn finalize_keyed_completion(&mut self, node_id: EvalNodeId) -> Result<bool, EvalError> {
        let Some((active_revision, current_revision)) = self
            .keyed_node_local(node_id)
            .map(|memo| (memo.active_revision, memo.revision))
        else {
            return Ok(false);
        };
        let Some(active_revision) = active_revision else {
            return Ok(false);
        };
        if active_revision < current_revision {
            self.enqueue_keyed_work(node_id)?;
            return Ok(true);
        }
        let memo = self.keyed_node_mut(node_id).expect("keyed node must exist");
        memo.evaluated_revision = Some(active_revision);
        memo.active_revision = None;
        Ok(false)
    }

    fn keyed_resolution(&self, node_id: EvalNodeId) -> Option<Result<Value, EvalError>> {
        match self.node_state_local(node_id)? {
            EvalNodeState::Ready(value) => Some(Ok(value.clone())),
            EvalNodeState::Poison(error) => Some(Err(error.clone())),
            _ => None,
        }
    }

    fn keyed_node(&self, node_id: EvalNodeId) -> Option<&KeyedNodeMemo> {
        self.keyed_node_local(node_id)
    }

    fn node_state(&self, node_id: EvalNodeId) -> Option<&EvalNodeState> {
        self.node_state_local(node_id)
    }

    fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        self.node_record_local(node_id)
    }

    fn learned_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        index: usize,
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError> {
        self.learned_no_goods()
            .find_map(|learned| {
                if learned.branch_context == *branch_context
                    && learned.assumption == *assumption
                    && learned.resolver == *resolver
                {
                    Some(
                        candidate_lists_equal(learned.candidates.as_ref(), candidates).map(
                            |equal| {
                                (equal
                                    && learned.start_index <= index
                                    && index <= learned.end_index)
                                    .then(|| learned.error.clone())
                            },
                        ),
                    )
                } else {
                    None
                }
            })
            .transpose()
            .map(|found| found.flatten())
    }

    fn learned_search_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError> {
        self.learned_search_no_goods()
            .find_map(|learned| {
                if learned.branch_context == *branch_context
                    && learned.assumption == *assumption
                    && learned.resolver == *resolver
                {
                    Some(
                        candidate_lists_equal(learned.candidates.as_ref(), candidates)
                            .map(|equal| equal.then(|| learned.error.clone())),
                    )
                } else {
                    None
                }
            })
            .transpose()
            .map(|found| found.flatten())
    }

    fn record_learned_no_good(&mut self, learned: LearnedNoGood) -> Result<(), EvalError> {
        if self.covers_learned_no_good(&learned)? {
            return Ok(());
        }
        for existing in &mut self.delta.learned_no_goods {
            if existing.branch_context == learned.branch_context
                && existing.assumption == learned.assumption
                && existing.resolver == learned.resolver
                && candidate_lists_equal(existing.candidates.as_ref(), learned.candidates.as_ref())?
                && errors_equal_for_learning(&existing.error, &learned.error)?
            {
                let existing_end_plus_one = existing.end_index.saturating_add(1);
                let learned_end_plus_one = learned.end_index.saturating_add(1);
                let ranges_touch_or_overlap = existing.start_index <= learned_end_plus_one
                    && learned.start_index <= existing_end_plus_one;
                if ranges_touch_or_overlap {
                    existing.start_index = existing.start_index.min(learned.start_index);
                    existing.end_index = existing.end_index.max(learned.end_index);
                    return Ok(());
                }
            }
        }
        self.delta.learned_no_goods.push(learned);
        Ok(())
    }

    fn record_learned_search_no_good(
        &mut self,
        learned: LearnedSearchNoGood,
    ) -> Result<(), EvalError> {
        for existing in &mut self.delta.learned_search_no_goods {
            if existing.branch_context == learned.branch_context
                && existing.assumption == learned.assumption
                && existing.resolver == learned.resolver
                && candidate_lists_equal(existing.candidates.as_ref(), learned.candidates.as_ref())?
            {
                if errors_equal_for_learning(&existing.error, &learned.error)? {
                    return Ok(());
                }
                *existing = learned;
                return Ok(());
            }
        }
        if self
            .base
            .base
            .learned_search_no_goods
            .iter()
            .any(|existing| {
                existing.branch_context == learned.branch_context
                    && existing.assumption == learned.assumption
                    && existing.resolver == learned.resolver
                    && candidate_lists_equal(
                        existing.candidates.as_ref(),
                        learned.candidates.as_ref(),
                    )
                    .is_ok_and(|equal| equal)
                    && errors_equal_for_learning(&existing.error, &learned.error)
                        .is_ok_and(|equal| equal)
            })
        {
            return Ok(());
        }
        self.delta.learned_search_no_goods.push(learned);
        Ok(())
    }

    fn enqueue_ready_task(
        &mut self,
        task: ReadyTask,
        kind: EvalWorkKind,
        node_id: EvalNodeId,
        pending: Option<PendingRef>,
        branch_context: EvalBranchContext,
    ) -> EvalQueuedWork {
        let work = EvalQueuedWork {
            id: self.allocator.alloc_work_id(),
            kind,
            node_id,
            pending,
        };
        self.queued.push(ReadyTaskRecord {
            work: work.clone(),
            task,
            branch_context,
        });
        work
    }
}

pub(super) enum ParallelWorkerMessage {
    Completion(Result<EvalWorkCompletion, EvalError>),
    Panic(Box<dyn Any + Send + 'static>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScheduledResume {
    pub(super) suspension: EvalSuspension,
    pub(super) result: Result<Value, EvalError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScheduledExpr {
    pub(super) expr: ExprHandle,
    pub(super) env: EvalEnv,
    pub(super) call_stack: Vec<EvalAuthoredCallFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReadyTask {
    StartEntry,
    EvaluateExpr(ScheduledExpr),
    EvaluateKeyed(EvalNodeId),
    Resume(ScheduledResume),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingKind {
    External,
    ChildDependency { dependency: EvalNodeId },
}

impl EvalPreparedSnapshot {
    fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        self.base.node_records.get(&node_id)
    }

    fn node_state(&self, node_id: EvalNodeId) -> Option<&EvalNodeState> {
        self.base.node_states.get(&node_id)
    }
}

impl EvalScheduler {
    pub(super) fn prepared_snapshot(&self, namespace: Option<u64>) -> EvalPreparedSnapshot {
        EvalPreparedSnapshot {
            base: self.base.clone(),
            lease_namespace: namespace,
            baseline_ready_tasks: self.ready_tasks.keys().copied().collect(),
        }
    }

    pub(super) fn dependency_edges(&self) -> impl Iterator<Item = &EvalDependencyEdge> {
        self.base.dependency_edges.iter()
    }

    pub(super) fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        self.base.node_records.get(&node_id)
    }

    pub(super) fn node_records(&self) -> Vec<EvalNodeRecord> {
        self.node_record_ids()
            .into_iter()
            .filter_map(|node_id| self.node_record(node_id).cloned())
            .collect()
    }

    pub(super) fn node_record_ids(&self) -> BTreeSet<EvalNodeId> {
        self.base.node_records.keys().copied().collect()
    }

    pub(super) fn node_state_ids(&self) -> BTreeSet<EvalNodeId> {
        self.base.node_states.keys().copied().collect()
    }

    pub(super) fn node_record_count(&self) -> usize {
        self.node_record_ids().len()
    }

    pub(super) fn node_state_count(&self) -> usize {
        self.node_state_ids().len()
    }

    pub(super) fn record_node(&mut self, record: EvalNodeRecord) {
        let node_id = record.node_id;
        if let im::ordmap::Entry::Vacant(entry) = self.base.node_records.entry(node_id) {
            entry.insert(record);
        }
    }

    pub(super) fn record_dependency(&mut self, parent: EvalNodeId, child: EvalNodeId) {
        self.base
            .dependency_edges
            .push_back(EvalDependencyEdge { parent, child });
    }

    pub(super) fn queued_work(&self) -> Vec<EvalQueuedWork> {
        self.ready_queue
            .iter()
            .filter_map(|id| self.ready_tasks.get(id).map(|record| record.work.clone()))
            .collect()
    }

    pub(super) fn in_flight_work(&self) -> Vec<EvalQueuedWork> {
        self.in_flight_tasks
            .values()
            .map(|record| record.record.work.clone())
            .collect()
    }

    pub(super) fn active_wave(&self) -> Option<EvalActiveWave> {
        let wave_id = self.active_wave?;
        let mut work = self
            .in_flight_tasks
            .values()
            .filter(|record| record.wave_id == wave_id)
            .map(|record| EvalLeasedWork {
                lease_sequence: record.lease_sequence,
                work: record.record.work.clone(),
                state: EvalLeasedWorkState::InFlight,
            })
            .collect::<Vec<_>>();
        work.extend(
            self.buffered_completions
                .values()
                .filter(|completion| completion.wave_id == wave_id)
                .map(|completion| EvalLeasedWork {
                    lease_sequence: completion.lease_sequence,
                    work: completion.work.clone(),
                    state: EvalLeasedWorkState::Buffered(completion.outcome.clone()),
                }),
        );
        work.sort_by_key(|record| record.lease_sequence);
        Some(EvalActiveWave { wave_id, work })
    }

    pub(super) fn buffered_work(&self) -> Vec<EvalBufferedWork> {
        self.buffered_completions
            .iter()
            .map(|(lease_sequence, completion)| EvalBufferedWork {
                wave_id: completion.wave_id,
                lease_sequence: *lease_sequence,
                work: completion.work.clone(),
                outcome: completion.outcome.clone(),
            })
            .collect()
    }

    pub(super) fn runnable_nodes(&self) -> Vec<EvalRunnableNode> {
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

    pub(super) fn enqueue_start_entry(
        &mut self,
        branch_context: EvalBranchContext,
    ) -> EvalQueuedWork {
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
            branch_context,
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

    pub(super) fn alloc_node_id(&mut self) -> EvalNodeId {
        let node_id = EvalNodeId(self.base.next_id);
        self.base.next_id += 1;
        node_id
    }

    pub(super) fn alloc_pending(&mut self, description: impl Into<Arc<str>>) -> PendingRef {
        let pending_id = self.alloc_node_id();
        let description = description.into();
        let pending = PendingRef::new(pending_id.0, description.clone());
        self.base
            .pending_kinds
            .insert(pending.node_id, PendingKind::External);
        self.record_node(EvalNodeRecord {
            node_id: pending_id,
            parent_id: None,
            kind: EvalNodeKind::PendingToken { description },
            source: None,
        });
        pending
    }

    pub(super) fn alloc_child_dependency_pending(&mut self, dependency: EvalNodeId) -> PendingRef {
        let pending = PendingRef::new(dependency.0, "waiting on child expression");
        self.base
            .pending_kinds
            .insert(pending.node_id, PendingKind::ChildDependency { dependency });
        pending
    }

    pub(super) fn pending_nodes(&self) -> Vec<PendingRef> {
        self.base
            .pending_resumes
            .values()
            .filter_map(|suspensions| {
                let suspension = suspensions.first()?;
                matches!(
                    self.base.pending_kinds.get(&suspension.pending.node_id),
                    Some(PendingKind::External)
                )
                .then(|| suspension.pending.clone())
            })
            .collect()
    }

    pub(super) fn pending_kind(&self, pending: &PendingRef) -> Option<PendingKind> {
        self.base.pending_kinds.get(&pending.node_id).copied()
    }

    pub(super) fn has_waiting_work(&self) -> bool {
        !self.in_flight_tasks.is_empty() || !self.buffered_completions.is_empty()
    }

    fn next_wave(&mut self) -> EvalWaveId {
        let wave_id = EvalWaveId(self.next_wave_id);
        self.next_wave_id += 1;
        self.active_wave = Some(wave_id);
        wave_id
    }

    fn current_wave(&mut self) -> Result<EvalWaveId, EvalError> {
        if self.has_waiting_work() {
            return self.active_wave.ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "eval scheduler lost its active wave while work remained leased",
                )
            });
        }
        Ok(self.next_wave())
    }

    pub(super) fn clear_active_wave_if_idle(&mut self) {
        if !self.has_waiting_work() {
            self.active_wave = None;
        }
    }

    pub(super) fn replace_queued_branch_context(
        &mut self,
        previous: &EvalBranchContext,
        next: &EvalBranchContext,
    ) {
        for record in self.ready_tasks.values_mut() {
            if record.branch_context == *previous {
                record.branch_context = next.clone();
            }
        }
    }

    pub(super) fn node_state(&self, node_id: EvalNodeId) -> Option<&EvalNodeState> {
        self.base.node_states.get(&node_id)
    }

    pub(super) fn keyed_node(&self, node_id: EvalNodeId) -> Option<&KeyedNodeMemo> {
        self.base.keyed_nodes.get(&node_id)
    }

    fn keyed_node_mut(&mut self, node_id: EvalNodeId) -> Option<&mut KeyedNodeMemo> {
        self.base.keyed_nodes.get_mut(&node_id)
    }

    fn find_keyed_node(
        &self,
        resolver: &FunctionRef,
        key: &Value,
        branch_context: &EvalBranchContext,
    ) -> Result<Option<EvalNodeId>, EvalError> {
        self.base
            .keyed_nodes
            .iter()
            .find_map(|(node_id, memo)| {
                if memo.resolver == *resolver && memo.branch_context == *branch_context {
                    Some(
                        values_equal(&memo.key.rep, &key.rep)
                            .map(|equal| equal.then_some(*node_id)),
                    )
                } else {
                    None
                }
            })
            .transpose()
            .map(|found| found.flatten())
    }

    fn work_is_queued(&self, work_id: EvalWorkId) -> bool {
        self.ready_tasks.contains_key(&work_id)
    }

    pub(super) fn remember_expr_task(
        &mut self,
        node_id: EvalNodeId,
        scheduled: ScheduledExpr,
        branch_context: EvalBranchContext,
    ) {
        self.base
            .retained_exprs
            .entry(node_id)
            .or_insert(RetainedExprTask {
                scheduled,
                branch_context,
            });
    }

    pub(super) fn learned_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        index: usize,
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError> {
        self.base
            .learned_no_goods
            .iter()
            .find_map(|learned| {
                if learned.branch_context == *branch_context
                    && learned.assumption == *assumption
                    && learned.resolver == *resolver
                {
                    Some(
                        candidate_lists_equal(learned.candidates.as_ref(), candidates).map(
                            |equal| {
                                (equal
                                    && learned.start_index <= index
                                    && index <= learned.end_index)
                                    .then(|| learned.error.clone())
                            },
                        ),
                    )
                } else {
                    None
                }
            })
            .transpose()
            .map(|found| found.flatten())
    }

    pub(super) fn learned_search_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError> {
        self.base
            .learned_search_no_goods
            .iter()
            .find_map(|learned| {
                if learned.branch_context == *branch_context
                    && learned.assumption == *assumption
                    && learned.resolver == *resolver
                {
                    Some(
                        candidate_lists_equal(learned.candidates.as_ref(), candidates)
                            .map(|equal| equal.then(|| learned.error.clone())),
                    )
                } else {
                    None
                }
            })
            .transpose()
            .map(|found| found.flatten())
    }

    fn covers_learned_no_good(&self, learned: &LearnedNoGood) -> Result<bool, EvalError> {
        Ok(self.base.learned_no_goods.iter().any(|existing| {
            existing.branch_context == learned.branch_context
                && existing.assumption == learned.assumption
                && existing.resolver == learned.resolver
                && existing.start_index <= learned.start_index
                && learned.end_index <= existing.end_index
                && candidate_lists_equal(existing.candidates.as_ref(), learned.candidates.as_ref())
                    .is_ok_and(|equal| equal)
                && errors_equal_for_learning(&existing.error, &learned.error)
                    .is_ok_and(|equal| equal)
        }))
    }

    pub(super) fn record_learned_search_no_good(
        &mut self,
        learned: LearnedSearchNoGood,
    ) -> Result<(), EvalError> {
        let learned_search_no_goods = &mut self.base.learned_search_no_goods;
        for existing in learned_search_no_goods.iter_mut() {
            if existing.branch_context == learned.branch_context
                && existing.assumption == learned.assumption
                && existing.resolver == learned.resolver
                && candidate_lists_equal(existing.candidates.as_ref(), learned.candidates.as_ref())?
            {
                if errors_equal_for_learning(&existing.error, &learned.error)? {
                    return Ok(());
                }
                *existing = learned;
                return Ok(());
            }
        }
        learned_search_no_goods.push_back(learned);
        Ok(())
    }

    pub(super) fn record_learned_no_good(
        &mut self,
        learned: LearnedNoGood,
    ) -> Result<(), EvalError> {
        if self.covers_learned_no_good(&learned)? {
            return Ok(());
        }
        let learned_no_goods = &mut self.base.learned_no_goods;
        for existing in learned_no_goods.iter_mut() {
            if existing.branch_context == learned.branch_context
                && existing.assumption == learned.assumption
                && existing.resolver == learned.resolver
                && candidate_lists_equal(existing.candidates.as_ref(), learned.candidates.as_ref())?
                && errors_equal_for_learning(&existing.error, &learned.error)?
            {
                let existing_end_plus_one = existing.end_index.saturating_add(1);
                let learned_end_plus_one = learned.end_index.saturating_add(1);
                let ranges_touch_or_overlap = existing.start_index <= learned_end_plus_one
                    && learned.start_index <= existing_end_plus_one;
                if ranges_touch_or_overlap {
                    existing.start_index = existing.start_index.min(learned.start_index);
                    existing.end_index = existing.end_index.max(learned.end_index);
                    return Ok(());
                }
            }
        }
        learned_no_goods.push_back(learned);
        Ok(())
    }

    fn enqueue_expr_work(&mut self, node_id: EvalNodeId) -> Result<(), EvalError> {
        let retained = self
            .base
            .retained_exprs
            .get(&node_id)
            .cloned()
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("expression node {:?} has no retained replay task", node_id),
                )
            })?;
        let work = self.enqueue_ready_task(
            ReadyTask::EvaluateExpr(retained.scheduled),
            EvalWorkKind::EvaluateExpr,
            node_id,
            None,
            retained.branch_context,
        );
        self.record_node_state(
            node_id,
            EvalNodeState::Runnable {
                work: work.id,
                pending: None,
            },
        );
        if self
            .base
            .node_records
            .get(&node_id)
            .and_then(|record| record.parent_id)
            == Some(ENTRY_WORK_NODE_ID)
        {
            self.record_node_state(
                ENTRY_WORK_NODE_ID,
                EvalNodeState::Runnable {
                    work: work.id,
                    pending: None,
                },
            );
        }
        Ok(())
    }

    fn dependent_nodes(&self, dependency: EvalNodeId) -> Vec<EvalNodeId> {
        self.base
            .dependency_edges
            .iter()
            .filter_map(|edge| (edge.child == dependency).then_some(edge.parent))
            .collect()
    }

    fn invalidate_completed_dependents(
        &mut self,
        dependency: EvalNodeId,
    ) -> Result<bool, EvalError> {
        let mut invalidated = false;
        let mut stack = self.dependent_nodes(dependency);
        let mut seen = BTreeSet::new();
        while let Some(node_id) = stack.pop() {
            if !seen.insert(node_id) {
                continue;
            }
            let state = self.node_state(node_id).cloned();
            let reran = match state {
                Some(EvalNodeState::Ready(_))
                | Some(EvalNodeState::Poison(_))
                | Some(EvalNodeState::Runnable { .. })
                | Some(EvalNodeState::BlockedOnNode { .. })
                | Some(EvalNodeState::Pending(_))
                | None => {
                    if self.base.keyed_nodes.contains_key(&node_id) {
                        self.enqueue_keyed_work(node_id)?;
                        true
                    } else if self.base.retained_exprs.contains_key(&node_id) {
                        self.enqueue_expr_work(node_id)?;
                        true
                    } else {
                        false
                    }
                }
            };
            if reran {
                invalidated = true;
                stack.extend(self.dependent_nodes(node_id));
            }
        }
        Ok(invalidated)
    }

    fn enqueue_keyed_work(&mut self, node_id: EvalNodeId) -> Result<(), EvalError> {
        let branch_context = if let Some(memo) = self.base.keyed_nodes.get(&node_id) {
            memo.branch_context.clone()
        } else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("keyed node {:?} is not registered", node_id),
            ));
        };
        let work = self.enqueue_ready_task(
            ReadyTask::EvaluateKeyed(node_id),
            EvalWorkKind::EvaluateKeyed,
            node_id,
            None,
            branch_context,
        );
        self.record_node_state(
            node_id,
            EvalNodeState::Runnable {
                work: work.id,
                pending: None,
            },
        );
        let keyed = self
            .base
            .keyed_nodes
            .get_mut(&node_id)
            .expect("keyed node must still exist");
        keyed.active_revision = Some(keyed.revision);
        keyed.evaluated_revision = None;
        Ok(())
    }

    pub(super) fn demand_keyed_node(
        &mut self,
        resolver: FunctionRef,
        key: Value,
        joined: Value,
        branch_context: EvalBranchContext,
        source: SourceSpan,
    ) -> Result<EvalNodeId, EvalError> {
        if let Some(node_id) = self.find_keyed_node(&resolver, &key, &branch_context)? {
            let mut widened = false;
            {
                let memo = self.keyed_node_mut(node_id).expect("keyed node must exist");
                let (next_joined, did_widen) = join_keyed_input(&memo.joined, &joined)?;
                if did_widen {
                    memo.joined = next_joined;
                    memo.revision += 1;
                    memo.last_update = EvalKeyedNodeUpdateReason::JoinedWidened;
                    widened = true;
                }
            }

            if widened {
                match self.node_state(node_id).cloned() {
                    Some(EvalNodeState::Runnable { work, .. }) if self.work_is_queued(work) => {
                        let memo = self.keyed_node_mut(node_id).expect("keyed node must exist");
                        memo.active_revision = Some(memo.revision);
                    }
                    Some(EvalNodeState::Ready(_)) | Some(EvalNodeState::Poison(_)) | None => {
                        self.enqueue_keyed_work(node_id)?;
                        let _ = self.invalidate_completed_dependents(node_id)?;
                    }
                    _ => {}
                }
            }
            return Ok(node_id);
        }

        let node_id = self.alloc_node_id();
        self.record_node(EvalNodeRecord {
            node_id,
            parent_id: None,
            kind: EvalNodeKind::Keyed {
                resolver: resolver.name.clone(),
                key: key.clone(),
            },
            source: Some(source),
        });
        self.base.keyed_nodes.insert(
            node_id,
            KeyedNodeMemo {
                node_id,
                resolver,
                key,
                joined,
                branch_context,
                revision: 1,
                active_revision: None,
                evaluated_revision: None,
                last_update: EvalKeyedNodeUpdateReason::Created,
            },
        );
        self.enqueue_keyed_work(node_id)?;
        Ok(node_id)
    }

    pub(super) fn finalize_keyed_completion(
        &mut self,
        node_id: EvalNodeId,
    ) -> Result<bool, EvalError> {
        let Some((active_revision, current_revision)) = self
            .base
            .keyed_nodes
            .get(&node_id)
            .map(|memo| (memo.active_revision, memo.revision))
        else {
            return Ok(false);
        };
        let Some(active_revision) = active_revision else {
            return Ok(false);
        };
        if active_revision < current_revision {
            self.enqueue_keyed_work(node_id)?;
            return Ok(true);
        }
        let memo = self
            .base
            .keyed_nodes
            .get_mut(&node_id)
            .expect("keyed node must exist");
        memo.evaluated_revision = Some(active_revision);
        memo.active_revision = None;
        Ok(false)
    }

    pub(super) fn keyed_resolution(&self, node_id: EvalNodeId) -> Option<Result<Value, EvalError>> {
        match self.node_state(node_id)? {
            EvalNodeState::Ready(value) => Some(Ok(value.clone())),
            EvalNodeState::Poison(error) => Some(Err(error.clone())),
            _ => None,
        }
    }

    pub(super) fn explain_node(&self, node_id: EvalNodeId) -> EvalNodeExplanation {
        let mut chain = vec![node_id];
        let mut current = node_id;
        loop {
            let terminal = match self.node_state(current) {
                Some(EvalNodeState::Runnable { work, pending }) => {
                    if let Some(buffered) = self.buffered_completion_for_work(*work) {
                        EvalExplainTerminal::Buffered {
                            node_id: current,
                            work: *work,
                            pending: pending.clone(),
                            lease_sequence: buffered.lease_sequence,
                            outcome: buffered.outcome.clone(),
                        }
                    } else {
                        EvalExplainTerminal::Runnable {
                            node_id: current,
                            work: *work,
                            pending: pending.clone(),
                        }
                    }
                }
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

    pub(super) fn record_node_state(&mut self, node_id: EvalNodeId, state: EvalNodeState) {
        self.base.node_states.insert(node_id, state);
    }

    pub(super) fn resolve_pending(
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

    pub(super) fn resolve_child_dependency(
        &mut self,
        dependency: EvalNodeId,
        result: Result<Value, EvalError>,
    ) -> Result<bool, EvalError> {
        match self.base.pending_kinds.get(&dependency.0).copied() {
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
        let Some(suspensions) = self.base.pending_resumes.remove(&pending_id) else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("pending node {pending_id} is not registered"),
            ));
        };
        let Some(pending) = suspensions
            .first()
            .map(|suspension| suspension.pending.clone())
        else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("pending node {pending_id} had no registered suspensions"),
            ));
        };
        self.base.pending_kinds.remove(&pending_id);
        let node_id = EvalNodeId(pending.node_id);
        match &result {
            Ok(value) => self.record_node_state(node_id, EvalNodeState::Ready(value.clone())),
            Err(error) => self.record_node_state(node_id, EvalNodeState::Poison(error.clone())),
        }
        for suspension in suspensions {
            let resume_node = suspension.resume_node;
            let branch_context = suspension.branch_context.clone();
            let work = self.enqueue_ready_task(
                ReadyTask::Resume(ScheduledResume {
                    suspension,
                    result: result.clone(),
                }),
                EvalWorkKind::ResumePending,
                resume_node,
                Some(pending.clone()),
                branch_context,
            );
            self.record_node_state(
                resume_node,
                EvalNodeState::Runnable {
                    work: work.id,
                    pending: Some(pending.clone()),
                },
            );
        }
        Ok(())
    }

    pub(super) fn register_suspension(&mut self, suspension: EvalSuspension) {
        self.base
            .pending_resumes
            .entry(suspension.pending.node_id)
            .or_default()
            .push(suspension);
    }

    pub(super) fn lease_ready_task(&mut self) -> Result<Option<ReadyTaskLease>, EvalError> {
        let Some(work_id) = self.ready_queue.pop_front() else {
            return Ok(None);
        };
        let Some(record) = self.ready_tasks.remove(&work_id) else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("ready task {work_id:?} was missing from scheduler state"),
            ));
        };
        let wave_id = self.current_wave()?;
        let lease_sequence = self.next_lease_sequence;
        self.next_lease_sequence += 1;
        let namespace = if self.in_flight_tasks.is_empty() {
            None
        } else {
            let namespace = self.next_lease_namespace;
            self.next_lease_namespace += 1;
            Some(namespace)
        };
        self.in_flight_tasks.insert(
            work_id,
            InFlightTaskRecord {
                wave_id,
                lease_sequence,
                record: record.clone(),
            },
        );
        Ok(Some(ReadyTaskLease {
            wave_id,
            lease_sequence,
            namespace,
            record,
        }))
    }

    pub(super) fn can_commit_lease_sequence(&self, lease_sequence: u64) -> bool {
        self.in_flight_tasks
            .values()
            .map(|record| record.lease_sequence)
            .min()
            == Some(lease_sequence)
    }

    pub(super) fn take_in_flight_task(
        &mut self,
        work_id: EvalWorkId,
        lease_sequence: u64,
        expected_work: &EvalQueuedWork,
    ) -> Result<ReadyTaskRecord, EvalError> {
        self.validate_in_flight_task(work_id, lease_sequence, expected_work)?;
        let record = self
            .in_flight_tasks
            .remove(&work_id)
            .expect("validated in-flight task should exist");
        Ok(record.record)
    }

    pub(super) fn buffer_completion(
        &mut self,
        completion: EvalWorkCompletion,
    ) -> Result<(), EvalError> {
        let lease_sequence = completion.lease_sequence;
        let work_id = completion.work.id;
        if self.buffered_completions.contains_key(&lease_sequence) {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "buffered eval work completion already exists for lease sequence {}",
                    lease_sequence
                ),
            ));
        }
        let _leased = self.take_in_flight_task(work_id, lease_sequence, &completion.work)?;
        self.buffered_completions.insert(lease_sequence, completion);
        Ok(())
    }

    fn buffered_completion_for_work(&self, work_id: EvalWorkId) -> Option<&EvalWorkCompletion> {
        self.buffered_completions
            .values()
            .find(|completion| completion.work.id == work_id)
    }

    pub(super) fn take_next_committable_buffered_completion(
        &mut self,
    ) -> Option<EvalWorkCompletion> {
        let (&lease_sequence, _) = self.buffered_completions.first_key_value()?;
        let next_in_flight = self
            .in_flight_tasks
            .values()
            .map(|record| record.lease_sequence)
            .min();
        match next_in_flight {
            Some(in_flight) if in_flight < lease_sequence => None,
            _ => {
                let completion = self.buffered_completions.remove(&lease_sequence);
                self.clear_active_wave_if_idle();
                completion
            }
        }
    }

    pub(super) fn validate_in_flight_task(
        &self,
        work_id: EvalWorkId,
        lease_sequence: u64,
        expected_work: &EvalQueuedWork,
    ) -> Result<(), EvalError> {
        let Some(record) = self.in_flight_tasks.get(&work_id) else {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "stale eval work completion for non-in-flight work {:?}",
                    work_id
                ),
            ));
        };
        if record.lease_sequence != lease_sequence || record.record.work != *expected_work {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("stale eval work completion for {:?}", work_id),
            ));
        }
        Ok(())
    }

    pub(super) fn apply_delta(&mut self, delta: EvalSchedulerDelta) -> Result<(), EvalError> {
        self.base.next_id = self.base.next_id.max(delta.next_id);
        self.base.next_work_id = self.base.next_work_id.max(delta.next_work_id);
        for record in delta.node_records {
            match self.base.node_records.get(&record.node_id) {
                Some(existing) if existing != &record => {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        format!(
                            "node record {:?} diverged during completion merge",
                            record.node_id
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    self.base.node_records.insert(record.node_id, record);
                }
            }
        }
        for keyed in delta.keyed_nodes {
            match self.base.keyed_nodes.get_mut(&keyed.node_id) {
                Some(existing) => merge_keyed_node(existing, keyed)?,
                None => {
                    self.base.keyed_nodes.insert(keyed.node_id, keyed);
                }
            }
        }
        self.base.dependency_edges.extend(delta.dependency_edges);
        for (pending_id, kind) in delta.pending_kinds {
            match self.base.pending_kinds.get(&pending_id) {
                Some(existing) if existing != &kind => {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        format!(
                            "pending kind {} diverged during completion merge",
                            pending_id
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    self.base.pending_kinds.insert(pending_id, kind);
                }
            }
        }
        for (pending_id, suspension) in delta.pending_resumes {
            let suspensions = self.base.pending_resumes.entry(pending_id).or_default();
            if !suspensions.contains(&suspension) {
                suspensions.push(suspension);
            }
        }
        for (node_id, state) in delta.node_states {
            self.base.node_states.insert(node_id, state);
        }
        for (node_id, retained) in delta.retained_exprs {
            match self.base.retained_exprs.get(&node_id) {
                Some(existing) if existing != &retained => {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        format!(
                            "retained expression task {:?} diverged during completion merge",
                            node_id
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    self.base.retained_exprs.insert(node_id, retained);
                }
            }
        }
        for learned in delta.learned_no_goods {
            self.record_learned_no_good(learned)?;
        }
        for learned in delta.learned_search_no_goods {
            self.record_learned_search_no_good(learned)?;
        }
        for record in delta.queued {
            if self.ready_tasks.contains_key(&record.work.id)
                || self.in_flight_tasks.contains_key(&record.work.id)
            {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!(
                        "queued work {:?} already exists during completion merge",
                        record.work.id
                    ),
                ));
            }
            self.ready_queue.push_back(record.work.id);
            self.ready_tasks.insert(record.work.id, record);
        }
        Ok(())
    }

    pub(super) fn enqueue_ready_task(
        &mut self,
        task: ReadyTask,
        kind: EvalWorkKind,
        node_id: EvalNodeId,
        pending: Option<PendingRef>,
        branch_context: EvalBranchContext,
    ) -> EvalQueuedWork {
        let work = EvalQueuedWork {
            id: EvalWorkId(self.base.next_work_id),
            kind,
            node_id,
            pending,
        };
        self.base.next_work_id += 1;
        self.ready_queue.push_back(work.id);
        self.ready_tasks.insert(
            work.id,
            ReadyTaskRecord {
                work: work.clone(),
                task,
                branch_context,
            },
        );
        work
    }
}

impl EvalWorkerScheduler for EvalScheduler {
    fn alloc_node_id(&mut self) -> EvalNodeId {
        Self::alloc_node_id(self)
    }

    fn record_dependency(&mut self, parent: EvalNodeId, child: EvalNodeId) {
        Self::record_dependency(self, parent, child);
    }

    fn record_node(&mut self, record: EvalNodeRecord) {
        Self::record_node(self, record);
    }

    fn record_node_state(&mut self, node_id: EvalNodeId, state: EvalNodeState) {
        Self::record_node_state(self, node_id, state);
    }

    fn alloc_pending(&mut self, description: Arc<str>) -> PendingRef {
        Self::alloc_pending(self, description)
    }

    fn alloc_child_dependency_pending(&mut self, dependency: EvalNodeId) -> PendingRef {
        Self::alloc_child_dependency_pending(self, dependency)
    }

    fn pending_kind(&self, pending: &PendingRef) -> Option<PendingKind> {
        Self::pending_kind(self, pending)
    }

    fn register_suspension(&mut self, suspension: EvalSuspension) {
        Self::register_suspension(self, suspension);
    }

    fn remember_expr_task(
        &mut self,
        node_id: EvalNodeId,
        scheduled: ScheduledExpr,
        branch_context: EvalBranchContext,
    ) {
        Self::remember_expr_task(self, node_id, scheduled, branch_context);
    }

    fn demand_keyed_node(
        &mut self,
        resolver: FunctionRef,
        key: Value,
        joined: Value,
        branch_context: EvalBranchContext,
        source: SourceSpan,
    ) -> Result<EvalNodeId, EvalError> {
        Self::demand_keyed_node(self, resolver, key, joined, branch_context, source)
    }

    fn finalize_keyed_completion(&mut self, node_id: EvalNodeId) -> Result<bool, EvalError> {
        Self::finalize_keyed_completion(self, node_id)
    }

    fn keyed_resolution(&self, node_id: EvalNodeId) -> Option<Result<Value, EvalError>> {
        Self::keyed_resolution(self, node_id)
    }

    fn keyed_node(&self, node_id: EvalNodeId) -> Option<&KeyedNodeMemo> {
        Self::keyed_node(self, node_id)
    }

    fn node_state(&self, node_id: EvalNodeId) -> Option<&EvalNodeState> {
        Self::node_state(self, node_id)
    }

    fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        Self::node_record(self, node_id)
    }

    fn learned_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        index: usize,
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError> {
        Self::learned_no_good(
            self,
            branch_context,
            assumption,
            candidates,
            index,
            resolver,
        )
    }

    fn learned_search_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError> {
        Self::learned_search_no_good(self, branch_context, assumption, candidates, resolver)
    }

    fn record_learned_no_good(&mut self, learned: LearnedNoGood) -> Result<(), EvalError> {
        Self::record_learned_no_good(self, learned)
    }

    fn record_learned_search_no_good(
        &mut self,
        learned: LearnedSearchNoGood,
    ) -> Result<(), EvalError> {
        Self::record_learned_search_no_good(self, learned)
    }

    fn enqueue_ready_task(
        &mut self,
        task: ReadyTask,
        kind: EvalWorkKind,
        node_id: EvalNodeId,
        pending: Option<PendingRef>,
        branch_context: EvalBranchContext,
    ) -> EvalQueuedWork {
        Self::enqueue_ready_task(self, task, kind, node_id, pending, branch_context)
    }
}

fn lease_scoped_id(namespace: u64, local: u64) -> u64 {
    const LEASE_FLAG: u64 = 1 << 63;
    LEASE_FLAG | (namespace << 32) | local
}

pub(super) fn execute_ready_task(
    program: Arc<CompiledProgram>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
    snapshot: EvalPreparedSnapshot,
    record: ReadyTaskRecord,
    call_profiling: bool,
) -> Result<
    (
        EvalSessionDelta,
        EvalCompletedWorkOutcome,
        Option<EvalAuthoredCall>,
    ),
    EvalError,
> {
    let mut prepared_scheduler = EvalPreparedScheduler::new(&snapshot);
    let mut trace = EvalTrace::default();
    let mut state = EvalState::new(
        program.as_ref(),
        program.entry_source(),
        record.branch_context.clone(),
        runtime,
        place,
        &mut trace,
        &mut prepared_scheduler,
        Some(record.work.id),
        true,
        call_profiling,
    );
    let mut invocation = None;

    let outcome = match record.task {
        ReadyTask::StartEntry => {
            let prepared = prepare_entry(&mut state, entry, args)?;
            let node_id = state.alloc_node_id();
            let body_kind = expr_kind(&prepared.body);
            let body_source = state
                .program
                .span_for_handle(ProgramNodeHandle::for_node(prepared.source, &prepared.body));
            state
                .scheduler
                .record_dependency(ENTRY_WORK_NODE_ID, node_id);
            state.scheduler.record_node(EvalNodeRecord {
                node_id,
                parent_id: Some(ENTRY_WORK_NODE_ID),
                kind: EvalNodeKind::Expr { kind: body_kind },
                source: Some(body_source.clone()),
            });
            let call_stack = if call_profiling {
                let frame = authored_call_frame(
                    &state,
                    node_id,
                    Arc::from(entry),
                    args,
                    body_source.clone(),
                );
                emit_authored_call_started(
                    &mut state,
                    node_id,
                    Some(ENTRY_WORK_NODE_ID),
                    body_source,
                    &frame,
                    args.len(),
                );
                vec![frame]
            } else {
                Vec::new()
            };
            let scheduled = ScheduledExpr {
                expr: ExprHandle {
                    key: ProgramNodeHandle::for_node(prepared.source, &prepared.body),
                },
                env: prepared.env,
                call_stack,
            };
            state.scheduler.remember_expr_task(
                node_id,
                scheduled.clone(),
                state.branch_context.clone(),
            );
            let work = state.scheduler.enqueue_ready_task(
                ReadyTask::EvaluateExpr(scheduled),
                EvalWorkKind::EvaluateExpr,
                node_id,
                None,
                state.branch_context.clone(),
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
            EvalCompletedWorkOutcome::Progressed
        }
        ReadyTask::EvaluateExpr(scheduled) => {
            let mut env = scheduled.env;
            let expr = state.program.resolve_expr(scheduled.expr.key)?;
            let step = if scheduled.call_stack.is_empty() {
                state.with_source(scheduled.expr.key.source(), |state| {
                    eval_expr_with_node_id(state, record.work.node_id, &expr, &mut env)
                })
            } else {
                state.with_call_stack(scheduled.call_stack, |state| {
                    state.with_source(scheduled.expr.key.source(), |state| {
                        eval_expr_with_node_id(state, record.work.node_id, &expr, &mut env)
                    })
                })
            };
            invocation = state.work_invocation.clone();
            match step {
                Ok(EvalStep::Ready(value)) => EvalCompletedWorkOutcome::Ready(value),
                Ok(EvalStep::Suspended(suspension)) => {
                    let pending = suspension.pending.clone();
                    state.scheduler.register_suspension(suspension);
                    match state.scheduler.pending_kind(&pending) {
                        Some(PendingKind::External) => {
                            state.scheduler.record_node_state(
                                ENTRY_WORK_NODE_ID,
                                EvalNodeState::Pending(pending.clone()),
                            );
                            EvalCompletedWorkOutcome::Pending(pending)
                        }
                        Some(PendingKind::ChildDependency { dependency }) => {
                            state.scheduler.record_node_state(
                                record.work.node_id,
                                EvalNodeState::BlockedOnNode { dependency },
                            );
                            EvalCompletedWorkOutcome::BlockedOnNode(dependency)
                        }
                        None => {
                            return Err(EvalError::new(
                                EvalErrorKind::Internal,
                                format!("pending node {} has no registered kind", pending.node_id),
                            ));
                        }
                    }
                }
                Err(error) => EvalCompletedWorkOutcome::Poison(error),
            }
        }
        ReadyTask::EvaluateKeyed(node_id) => {
            if !call_profiling {
                let prepared = prepare_keyed_call(&mut state, node_id)?;
                let mut env = prepared.env;
                match state.with_source(prepared.source, |state| {
                    eval_dependent_expr(state, node_id, &prepared.body, &mut env)
                })? {
                    EvalStep::Ready(value) => EvalCompletedWorkOutcome::Ready(value),
                    EvalStep::Suspended(suspension) => {
                        let pending = suspension.pending.clone();
                        state.scheduler.register_suspension(suspension);
                        match state.scheduler.pending_kind(&pending) {
                            Some(PendingKind::External) => {
                                state.scheduler.record_node_state(
                                    node_id,
                                    EvalNodeState::Pending(pending.clone()),
                                );
                                EvalCompletedWorkOutcome::Pending(pending)
                            }
                            Some(PendingKind::ChildDependency { dependency }) => {
                                state.scheduler.record_node_state(
                                    node_id,
                                    EvalNodeState::BlockedOnNode { dependency },
                                );
                                EvalCompletedWorkOutcome::BlockedOnNode(dependency)
                            }
                            None => {
                                return Err(EvalError::new(
                                    EvalErrorKind::Internal,
                                    format!(
                                        "pending node {} has no registered kind",
                                        pending.node_id
                                    ),
                                ));
                            }
                        }
                    }
                }
            } else {
                let (resolver_name, joined, source) = {
                    let memo = state.scheduler.keyed_node(node_id).ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            format!("keyed node {:?} is not registered", node_id),
                        )
                    })?;
                    let source = state
                        .scheduler
                        .node_record(node_id)
                        .and_then(|record| record.source.clone())
                        .unwrap_or_else(|| SourceSpan::synthetic("<keyed>"));
                    (
                        memo.resolver.name.clone(),
                        keyed_context_value(memo.key.clone(), memo.joined.clone()),
                        source,
                    )
                };
                let frame =
                    authored_call_frame(&state, node_id, resolver_name, &[joined], source.clone());
                emit_authored_call_started(&mut state, node_id, None, source, &frame, 1);
                let prepared = prepare_keyed_call(&mut state, node_id)?;
                let mut env = prepared.env;
                let step = state.with_authored_call(frame, |state| {
                    state.with_source(prepared.source, |state| {
                        eval_dependent_expr(state, node_id, &prepared.body, &mut env)
                    })
                });
                invocation = state.work_invocation.clone();
                match step? {
                    EvalStep::Ready(value) => EvalCompletedWorkOutcome::Ready(value),
                    EvalStep::Suspended(suspension) => {
                        let pending = suspension.pending.clone();
                        state.scheduler.register_suspension(suspension);
                        match state.scheduler.pending_kind(&pending) {
                            Some(PendingKind::External) => {
                                state.scheduler.record_node_state(
                                    node_id,
                                    EvalNodeState::Pending(pending.clone()),
                                );
                                EvalCompletedWorkOutcome::Pending(pending)
                            }
                            Some(PendingKind::ChildDependency { dependency }) => {
                                state.scheduler.record_node_state(
                                    node_id,
                                    EvalNodeState::BlockedOnNode { dependency },
                                );
                                EvalCompletedWorkOutcome::BlockedOnNode(dependency)
                            }
                            None => {
                                return Err(EvalError::new(
                                    EvalErrorKind::Internal,
                                    format!(
                                        "pending node {} has no registered kind",
                                        pending.node_id
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
        ReadyTask::Resume(ready) => {
            let step = resume_suspension(&mut state, ready.suspension, ready.result);
            invocation = state.work_invocation.clone();
            match step {
                Ok(EvalStep::Ready(value)) => EvalCompletedWorkOutcome::Ready(value),
                Ok(EvalStep::Suspended(suspension)) => {
                    let pending = suspension.pending.clone();
                    state.scheduler.register_suspension(suspension);
                    match state.scheduler.pending_kind(&pending) {
                        Some(PendingKind::External) => {
                            state.scheduler.record_node_state(
                                ENTRY_WORK_NODE_ID,
                                EvalNodeState::Pending(pending.clone()),
                            );
                            EvalCompletedWorkOutcome::Pending(pending)
                        }
                        Some(PendingKind::ChildDependency { dependency }) => {
                            state.scheduler.record_node_state(
                                record.work.node_id,
                                EvalNodeState::BlockedOnNode { dependency },
                            );
                            EvalCompletedWorkOutcome::BlockedOnNode(dependency)
                        }
                        None => {
                            return Err(EvalError::new(
                                EvalErrorKind::Internal,
                                format!("pending node {} has no registered kind", pending.node_id),
                            ));
                        }
                    }
                }
                Err(error) => EvalCompletedWorkOutcome::Poison(error),
            }
        }
    };

    let (recorded_delta, completed_invalidated) = prepared_scheduler.finish();

    Ok((
        EvalSessionDelta {
            scheduler: recorded_delta,
            trace_events: trace.events,
            completed_invalidated,
        },
        outcome,
        invocation,
    ))
}
