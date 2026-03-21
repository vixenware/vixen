use super::*;

#[derive(Debug, Clone)]
pub(super) struct EvalScheduler {
    pub(super) next_id: u64,
    pub(super) next_work_id: u64,
    pub(super) next_wave_id: u64,
    pub(super) next_lease_namespace: u64,
    pub(super) next_lease_sequence: u64,
    pub(super) active_wave: Option<EvalWaveId>,
    pub(super) lease_namespace: Option<LeaseNamespace>,
    pub(super) node_states: Arc<BTreeMap<EvalNodeId, EvalNodeState>>,
    pub(super) node_records: Arc<BTreeMap<EvalNodeId, EvalNodeRecord>>,
    pub(super) node_state_overrides: BTreeMap<EvalNodeId, EvalNodeState>,
    pub(super) node_record_overrides: BTreeMap<EvalNodeId, EvalNodeRecord>,
    pub(super) keyed_nodes: Arc<BTreeMap<EvalNodeId, KeyedNodeMemo>>,
    pub(super) dependency_edges: Arc<Vec<EvalDependencyEdge>>,
    pub(super) ready_queue: VecDeque<EvalWorkId>,
    pub(super) ready_tasks: BTreeMap<EvalWorkId, ReadyTaskRecord>,
    pub(super) baseline_ready_tasks: BTreeSet<EvalWorkId>,
    pub(super) in_flight_tasks: BTreeMap<EvalWorkId, InFlightTaskRecord>,
    pub(super) buffered_completions: BTreeMap<u64, EvalWorkCompletion>,
    pub(super) pending_resumes: Arc<BTreeMap<u64, Vec<EvalSuspension>>>,
    pub(super) pending_kinds: Arc<BTreeMap<u64, PendingKind>>,
    pub(super) retained_exprs: Arc<BTreeMap<EvalNodeId, RetainedExprTask>>,
    pub(super) learned_no_goods: Arc<Vec<LearnedNoGood>>,
    pub(super) learned_search_no_goods: Arc<Vec<LearnedSearchNoGood>>,
    pub(super) completed_invalidated: bool,
    pub(super) recorded_delta: Option<Box<RecordedSchedulerDelta>>,
}

pub(super) const ENTRY_WORK_NODE_ID: EvalNodeId = EvalNodeId(0);

impl Default for EvalScheduler {
    fn default() -> Self {
        Self {
            next_id: 1,
            next_work_id: 1,
            next_wave_id: 1,
            next_lease_namespace: 1,
            next_lease_sequence: 1,
            active_wave: None,
            lease_namespace: None,
            node_states: Arc::new(BTreeMap::new()),
            node_records: Arc::new(BTreeMap::new()),
            node_state_overrides: BTreeMap::new(),
            node_record_overrides: BTreeMap::new(),
            keyed_nodes: Arc::new(BTreeMap::new()),
            dependency_edges: Arc::new(Vec::new()),
            ready_queue: VecDeque::new(),
            ready_tasks: BTreeMap::new(),
            baseline_ready_tasks: BTreeSet::new(),
            in_flight_tasks: BTreeMap::new(),
            buffered_completions: BTreeMap::new(),
            pending_resumes: Arc::new(BTreeMap::new()),
            pending_kinds: Arc::new(BTreeMap::new()),
            retained_exprs: Arc::new(BTreeMap::new()),
            learned_no_goods: Arc::new(Vec::new()),
            learned_search_no_goods: Arc::new(Vec::new()),
            completed_invalidated: false,
            recorded_delta: None,
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

impl EvalScheduler {
    pub(super) fn clone_for_lease(&self, namespace: Option<u64>) -> Self {
        Self {
            next_id: self.next_id,
            next_work_id: self.next_work_id,
            next_wave_id: self.next_wave_id,
            next_lease_namespace: self.next_lease_namespace,
            next_lease_sequence: self.next_lease_sequence,
            active_wave: None,
            lease_namespace: namespace.map(|id| LeaseNamespace {
                id,
                next_node_local: 1,
                next_work_local: 1,
            }),
            node_states: Arc::clone(&self.node_states),
            node_records: Arc::clone(&self.node_records),
            node_state_overrides: BTreeMap::new(),
            node_record_overrides: BTreeMap::new(),
            keyed_nodes: Arc::clone(&self.keyed_nodes),
            dependency_edges: Arc::clone(&self.dependency_edges),
            ready_queue: VecDeque::new(),
            ready_tasks: BTreeMap::new(),
            baseline_ready_tasks: self.ready_tasks.keys().copied().collect(),
            in_flight_tasks: BTreeMap::new(),
            buffered_completions: BTreeMap::new(),
            pending_resumes: Arc::clone(&self.pending_resumes),
            pending_kinds: Arc::clone(&self.pending_kinds),
            retained_exprs: Arc::clone(&self.retained_exprs),
            learned_no_goods: Arc::clone(&self.learned_no_goods),
            learned_search_no_goods: Arc::clone(&self.learned_search_no_goods),
            completed_invalidated: false,
            recorded_delta: None,
        }
    }

    pub(super) fn begin_recording_delta(&mut self) {
        self.recorded_delta = Some(Box::default());
    }

    pub(super) fn take_recorded_delta(&mut self) -> Option<EvalSchedulerDelta> {
        self.recorded_delta.take().map(|delta| delta.into_delta())
    }

    fn record_allocated_ids(&mut self) {
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta.next_id = self.next_id;
            delta.next_work_id = self.next_work_id;
        }
    }

    fn record_keyed_node_delta(&mut self, node_id: EvalNodeId) {
        if let Some(delta) = self.recorded_delta.as_mut()
            && let Some(keyed) = self.keyed_nodes.get(&node_id)
        {
            delta.keyed_nodes.insert(node_id, keyed.clone());
        }
    }

    pub(super) fn dependency_edges(&self) -> &[EvalDependencyEdge] {
        &self.dependency_edges
    }

    pub(super) fn node_record(&self, node_id: EvalNodeId) -> Option<&EvalNodeRecord> {
        self.node_record_overrides
            .get(&node_id)
            .or_else(|| self.node_records.get(&node_id))
    }

    pub(super) fn node_records(&self) -> Vec<EvalNodeRecord> {
        self.node_record_ids()
            .into_iter()
            .filter_map(|node_id| self.node_record(node_id).cloned())
            .collect()
    }

    pub(super) fn node_record_ids(&self) -> BTreeSet<EvalNodeId> {
        let mut ids = self.node_records.keys().copied().collect::<BTreeSet<_>>();
        ids.extend(self.node_record_overrides.keys().copied());
        ids
    }

    pub(super) fn node_state_ids(&self) -> BTreeSet<EvalNodeId> {
        let mut ids = self.node_states.keys().copied().collect::<BTreeSet<_>>();
        ids.extend(self.node_state_overrides.keys().copied());
        ids
    }

    pub(super) fn node_record_count(&self) -> usize {
        self.node_record_ids().len()
    }

    pub(super) fn node_state_count(&self) -> usize {
        self.node_state_ids().len()
    }

    pub(super) fn record_node(&mut self, record: EvalNodeRecord) {
        let node_id = record.node_id;
        if self.lease_namespace.is_some() {
            if self.node_record(node_id).is_none() {
                self.node_record_overrides.insert(node_id, record.clone());
                if let Some(delta) = self.recorded_delta.as_mut() {
                    delta.node_records.insert(node_id, record);
                }
            }
            return;
        }
        let node_records = Arc::make_mut(&mut self.node_records);
        if let Entry::Vacant(entry) = node_records.entry(node_id) {
            entry.insert(record.clone());
            if let Some(delta) = self.recorded_delta.as_mut() {
                delta.node_records.insert(node_id, record);
            }
        }
    }

    pub(super) fn record_dependency(&mut self, parent: EvalNodeId, child: EvalNodeId) {
        let edge = EvalDependencyEdge { parent, child };
        Arc::make_mut(&mut self.dependency_edges).push(edge.clone());
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta.dependency_edges.push(edge);
        }
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
        let node_id = if let Some(namespace) = &mut self.lease_namespace {
            let node_id = EvalNodeId(lease_scoped_id(namespace.id, namespace.next_node_local));
            namespace.next_node_local += 1;
            node_id
        } else {
            let node_id = EvalNodeId(self.next_id);
            self.next_id += 1;
            node_id
        };
        self.record_allocated_ids();
        node_id
    }

    pub(super) fn alloc_pending(&mut self, description: impl Into<Arc<str>>) -> PendingRef {
        let pending_id = self.alloc_node_id();
        let description = description.into();
        let pending = PendingRef::new(pending_id.0, description.clone());
        Arc::make_mut(&mut self.pending_kinds).insert(pending.node_id, PendingKind::External);
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta
                .pending_kinds
                .insert(pending.node_id, PendingKind::External);
        }
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
        Arc::make_mut(&mut self.pending_kinds)
            .insert(pending.node_id, PendingKind::ChildDependency { dependency });
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta
                .pending_kinds
                .insert(pending.node_id, PendingKind::ChildDependency { dependency });
        }
        pending
    }

    pub(super) fn pending_nodes(&self) -> Vec<PendingRef> {
        self.pending_resumes
            .values()
            .filter_map(|suspensions| {
                let suspension = suspensions.first()?;
                matches!(
                    self.pending_kinds.get(&suspension.pending.node_id),
                    Some(PendingKind::External)
                )
                .then(|| suspension.pending.clone())
            })
            .collect()
    }

    pub(super) fn pending_kind(&self, pending: &PendingRef) -> Option<PendingKind> {
        self.pending_kinds.get(&pending.node_id).copied()
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
        self.node_state_overrides
            .get(&node_id)
            .or_else(|| self.node_states.get(&node_id))
    }

    pub(super) fn keyed_node(&self, node_id: EvalNodeId) -> Option<&KeyedNodeMemo> {
        self.keyed_nodes.get(&node_id)
    }

    fn keyed_node_mut(&mut self, node_id: EvalNodeId) -> Option<&mut KeyedNodeMemo> {
        Arc::make_mut(&mut self.keyed_nodes).get_mut(&node_id)
    }

    fn find_keyed_node(
        &self,
        resolver: &FunctionRef,
        key: &Value,
        branch_context: &EvalBranchContext,
    ) -> Result<Option<EvalNodeId>, EvalError> {
        self.keyed_nodes
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
        self.ready_tasks.contains_key(&work_id) || self.baseline_ready_tasks.contains(&work_id)
    }

    pub(super) fn remember_expr_task(
        &mut self,
        node_id: EvalNodeId,
        scheduled: ScheduledExpr,
        branch_context: EvalBranchContext,
    ) {
        Arc::make_mut(&mut self.retained_exprs)
            .entry(node_id)
            .or_insert(RetainedExprTask {
                scheduled,
                branch_context,
            });
        if let Some(delta) = self.recorded_delta.as_mut()
            && let Some(retained) = self.retained_exprs.get(&node_id)
        {
            delta
                .retained_exprs
                .entry(node_id)
                .or_insert(retained.clone());
        }
    }

    pub(super) fn learned_no_good(
        &self,
        branch_context: &EvalBranchContext,
        assumption: &Arc<str>,
        candidates: &[Value],
        index: usize,
        resolver: &FunctionRef,
    ) -> Result<Option<EvalError>, EvalError> {
        self.learned_no_goods
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
        self.learned_search_no_goods
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
        Ok(self.learned_no_goods.iter().any(|existing| {
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
        let learned_search_no_goods = Arc::make_mut(&mut self.learned_search_no_goods);
        for existing in learned_search_no_goods.iter_mut() {
            if existing.branch_context == learned.branch_context
                && existing.assumption == learned.assumption
                && existing.resolver == learned.resolver
                && candidate_lists_equal(existing.candidates.as_ref(), learned.candidates.as_ref())?
            {
                if errors_equal_for_learning(&existing.error, &learned.error)? {
                    return Ok(());
                }
                *existing = learned.clone();
                if let Some(delta) = self.recorded_delta.as_mut() {
                    delta.learned_search_no_goods.push(existing.clone());
                }
                return Ok(());
            }
        }
        learned_search_no_goods.push(learned.clone());
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta.learned_search_no_goods.push(learned);
        }
        Ok(())
    }

    pub(super) fn record_learned_no_good(
        &mut self,
        learned: LearnedNoGood,
    ) -> Result<(), EvalError> {
        if self.covers_learned_no_good(&learned)? {
            return Ok(());
        }
        let learned_no_goods = Arc::make_mut(&mut self.learned_no_goods);
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
                    if let Some(delta) = self.recorded_delta.as_mut() {
                        delta.learned_no_goods.push(existing.clone());
                    }
                    return Ok(());
                }
            }
        }
        learned_no_goods.push(learned.clone());
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta.learned_no_goods.push(learned);
        }
        Ok(())
    }

    fn enqueue_expr_work(&mut self, node_id: EvalNodeId) -> Result<(), EvalError> {
        let retained = self.retained_exprs.get(&node_id).cloned().ok_or_else(|| {
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
        self.dependency_edges
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
                    if self.keyed_nodes.contains_key(&node_id) {
                        self.enqueue_keyed_work(node_id)?;
                        true
                    } else if self.retained_exprs.contains_key(&node_id) {
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

    fn enqueue_keyed_work(&mut self, node_id: EvalNodeId) -> Result<(), EvalError> {
        let branch_context = if let Some(memo) = self.keyed_nodes.get(&node_id) {
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
        let keyed = Arc::make_mut(&mut self.keyed_nodes)
            .get_mut(&node_id)
            .expect("keyed node must still exist");
        keyed.active_revision = Some(keyed.revision);
        keyed.evaluated_revision = None;
        self.record_keyed_node_delta(node_id);
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
                self.record_keyed_node_delta(node_id);
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
        Arc::make_mut(&mut self.keyed_nodes).insert(
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
        self.record_keyed_node_delta(node_id);
        self.enqueue_keyed_work(node_id)?;
        Ok(node_id)
    }

    pub(super) fn finalize_keyed_completion(
        &mut self,
        node_id: EvalNodeId,
    ) -> Result<bool, EvalError> {
        let Some((active_revision, current_revision)) = self
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
        let memo = Arc::make_mut(&mut self.keyed_nodes)
            .get_mut(&node_id)
            .expect("keyed node must exist");
        memo.evaluated_revision = Some(active_revision);
        memo.active_revision = None;
        self.record_keyed_node_delta(node_id);
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
        if self.lease_namespace.is_some() {
            self.node_state_overrides.insert(node_id, state.clone());
            if let Some(delta) = self.recorded_delta.as_mut() {
                delta.node_states.insert(node_id, state);
            }
            return;
        }
        Arc::make_mut(&mut self.node_states).insert(node_id, state.clone());
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta.node_states.insert(node_id, state);
        }
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
        let Some(suspensions) = Arc::make_mut(&mut self.pending_resumes).remove(&pending_id) else {
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
        Arc::make_mut(&mut self.pending_kinds).remove(&pending_id);
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
        Arc::make_mut(&mut self.pending_resumes)
            .entry(suspension.pending.node_id)
            .or_default()
            .push(suspension.clone());
        if let Some(delta) = self.recorded_delta.as_mut() {
            delta
                .pending_resumes
                .entry(suspension.pending.node_id)
                .or_default()
                .push(suspension);
        }
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
        self.next_id = self.next_id.max(delta.next_id);
        self.next_work_id = self.next_work_id.max(delta.next_work_id);
        let node_records = Arc::make_mut(&mut self.node_records);
        for record in delta.node_records {
            match node_records.get(&record.node_id) {
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
                    node_records.insert(record.node_id, record);
                }
            }
        }
        let keyed_nodes = Arc::make_mut(&mut self.keyed_nodes);
        for keyed in delta.keyed_nodes {
            match keyed_nodes.get_mut(&keyed.node_id) {
                Some(existing) => merge_keyed_node(existing, keyed)?,
                None => {
                    keyed_nodes.insert(keyed.node_id, keyed);
                }
            }
        }
        Arc::make_mut(&mut self.dependency_edges).extend(delta.dependency_edges);
        let pending_kinds = Arc::make_mut(&mut self.pending_kinds);
        for (pending_id, kind) in delta.pending_kinds {
            match pending_kinds.get(&pending_id) {
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
                    pending_kinds.insert(pending_id, kind);
                }
            }
        }
        let pending_resumes = Arc::make_mut(&mut self.pending_resumes);
        for (pending_id, suspension) in delta.pending_resumes {
            let suspensions = pending_resumes.entry(pending_id).or_default();
            if !suspensions.contains(&suspension) {
                suspensions.push(suspension);
            }
        }
        let node_states = Arc::make_mut(&mut self.node_states);
        for (node_id, state) in delta.node_states {
            node_states.insert(node_id, state);
        }
        let retained_exprs = Arc::make_mut(&mut self.retained_exprs);
        for (node_id, retained) in delta.retained_exprs {
            match retained_exprs.get(&node_id) {
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
                    retained_exprs.insert(node_id, retained);
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
            id: EvalWorkId(if let Some(namespace) = &mut self.lease_namespace {
                let work_id = lease_scoped_id(namespace.id, namespace.next_work_local);
                namespace.next_work_local += 1;
                work_id
            } else {
                let work_id = self.next_work_id;
                self.next_work_id += 1;
                work_id
            }),
            kind,
            node_id,
            pending,
        };
        self.record_allocated_ids();
        self.ready_queue.push_back(work.id);
        self.ready_tasks.insert(
            work.id,
            ReadyTaskRecord {
                work: work.clone(),
                task,
                branch_context,
            },
        );
        self.baseline_ready_tasks.remove(&work.id);
        if let Some(delta) = self.recorded_delta.as_mut()
            && let Some(record) = self.ready_tasks.get(&work.id)
        {
            delta.queued.push(record.clone());
        }
        work
    }
}

fn lease_scoped_id(namespace: u64, local: u64) -> u64 {
    const LEASE_FLAG: u64 = 1 << 63;
    LEASE_FLAG | (namespace << 32) | local
}

impl EvalSchedulerDelta {
    pub(super) fn from_diff(before: &EvalScheduler, after: &EvalScheduler) -> Self {
        let node_records = after
            .node_record_ids()
            .into_iter()
            .filter_map(|node_id| {
                let record = after
                    .node_record(node_id)
                    .expect("node record id should resolve to a record");
                (!matches!(before.node_record(node_id), Some(existing) if existing == record))
                    .then(|| record.clone())
            })
            .collect();
        let node_states = after
            .node_state_ids()
            .into_iter()
            .filter_map(|node_id| {
                let state = after
                    .node_state(node_id)
                    .expect("node state id should resolve to a state");
                (!matches!(before.node_state(node_id), Some(existing) if existing == state))
                    .then(|| (node_id, state.clone()))
            })
            .collect();
        let keyed_nodes = after
            .keyed_nodes
            .iter()
            .filter_map(|(node_id, keyed)| {
                (!matches!(before.keyed_nodes.get(node_id), Some(existing) if existing == keyed))
                    .then(|| keyed.clone())
            })
            .collect();
        let dependency_edges = after.dependency_edges[before.dependency_edges.len()..].to_vec();
        let queued = after
            .ready_queue
            .iter()
            .skip(before.ready_queue.len())
            .filter_map(|work_id| after.ready_tasks.get(work_id).cloned())
            .collect();
        let pending_resumes = after
            .pending_resumes
            .iter()
            .flat_map(|(pending_id, suspensions)| {
                let before_suspensions = before.pending_resumes.get(pending_id);
                suspensions
                    .iter()
                    .filter(move |suspension| {
                        !before_suspensions.is_some_and(|existing| existing.contains(suspension))
                    })
                    .cloned()
                    .map(|suspension| (*pending_id, suspension))
            })
            .collect();
        let pending_kinds = after
            .pending_kinds
            .iter()
            .filter_map(|(pending_id, kind)| {
                (!matches!(before.pending_kinds.get(pending_id), Some(existing) if existing == kind))
                    .then(|| (*pending_id, *kind))
            })
            .collect();
        let retained_exprs = after
            .retained_exprs
            .iter()
            .filter_map(|(node_id, retained)| {
                (!matches!(before.retained_exprs.get(node_id), Some(existing) if existing == retained))
                    .then(|| (*node_id, retained.clone()))
            })
            .collect();
        let learned_no_goods = after
            .learned_no_goods
            .iter()
            .filter(|learned| !before.learned_no_goods.contains(*learned))
            .cloned()
            .collect();
        let learned_search_no_goods = after
            .learned_search_no_goods
            .iter()
            .filter(|learned| !before.learned_search_no_goods.contains(*learned))
            .cloned()
            .collect();
        Self {
            next_id: after.next_id,
            next_work_id: after.next_work_id,
            node_records,
            keyed_nodes,
            node_states,
            dependency_edges,
            queued,
            pending_resumes,
            pending_kinds,
            retained_exprs,
            learned_no_goods,
            learned_search_no_goods,
        }
    }

    pub(super) fn retain_changes_against(&mut self, before: &EvalScheduler) {
        self.node_records.retain(|record| {
            !matches!(
                before.node_records.get(&record.node_id),
                Some(existing) if existing == record
            )
        });
        self.node_states.retain(|(node_id, state)| {
            !matches!(
                before.node_states.get(node_id),
                Some(existing) if existing == state
            )
        });
        self.keyed_nodes.retain(|keyed| {
            !matches!(
                before.keyed_nodes.get(&keyed.node_id),
                Some(existing) if existing == keyed
            )
        });
        self.pending_kinds.retain(|(pending_id, kind)| {
            !matches!(
                before.pending_kinds.get(pending_id),
                Some(existing) if existing == kind
            )
        });
        self.pending_resumes.retain(|(pending_id, suspension)| {
            !before
                .pending_resumes
                .get(pending_id)
                .is_some_and(|existing| existing.contains(suspension))
        });
        self.retained_exprs.retain(|(node_id, retained)| {
            !matches!(
                before.retained_exprs.get(node_id),
                Some(existing) if existing == retained
            )
        });
        self.learned_no_goods
            .retain(|learned| !before.learned_no_goods.contains(learned));
        self.learned_search_no_goods
            .retain(|learned| !before.learned_search_no_goods.contains(learned));
    }
}

pub(super) fn execute_ready_task(
    program: Arc<CompiledProgram>,
    entry: &str,
    args: &[Value],
    runtime: &impl RuntimeHooks,
    place: &str,
    scheduler: EvalScheduler,
    record: ReadyTaskRecord,
) -> Result<(EvalSessionDelta, EvalCompletedWorkOutcome), EvalError> {
    let validate_recorded_delta = std::env::var_os("VX_EVAL2_VALIDATE_RECORDED_DELTAS").is_some();
    let mut after = scheduler;
    let before = validate_recorded_delta.then(|| after.clone());
    after.begin_recording_delta();
    let mut trace = EvalTrace::default();
    let mut state = EvalState::new(
        program.as_ref(),
        program.entry_source(),
        record.branch_context.clone(),
        runtime,
        place,
        &mut trace,
        &mut after,
        Some(record.work.id),
        true,
    );

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
                source: Some(body_source),
            });
            let scheduled = ScheduledExpr {
                expr: ExprHandle {
                    key: ProgramNodeHandle::for_node(prepared.source, &prepared.body),
                },
                env: prepared.env,
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
            match state.with_source(scheduled.expr.key.source(), |state| {
                eval_expr_with_node_id(state, record.work.node_id, &expr, &mut env)
            }) {
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
                                format!("pending node {} has no registered kind", pending.node_id),
                            ));
                        }
                    }
                }
            }
        }
        ReadyTask::Resume(ready) => {
            match resume_suspension(&mut state, ready.suspension, ready.result) {
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

    let mut recorded_delta = after
        .take_recorded_delta()
        .expect("worker scheduler should record a delta");
    recorded_delta.next_id = after.next_id;
    recorded_delta.next_work_id = after.next_work_id;
    if let Some(before) = before {
        let mut filtered_delta = recorded_delta.clone();
        filtered_delta.retain_changes_against(&before);
        let diff_delta = EvalSchedulerDelta::from_diff(&before, &after);
        debug_assert_eq!(filtered_delta, diff_delta);
    }

    Ok((
        EvalSessionDelta {
            scheduler: recorded_delta,
            trace_events: trace.events,
            completed_invalidated: after.completed_invalidated,
        },
        outcome,
    ))
}
