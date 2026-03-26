use super::*;
use std::collections::BTreeMap;
use std::env;
use std::time::{Duration, Instant};

pub struct EvalSession<'runtime, Runtime: RuntimeHooks> {
    program: Arc<CompiledProgram>,
    entry: Arc<str>,
    args: Arc<[Value]>,
    runtime: &'runtime Runtime,
    place: Arc<str>,
    branch_context: EvalBranchContext,
    scheduler: EvalScheduler,
    trace: EvalTrace,
    completed_work: Vec<EvalCompletedWork>,
    completed: Option<Result<Value, EvalError>>,
    owner_commit_profile: EvalOwnerCommitProfile,
    trace_enabled: bool,
    completed_work_enabled: bool,
    history: Vec<EvalSessionCheckpoint>,
    call_summary: bool,
    call_profiling: bool,
}

struct EvalParallelWaveCheckpoint {
    ready_queue: VecDeque<EvalWorkId>,
    ready_tasks: EvalWorkTable<ReadyTaskRecord>,
    in_flight_tasks: EvalWorkTable<InFlightTaskRecord>,
    buffered_completions: BTreeMap<u64, EvalWorkCompletion>,
    active_wave: Option<EvalWaveId>,
    next_wave_id: u64,
    next_lease_namespace: u64,
    next_lease_sequence: u64,
}

enum ParallelWorkerCommand<'runtime, Runtime: RuntimeHooks> {
    Execute(EvalPreparedWork<'runtime, Runtime>),
    Shutdown,
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
        let branch_context = EvalBranchContext::default();
        scheduler.enqueue_start_entry(branch_context.clone());
        let program = Arc::new(CompiledProgram::new(source_id, source));
        Self::new_with_program(
            program,
            entry,
            args,
            runtime,
            place,
            scheduler,
            branch_context,
        )
    }

    pub fn new_program(
        program: EvalProgram,
        entry: impl Into<String>,
        args: Vec<Value>,
        runtime: &'runtime Runtime,
        place: impl Into<String>,
    ) -> Self {
        let mut scheduler = EvalScheduler::default();
        let branch_context = EvalBranchContext::default();
        scheduler.enqueue_start_entry(branch_context.clone());
        let program = Arc::new(CompiledProgram::from_eval_program(program));
        Self::new_with_program(
            program,
            entry,
            args,
            runtime,
            place,
            scheduler,
            branch_context,
        )
    }

    fn new_with_program(
        program: Arc<CompiledProgram>,
        entry: impl Into<String>,
        args: Vec<Value>,
        runtime: &'runtime Runtime,
        place: impl Into<String>,
        scheduler: EvalScheduler,
        branch_context: EvalBranchContext,
    ) -> Self {
        Self {
            program,
            entry: Arc::from(entry.into()),
            args: Arc::from(args),
            runtime,
            place: Arc::from(place.into()),
            branch_context,
            scheduler,
            trace: EvalTrace::default(),
            completed_work: Vec::new(),
            completed: None,
            owner_commit_profile: EvalOwnerCommitProfile::default(),
            trace_enabled: cfg!(test),
            completed_work_enabled: cfg!(test) || env::var_os("VX_EVAL2_PROFILE_CALLS").is_some(),
            history: Vec::new(),
            call_summary: false,
            call_profiling: env::var_os("VX_EVAL2_PROFILE_CALLS").is_some(),
        }
    }

    pub fn run(&mut self) -> Result<Resolution, EvalError> {
        loop {
            match self.drive_step_without_history()? {
                EvalSessionStep::Progressed => {}
                EvalSessionStep::Pending(pending) => return Ok(Resolution::Pending(pending)),
                EvalSessionStep::WaitingOnInFlight => return Ok(Resolution::Waiting),
                EvalSessionStep::Ready(value) => return Ok(Resolution::Ready(value)),
                EvalSessionStep::Poison(error) => return Ok(Resolution::Poison(error)),
                EvalSessionStep::Idle => {
                    return Ok(resolution_without_ready_work(
                        &self.scheduler,
                        &self.completed,
                    ));
                }
            }
        }
    }

    pub(crate) fn run_with_event_sink(
        &mut self,
        sink: &mut impl EvalEventSink,
        scrub_work_ids: bool,
    ) -> Result<Resolution, EvalError> {
        self.trace_enabled = true;
        let mut emitted = 0;
        loop {
            let step = self.drive_step_without_history()?;
            emit_new_trace_events(&self.trace, &mut emitted, sink, scrub_work_ids);
            match step {
                EvalSessionStep::Progressed => {}
                EvalSessionStep::Pending(pending) => return Ok(Resolution::Pending(pending)),
                EvalSessionStep::WaitingOnInFlight => return Ok(Resolution::Waiting),
                EvalSessionStep::Ready(value) => return Ok(Resolution::Ready(value)),
                EvalSessionStep::Poison(error) => return Ok(Resolution::Poison(error)),
                EvalSessionStep::Idle => {
                    return Ok(resolution_without_ready_work(
                        &self.scheduler,
                        &self.completed,
                    ));
                }
            }
        }
    }

    fn drive_step_without_history(&mut self) -> Result<EvalSessionStep, EvalError> {
        if let Some(result) = &self.completed {
            return Ok(match result {
                Ok(value) => EvalSessionStep::Ready(value.clone()),
                Err(error) => EvalSessionStep::Poison(error.clone()),
            });
        }

        let Some(prepared) = self.prepare_step_impl(false, false)? else {
            return Ok(step_without_ready_work(&self.scheduler, &self.completed));
        };
        let completion = prepared.execute()?;
        self.apply_completion_impl(completion, false)
    }

    pub fn run_parallel(&mut self, max_workers: usize) -> Result<Resolution, EvalError> {
        if max_workers == 0 {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "parallel eval requires at least one worker",
            ));
        }

        if let Some(resolution) = completed_resolution(&self.completed) {
            return Ok(resolution);
        }

        thread::scope(|scope| -> Result<Resolution, EvalError> {
            let (completion_tx, completion_rx) = mpsc::channel::<ParallelWorkerMessage>();
            let mut worker_commands = Vec::with_capacity(max_workers);
            for _ in 0..max_workers {
                let (command_tx, command_rx) =
                    mpsc::sync_channel::<ParallelWorkerCommand<'runtime, Runtime>>(1);
                let completion_tx = completion_tx.clone();
                scope.spawn(move || {
                    while let Ok(command) = command_rx.recv() {
                        let message = match command {
                            ParallelWorkerCommand::Execute(prepared) => {
                                match catch_unwind(AssertUnwindSafe(|| prepared.execute())) {
                                    Ok(completion) => ParallelWorkerMessage::Completion(completion),
                                    Err(payload) => ParallelWorkerMessage::Panic(payload),
                                }
                            }
                            ParallelWorkerCommand::Shutdown => break,
                        };
                        if completion_tx.send(message).is_err() {
                            break;
                        }
                    }
                });
                worker_commands.push(command_tx);
            }
            drop(completion_tx);

            loop {
                let wave_checkpoint = self.parallel_wave_checkpoint();
                let prepared = self.lease_parallel_wave(max_workers)?;
                if prepared.is_empty() {
                    for command_tx in worker_commands.drain(..) {
                        let _ = command_tx.send(ParallelWorkerCommand::Shutdown);
                    }
                    return Ok(resolution_without_ready_work(
                        &self.scheduler,
                        &self.completed,
                    ));
                }
                match catch_unwind(AssertUnwindSafe(|| {
                    self.execute_parallel_wave(&worker_commands, &completion_rx, prepared)
                })) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        for command_tx in worker_commands.drain(..) {
                            let _ = command_tx.send(ParallelWorkerCommand::Shutdown);
                        }
                        self.restore_parallel_wave_checkpoint(wave_checkpoint);
                        return Err(error);
                    }
                    Err(payload) => {
                        for command_tx in worker_commands.drain(..) {
                            let _ = command_tx.send(ParallelWorkerCommand::Shutdown);
                        }
                        self.restore_parallel_wave_checkpoint(wave_checkpoint);
                        resume_unwind(payload);
                    }
                }
            }
        })
    }

    pub fn step(&mut self) -> Result<EvalSessionStep, EvalError> {
        if let Some(result) = &self.completed {
            return Ok(match result {
                Ok(value) => EvalSessionStep::Ready(value.clone()),
                Err(error) => EvalSessionStep::Poison(error.clone()),
            });
        }

        let snapshot = self.checkpoint();

        let Some(prepared) = self.prepare_step_impl(false, false)? else {
            return Ok(step_without_ready_work(&self.scheduler, &self.completed));
        };
        let completion = prepared.execute()?;
        match self.apply_completion_impl(completion, false) {
            Ok(step) => {
                self.history.push(snapshot);
                Ok(step)
            }
            Err(error) => {
                self.restore(snapshot);
                Err(error)
            }
        }
    }

    pub fn resolve_pending(&mut self, pending: &PendingRef, value: Value) -> Result<(), EvalError> {
        self.scheduler
            .resolve_pending(pending, Ok(value), &mut self.owner_commit_profile)?;
        Ok(())
    }

    pub fn poison_pending(
        &mut self,
        pending: &PendingRef,
        error: EvalError,
    ) -> Result<(), EvalError> {
        self.scheduler
            .resolve_pending(pending, Err(error), &mut self.owner_commit_profile)?;
        Ok(())
    }

    pub fn pending_nodes(&self) -> Vec<PendingRef> {
        self.scheduler.pending_nodes()
    }

    pub fn set_trace_enabled(&mut self, enabled: bool) {
        self.trace_enabled = enabled;
    }

    pub fn set_completed_work_enabled(&mut self, enabled: bool) {
        self.completed_work_enabled = enabled;
    }

    pub fn trace_enabled(&self) -> bool {
        self.trace_enabled
    }

    pub fn set_call_profiling_enabled(&mut self, enabled: bool) {
        self.call_profiling = enabled;
        if enabled {
            self.completed_work_enabled = true;
        }
    }

    pub fn call_profiling_enabled(&self) -> bool {
        self.call_profiling
    }

    pub fn set_call_summary_enabled(&mut self, enabled: bool) {
        self.call_summary = enabled;
        if enabled {
            self.completed_work_enabled = true;
        }
    }

    pub fn call_summary_enabled(&self) -> bool {
        self.call_summary || self.call_profiling
    }

    pub fn owner_commit_profile(&self) -> &EvalOwnerCommitProfile {
        &self.owner_commit_profile
    }

    pub fn branch_context(&self) -> &EvalBranchContext {
        &self.branch_context
    }

    pub fn set_branch_context(&mut self, branch_context: EvalBranchContext) {
        let previous = self.branch_context.clone();
        self.branch_context = branch_context;
        self.scheduler
            .replace_queued_branch_context(&previous, &self.branch_context);
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

    pub fn in_flight_work(&self) -> Vec<EvalQueuedWork> {
        self.scheduler.in_flight_work()
    }

    pub fn active_wave(&self) -> Option<EvalActiveWave> {
        self.scheduler.active_wave()
    }

    pub fn buffered_work(&self) -> Vec<EvalBufferedWork> {
        self.scheduler.buffered_work()
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

    pub fn dependency_edges(&self) -> Vec<EvalDependencyEdge> {
        self.scheduler.dependency_edges().cloned().collect()
    }

    pub fn explain_node(&self, node_id: EvalNodeId) -> EvalNodeExplanation {
        self.scheduler.explain_node(node_id)
    }

    pub fn trace(&self) -> &EvalTrace {
        &self.trace
    }

    pub fn base_generation(&self) -> u64 {
        self.scheduler.base_generation()
    }

    pub fn stats(&self) -> EvalSessionStats {
        let mut graph_nodes = self.scheduler.node_record_ids();
        graph_nodes.extend(self.scheduler.node_state_ids());
        graph_nodes.extend(self.scheduler.base.keyed_nodes.keys());
        for edge in self.scheduler.base.dependency_edges.iter() {
            graph_nodes.insert(edge.parent);
            graph_nodes.insert(edge.child);
        }

        EvalSessionStats {
            graph_node_count: graph_nodes.len(),
            node_record_count: self.scheduler.node_record_count(),
            node_state_count: self.scheduler.node_state_count(),
            keyed_node_count: self.scheduler.base.keyed_nodes.len(),
            dependency_edge_count: self.scheduler.base.dependency_edges.len(),
            queued_work_count: self.scheduler.ready_queue.len(),
            in_flight_work_count: self.scheduler.in_flight_tasks.len(),
            buffered_work_count: self.scheduler.buffered_completions.len(),
            pending_count: self.scheduler.pending_nodes().len(),
            completed_work_count: self.completed_work.len(),
            trace_event_count: self.trace.events.len(),
            learned_no_good_count: self.scheduler.base.learned_no_goods.len(),
            widened_no_good_count: self
                .scheduler
                .base
                .learned_no_goods
                .iter()
                .filter(|learned| learned.start_index < learned.end_index)
                .count(),
            learned_search_no_good_count: self.scheduler.base.learned_search_no_goods.len(),
        }
    }

    pub fn profile(&self) -> EvalSessionProfile {
        let mut entries = BTreeMap::<(EvalNodeId, EvalWorkKind), EvalProfileEntry>::new();
        let mut total_time = Duration::default();

        for completed in &self.completed_work {
            total_time += completed.elapsed;
            let key = (completed.work.node_id, completed.work.kind);
            let record = self.node_record(completed.work.node_id);
            let label = record
                .map(profile_label_for_record)
                .unwrap_or_else(|| Arc::from("<unknown>"));
            let source = record.and_then(|record| record.source.clone());
            let entry = entries.entry(key).or_insert_with(|| EvalProfileEntry {
                node_id: completed.work.node_id,
                work_kind: completed.work.kind,
                label,
                source,
                runs: 0,
                total_time: Duration::default(),
                max_time: Duration::default(),
            });
            entry.runs += 1;
            entry.total_time += completed.elapsed;
            entry.max_time = entry.max_time.max(completed.elapsed);
        }

        let mut entries = entries.into_values().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .total_time
                .cmp(&left.total_time)
                .then_with(|| right.max_time.cmp(&left.max_time))
                .then_with(|| left.node_id.cmp(&right.node_id))
        });

        EvalSessionProfile {
            total_time,
            entries,
        }
    }

    pub fn profile_report(&self, limit: usize) -> String {
        format_profile_summary_report(&self.profile_summary(), limit)
    }

    pub(crate) fn causal_profile(&self) -> EvalSessionCausalProfile {
        let mut entries = HashMap::<
            (
                EvalWorkKind,
                EvalWorkCauseKind,
                Arc<str>,
                Option<SourceSpan>,
            ),
            EvalCausalProfileEntry,
        >::new();
        let mut total_time = Duration::default();

        for completed in &self.completed_work {
            if completed.work.kind == EvalWorkKind::ResumePending {
                continue;
            }
            if let Some(invocation) = &completed.invocation
                && completed.work.node_id != invocation.root_node_id
            {
                continue;
            }

            total_time += completed.elapsed;
            let record = self.node_record(completed.work.node_id);
            let label = causal_profile_label_for_completed(completed, record);
            let source = causal_profile_source_for_completed(completed, record);
            let key = (
                completed.work.kind,
                completed.work.cause.kind,
                label.clone(),
                source.clone(),
            );
            let entry = entries
                .entry(key)
                .or_insert_with(|| EvalCausalProfileEntry {
                    work_kind: completed.work.kind,
                    cause_kind: completed.work.cause.kind,
                    label,
                    source,
                    runs: 0,
                    total_time: Duration::default(),
                    max_time: Duration::default(),
                });
            entry.runs += 1;
            entry.total_time += completed.elapsed;
            entry.max_time = entry.max_time.max(completed.elapsed);
        }

        let mut entries = entries.into_values().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .total_time
                .cmp(&left.total_time)
                .then_with(|| right.max_time.cmp(&left.max_time))
                .then_with(|| right.runs.cmp(&left.runs))
                .then_with(|| left.label.cmp(&right.label))
        });

        EvalSessionCausalProfile {
            total_time,
            entries,
        }
    }

    pub fn causal_profile_report(&self, limit: usize) -> String {
        format_causal_profile_report(self, &self.causal_profile(), limit)
    }

    pub fn owner_commit_profile_report(&self) -> String {
        format_owner_commit_profile_report(&self.owner_commit_profile)
    }

    pub fn profile_summary(&self) -> EvalSessionProfileSummary {
        let profile = self.profile();
        let mut entries =
            BTreeMap::<(EvalWorkKind, Arc<str>, Option<String>), EvalProfileSummaryEntry>::new();

        for entry in profile.entries {
            let source_key = entry.source.as_ref().map(|source| format!("{source:?}"));
            let key = (entry.work_kind, entry.label.clone(), source_key);
            let summary = entries
                .entry(key)
                .or_insert_with(|| EvalProfileSummaryEntry {
                    work_kind: entry.work_kind,
                    label: entry.label.clone(),
                    source: entry.source.clone(),
                    node_count: 0,
                    runs: 0,
                    total_time: Duration::default(),
                    max_time: Duration::default(),
                });
            summary.node_count += 1;
            summary.runs += entry.runs;
            summary.total_time += entry.total_time;
            summary.max_time = summary.max_time.max(entry.max_time);
        }

        let mut entries = entries.into_values().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .total_time
                .cmp(&left.total_time)
                .then_with(|| right.max_time.cmp(&left.max_time))
                .then_with(|| right.node_count.cmp(&left.node_count))
                .then_with(|| left.label.cmp(&right.label))
        });

        EvalSessionProfileSummary {
            total_time: profile.total_time,
            entries,
        }
    }

    pub fn call_profile(&self) -> EvalSessionCallProfile {
        let mut runs = BTreeMap::<(Option<EvalWorkId>, EvalNodeId), EvalCallProfileRun>::new();
        let mut attributed_time = Duration::default();

        for completed in &self.completed_work {
            let Some(invocation) = completed.invocation.clone() else {
                continue;
            };
            attributed_time += completed.elapsed;
            let key = (invocation.root_work_id, invocation.root_node_id);
            let run = runs.entry(key).or_insert_with(|| EvalCallProfileRun {
                invocation,
                work_count: 0,
                self_time: Duration::default(),
                total_time: Duration::default(),
                max_work_time: Duration::default(),
            });
            run.work_count += 1;
            run.self_time += completed.elapsed;
            run.max_work_time = run.max_work_time.max(completed.elapsed);
        }

        let mut runs = runs.into_values().collect::<Vec<_>>();
        let index_by_key = runs
            .iter()
            .enumerate()
            .map(|(index, run)| {
                (
                    (run.invocation.root_work_id, run.invocation.root_node_id),
                    index,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut children_by_parent =
            BTreeMap::<Option<(Option<EvalWorkId>, EvalNodeId)>, Vec<usize>>::new();

        for (index, run) in runs.iter().enumerate() {
            let parent_key = run
                .invocation
                .caller_node_id
                .map(|node_id| (run.invocation.caller_work_id, node_id));
            let bucket = parent_key
                .filter(|key| index_by_key.contains_key(key))
                .map(Some)
                .unwrap_or(None);
            children_by_parent.entry(bucket).or_default().push(index);
        }

        let root_indices = children_by_parent.remove(&None).unwrap_or_default();
        let mut total_cache = BTreeMap::<usize, Duration>::new();
        for root_index in &root_indices {
            compute_call_run_total_time(*root_index, &runs, &children_by_parent, &mut total_cache);
        }
        for (index, run) in runs.iter_mut().enumerate() {
            run.total_time = *total_cache.get(&index).unwrap_or(&run.self_time);
        }
        let roots = build_call_tree_entries(&root_indices, &runs, &children_by_parent);
        runs.sort_by(|left, right| {
            right
                .total_time
                .cmp(&left.total_time)
                .then_with(|| right.self_time.cmp(&left.self_time))
                .then_with(|| left.invocation.callee.cmp(&right.invocation.callee))
        });

        let mut summary = summarize_call_runs(&runs);
        summary.sort_by(|left, right| {
            right
                .total_time
                .cmp(&left.total_time)
                .then_with(|| right.self_time.cmp(&left.self_time))
                .then_with(|| left.callee.cmp(&right.callee))
        });

        EvalSessionCallProfile {
            attributed_time,
            runs,
            summary,
            roots,
        }
    }

    pub fn call_summary(&self) -> EvalSessionCallSummary {
        let mut runs = BTreeMap::<(Option<EvalWorkId>, EvalNodeId), EvalCallProfileRun>::new();
        let mut attributed_time = Duration::default();

        for completed in &self.completed_work {
            let Some(invocation) = completed.invocation.clone() else {
                continue;
            };
            attributed_time += completed.elapsed;
            let key = (invocation.root_work_id, invocation.root_node_id);
            let run = runs.entry(key).or_insert_with(|| EvalCallProfileRun {
                invocation,
                work_count: 0,
                self_time: Duration::default(),
                total_time: Duration::default(),
                max_work_time: Duration::default(),
            });
            run.work_count += 1;
            run.self_time += completed.elapsed;
            run.total_time += completed.elapsed;
            run.max_work_time = run.max_work_time.max(completed.elapsed);
        }

        let mut runs = runs.into_values().collect::<Vec<_>>();
        runs.sort_by(|left, right| {
            right
                .total_time
                .cmp(&left.total_time)
                .then_with(|| right.self_time.cmp(&left.self_time))
                .then_with(|| left.invocation.callee.cmp(&right.invocation.callee))
        });

        let mut summary = summarize_call_runs(&runs);
        summary.sort_by(|left, right| {
            right
                .total_time
                .cmp(&left.total_time)
                .then_with(|| right.self_time.cmp(&left.self_time))
                .then_with(|| left.callee.cmp(&right.callee))
        });

        EvalSessionCallSummary {
            attributed_time,
            runs,
            summary,
        }
    }

    pub fn call_profile_report(&self, limit: usize) -> String {
        format_call_profile_report(&self.call_profile(), limit)
    }

    pub fn call_summary_report(&self, limit: usize) -> String {
        format_call_summary_report(&self.call_summary(), limit)
    }

    pub fn completed_work(&self) -> &[EvalCompletedWork] {
        &self.completed_work
    }

    pub fn checkpoint(&self) -> EvalSessionCheckpoint {
        EvalSessionCheckpoint {
            branch_context: self.branch_context.clone(),
            scheduler: self.scheduler.clone(),
            trace_len: self.trace.events.len(),
            completed_work_len: self.completed_work.len(),
            completed: self.completed.clone(),
            owner_commit_profile: self.owner_commit_profile.clone(),
        }
    }

    fn parallel_wave_checkpoint(&self) -> EvalParallelWaveCheckpoint {
        EvalParallelWaveCheckpoint {
            ready_queue: self.scheduler.ready_queue.clone(),
            ready_tasks: self.scheduler.ready_tasks.clone(),
            in_flight_tasks: self.scheduler.in_flight_tasks.clone(),
            buffered_completions: self.scheduler.buffered_completions.clone(),
            active_wave: self.scheduler.active_wave,
            next_wave_id: self.scheduler.next_wave_id,
            next_lease_namespace: self.scheduler.next_lease_namespace,
            next_lease_sequence: self.scheduler.next_lease_sequence,
        }
    }

    fn restore_parallel_wave_checkpoint(&mut self, checkpoint: EvalParallelWaveCheckpoint) {
        self.scheduler.ready_queue = checkpoint.ready_queue;
        self.scheduler.ready_tasks = checkpoint.ready_tasks;
        self.scheduler.in_flight_tasks = checkpoint.in_flight_tasks;
        self.scheduler.buffered_completions = checkpoint.buffered_completions;
        self.scheduler.active_wave = checkpoint.active_wave;
        self.scheduler.next_wave_id = checkpoint.next_wave_id;
        self.scheduler.next_lease_namespace = checkpoint.next_lease_namespace;
        self.scheduler.next_lease_sequence = checkpoint.next_lease_sequence;
    }

    pub fn restore(&mut self, checkpoint: EvalSessionCheckpoint) {
        let next_lease_sequence = self
            .scheduler
            .next_lease_sequence
            .max(checkpoint.scheduler.next_lease_sequence);
        let next_wave_id = self
            .scheduler
            .next_wave_id
            .max(checkpoint.scheduler.next_wave_id);
        let next_lease_namespace = self
            .scheduler
            .next_lease_namespace
            .max(checkpoint.scheduler.next_lease_namespace);
        self.scheduler = checkpoint.scheduler;
        self.scheduler.next_wave_id = next_wave_id;
        self.scheduler.next_lease_sequence = next_lease_sequence;
        self.scheduler.next_lease_namespace = next_lease_namespace;
        self.branch_context = checkpoint.branch_context;
        self.trace.events.truncate(checkpoint.trace_len);
        self.completed_work.truncate(checkpoint.completed_work_len);
        self.completed = checkpoint.completed;
        self.owner_commit_profile = checkpoint.owner_commit_profile;
    }

    pub fn prepare_step(&mut self) -> Option<EvalPreparedWork<'runtime, Runtime>> {
        self.prepare_step_impl(true, false)
            .expect("leasing prepared work should not fail")
    }

    fn prepare_step_impl(
        &mut self,
        record_history: bool,
        drain_local_segment: bool,
    ) -> Result<Option<EvalPreparedWork<'runtime, Runtime>>, EvalError> {
        if self.completed.is_some() {
            return Ok(None);
        }
        let snapshot = record_history.then(|| self.checkpoint());
        let Some(lease) = self.scheduler.lease_ready_task()? else {
            return Ok(None);
        };
        if let Some(snapshot) = snapshot {
            self.history.push(snapshot);
        }
        Ok(Some(EvalPreparedWork {
            program: Arc::clone(&self.program),
            entry: Arc::clone(&self.entry),
            args: Arc::clone(&self.args),
            runtime: self.runtime,
            place: Arc::clone(&self.place),
            branch_context: lease.record.branch_context.clone(),
            wave_id: lease.wave_id,
            lease_sequence: lease.lease_sequence,
            base_generation: self.scheduler.base_generation(),
            scheduler: self.scheduler.prepared_snapshot(lease.namespace),
            record: lease.record,
            drain_local_segment,
            collect_trace: self.trace_enabled,
            collect_completed_work: self.completed_work_enabled,
            call_summary: self.call_summary_enabled(),
            call_profiling: self.call_profiling,
        }))
    }

    fn lease_parallel_wave(
        &mut self,
        max_workers: usize,
    ) -> Result<Vec<EvalPreparedWork<'runtime, Runtime>>, EvalError> {
        let mut prepared = Vec::new();
        while prepared.len() < max_workers {
            let Some(next) = self.prepare_step_impl(false, true)? else {
                break;
            };
            prepared.push(next);
        }
        Ok(prepared)
    }

    fn execute_parallel_wave(
        &mut self,
        worker_commands: &[mpsc::SyncSender<ParallelWorkerCommand<'runtime, Runtime>>],
        completion_rx: &mpsc::Receiver<ParallelWorkerMessage>,
        prepared: Vec<EvalPreparedWork<'runtime, Runtime>>,
    ) -> Result<(), EvalError> {
        let expected = prepared.len();
        for (command_tx, prepared) in worker_commands.iter().zip(prepared) {
            command_tx
                .send(ParallelWorkerCommand::Execute(prepared))
                .expect("parallel eval worker command channel closed unexpectedly");
        }

        let mut completions = Vec::with_capacity(expected);
        let mut first_error = None;
        let mut first_panic = None;
        for _ in 0..expected {
            match completion_rx.recv().expect(
                "parallel eval wave channel closed before all worker outcomes were reported",
            ) {
                ParallelWorkerMessage::Completion(Ok(completion)) => completions.push(completion),
                ParallelWorkerMessage::Completion(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                ParallelWorkerMessage::Panic(payload) => {
                    if first_panic.is_none() {
                        first_panic = Some(payload);
                    }
                }
            }
        }
        if let Some(payload) = first_panic {
            resume_unwind(payload);
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        completions.sort_by_key(EvalWorkCompletion::lease_sequence);
        let _step = self.apply_completion_batch_impl(completions, false)?;
        Ok(())
    }

    pub fn apply_completion(
        &mut self,
        completion: EvalWorkCompletion,
    ) -> Result<EvalSessionStep, EvalError> {
        self.apply_completion_impl(completion, true)
    }

    fn apply_completion_batch_impl(
        &mut self,
        completions: Vec<EvalWorkCompletion>,
        record_history: bool,
    ) -> Result<EvalSessionStep, EvalError> {
        let mut snapshot = record_history.then(|| self.checkpoint());
        let mut step = None;

        for completion in completions {
            match apply_completion_to_state(
                &mut self.scheduler,
                &mut self.trace,
                &mut self.completed_work,
                &mut self.completed,
                &mut self.owner_commit_profile,
                self.trace_enabled,
                self.completed_work_enabled,
                completion,
            ) {
                Ok(next) => step = Some(next),
                Err(error) => {
                    if let Some(snapshot) = snapshot.take() {
                        self.restore(snapshot);
                    }
                    return Err(error);
                }
            }
        }
        self.scheduler.clear_active_wave_if_idle();

        if let Some(snapshot) = snapshot {
            self.history.push(snapshot);
        }
        Ok(step.unwrap_or_else(|| step_without_ready_work(&self.scheduler, &self.completed)))
    }

    fn apply_completion_impl(
        &mut self,
        completion: EvalWorkCompletion,
        record_history: bool,
    ) -> Result<EvalSessionStep, EvalError> {
        let snapshot = record_history.then(|| self.checkpoint());
        let step = match apply_completion_to_state(
            &mut self.scheduler,
            &mut self.trace,
            &mut self.completed_work,
            &mut self.completed,
            &mut self.owner_commit_profile,
            self.trace_enabled,
            self.completed_work_enabled,
            completion,
        ) {
            Ok(step) => step,
            Err(error) => {
                if let Some(snapshot) = snapshot {
                    self.restore(snapshot);
                }
                return Err(error);
            }
        };
        self.scheduler.clear_active_wave_if_idle();

        if let Some(snapshot) = snapshot {
            self.history.push(snapshot);
        }
        Ok(step)
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
        records.extend(
            self.scheduler
                .in_flight_work()
                .into_iter()
                .map(|work| EvalWorkRecord {
                    work,
                    state: EvalWorkState::InFlight,
                }),
        );
        records.extend(
            self.scheduler
                .buffered_work()
                .into_iter()
                .map(|buffered| EvalWorkRecord {
                    work: buffered.work,
                    state: EvalWorkState::Buffered(buffered.outcome),
                }),
        );
        records.sort_by_key(|record| record.work.id);
        records
    }

    pub fn graph_snapshot(&self) -> EvalGraphSnapshot {
        let mut node_ids = self
            .scheduler
            .node_record_ids()
            .into_iter()
            .collect::<Vec<_>>();
        node_ids.extend(self.scheduler.node_state_ids());
        node_ids.extend(self.scheduler.base.keyed_nodes.keys());
        for edge in self.scheduler.base.dependency_edges.iter() {
            node_ids.push(edge.parent);
            node_ids.push(edge.child);
        }
        node_ids.sort();
        node_ids.dedup();

        let mut dependencies = BTreeMap::<EvalNodeId, Vec<EvalNodeId>>::new();
        let mut dependents = BTreeMap::<EvalNodeId, Vec<EvalNodeId>>::new();
        for edge in self.scheduler.base.dependency_edges.iter() {
            dependencies
                .entry(edge.parent)
                .or_default()
                .push(edge.child);
            dependents.entry(edge.child).or_default().push(edge.parent);
        }
        let mut keyed_nodes = self
            .scheduler
            .base
            .keyed_nodes
            .iter()
            .map(|(node_id, memo)| (node_id, memo.public()))
            .collect::<BTreeMap<_, _>>();

        EvalGraphSnapshot {
            branch_context: self.branch_context.clone(),
            nodes: node_ids
                .into_iter()
                .map(|node_id| EvalGraphNode {
                    node_id,
                    record: self.scheduler.node_record(node_id).cloned(),
                    keyed: keyed_nodes.remove(&node_id),
                    state: self.scheduler.node_state(node_id).cloned(),
                    dependencies: dependencies.remove(&node_id).unwrap_or_default(),
                    dependents: dependents.remove(&node_id).unwrap_or_default(),
                })
                .collect(),
            work: self.work_records(),
            active_wave: self.active_wave(),
            pending: self.pending_nodes(),
            completed: self.completed.clone(),
        }
    }
}

impl<'runtime, Runtime: RuntimeHooks> EvalPreparedWork<'runtime, Runtime> {
    pub fn branch_context(&self) -> &EvalBranchContext {
        &self.branch_context
    }

    pub fn work(&self) -> &EvalQueuedWork {
        &self.record.work
    }

    pub fn lease_sequence(&self) -> u64 {
        self.lease_sequence
    }

    pub fn base_generation(&self) -> u64 {
        self.base_generation
    }

    pub fn execute(self) -> Result<EvalWorkCompletion, EvalError> {
        let EvalPreparedWork {
            program,
            entry,
            args,
            runtime,
            place,
            branch_context,
            wave_id,
            lease_sequence,
            base_generation,
            scheduler,
            record,
            drain_local_segment,
            collect_trace,
            collect_completed_work,
            call_summary,
            call_profiling,
        } = self;
        #[cfg(test)]
        if let Some(error) = injected_worker_error(&branch_context, &record.work) {
            return Err(error);
        }
        let started = Instant::now();
        let completion_record = record.clone();
        let (delta, outcome, invocation) = execute_ready_task(
            program,
            &entry,
            &args,
            runtime,
            &place,
            scheduler,
            record,
            drain_local_segment,
            collect_trace,
            collect_completed_work,
            call_summary,
            call_profiling,
        )?;
        Ok(EvalWorkCompletion {
            branch_context,
            wave_id,
            lease_sequence,
            base_generation,
            record: completion_record.clone(),
            work: completion_record.work.clone(),
            outcome,
            delta,
            elapsed: started.elapsed(),
            invocation,
        })
    }
}

#[cfg(test)]
fn injected_worker_error(
    branch_context: &EvalBranchContext,
    work: &EvalQueuedWork,
) -> Option<EvalError> {
    (branch_context.key.as_ref() == "__test_worker_error__"
        && matches!(work.kind, EvalWorkKind::EvaluateExpr)
        && work.id.0 % 2 == 1)
        .then(|| EvalError::new(EvalErrorKind::Internal, "injected test worker error"))
}

#[cfg(test)]
impl<'runtime, Runtime: RuntimeHooks> EvalSession<'runtime, Runtime> {
    pub(crate) fn program_parse_count(&self) -> usize {
        self.program.parse_count()
    }
}

impl EvalWorkCompletion {
    pub fn branch_context(&self) -> &EvalBranchContext {
        &self.branch_context
    }

    pub fn wave_id(&self) -> EvalWaveId {
        self.wave_id
    }

    pub fn work(&self) -> &EvalQueuedWork {
        &self.work
    }

    pub fn outcome(&self) -> &EvalCompletedWorkOutcome {
        &self.outcome
    }

    pub fn lease_sequence(&self) -> u64 {
        self.lease_sequence
    }

    pub fn base_generation(&self) -> u64 {
        self.base_generation
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

fn apply_completion_to_state(
    scheduler: &mut EvalScheduler,
    trace: &mut EvalTrace,
    completed_work: &mut Vec<EvalCompletedWork>,
    completed: &mut Option<Result<Value, EvalError>>,
    owner_commit_profile: &mut EvalOwnerCommitProfile,
    collect_trace: bool,
    collect_completed_work: bool,
    completion: EvalWorkCompletion,
) -> Result<EvalSessionStep, EvalError> {
    owner_commit_profile.completions_seen += 1;
    scheduler.validate_in_flight_task(
        completion.work.id,
        completion.lease_sequence,
        &completion.work,
    )?;

    if scheduler.can_commit_lease_sequence(completion.lease_sequence) {
        let leased = scheduler.take_in_flight_task(
            completion.work.id,
            completion.lease_sequence,
            &completion.work,
        )?;
        if leased != completion.record {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "leased eval work record {:?} diverged before commit",
                    completion.work.id
                ),
            ));
        }
        let mut committed = commit_completion(
            scheduler,
            trace,
            completed_work,
            completed,
            owner_commit_profile,
            collect_trace,
            collect_completed_work,
            completion,
        )?;
        let mut step = committed.step;
        while !committed.replayed {
            let Some(buffered) = scheduler.take_next_committable_buffered_completion() else {
                break;
            };
            committed = commit_completion(
                scheduler,
                trace,
                completed_work,
                completed,
                owner_commit_profile,
                collect_trace,
                collect_completed_work,
                buffered,
            )?;
            step = committed.step;
        }
        Ok(step)
    } else {
        owner_commit_profile.completions_buffered += 1;
        scheduler.buffer_completion(completion)?;
        Ok(step_after_buffering(scheduler, completed))
    }
}

fn emit_new_trace_events(
    trace: &EvalTrace,
    emitted: &mut usize,
    sink: &mut impl EvalEventSink,
    scrub_work_ids: bool,
) {
    for event in &trace.events[*emitted..] {
        let mut event = event.clone();
        if scrub_work_ids {
            event.work_id = None;
        }
        sink.push(event);
    }
    *emitted = trace.events.len();
}

fn commit_completion(
    scheduler: &mut EvalScheduler,
    trace: &mut EvalTrace,
    completed_work: &mut Vec<EvalCompletedWork>,
    completed: &mut Option<Result<Value, EvalError>>,
    owner_commit_profile: &mut EvalOwnerCommitProfile,
    collect_trace: bool,
    collect_completed_work: bool,
    completion: EvalWorkCompletion,
) -> Result<CommitCompletionResult, EvalError> {
    owner_commit_profile.completions_committed += 1;
    if completion_targets_superseded_work(scheduler, &completion.work) {
        owner_commit_profile.completions_superseded += 1;
        if collect_completed_work {
            completed_work.push(EvalCompletedWork {
                work: completion.work,
                outcome: completion.outcome,
                base_generation: completion.base_generation,
                elapsed: completion.elapsed,
                invocation: completion.invocation,
            });
        }
        return Ok(CommitCompletionResult::progressed());
    }
    if completion.base_generation > scheduler.base_generation() {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            format!(
                "eval completion for {:?} observed future base generation {} > {}",
                completion.work.id,
                completion.base_generation,
                scheduler.base_generation(),
            ),
        ));
    }
    if scheduler.completion_reads_stale_base(&completion)? {
        owner_commit_profile.completions_replayed += 1;
        scheduler.requeue_ready_record_front(completion.record)?;
        return Ok(CommitCompletionResult::replayed());
    }
    owner_commit_profile.delta_node_records += completion.delta.scheduler.node_records.len();
    owner_commit_profile.delta_keyed_nodes += completion.delta.scheduler.keyed_nodes.len();
    owner_commit_profile.delta_node_states += completion.delta.scheduler.node_states.len();
    owner_commit_profile.delta_dependency_edges +=
        completion.delta.scheduler.dependency_edges.len();
    owner_commit_profile.delta_queued += completion.delta.scheduler.queued.len();
    owner_commit_profile.delta_pending_entries += completion.delta.scheduler.pending_entries.len();
    owner_commit_profile.delta_retained_exprs += completion.delta.scheduler.retained_exprs.len();
    owner_commit_profile.delta_learned_no_goods +=
        completion.delta.scheduler.learned_no_goods.len();
    owner_commit_profile.delta_learned_search_no_goods +=
        completion.delta.scheduler.learned_search_no_goods.len();
    owner_commit_profile.delta_trace_events += completion.delta.trace_events.len();
    owner_commit_profile.delta_local_completed_work += completion.delta.local_completed_work.len();
    owner_commit_profile.completed_invalidations +=
        usize::from(completion.delta.completed_invalidated);
    if collect_trace {
        trace.events.extend(completion.delta.trace_events);
    }
    let completed_invalidated = completion.delta.completed_invalidated;
    if collect_completed_work {
        completed_work.extend(completion.delta.local_completed_work);
    }
    scheduler.apply_delta(completion.delta.scheduler)?;
    if completed_invalidated {
        *completed = None;
    }
    let keyed_stale = matches!(
        completion.outcome,
        EvalCompletedWorkOutcome::Ready(_) | EvalCompletedWorkOutcome::Poison(_)
    ) && scheduler.finalize_keyed_completion(completion.work.node_id)?;
    if collect_completed_work {
        completed_work.push(EvalCompletedWork {
            work: completion.work.clone(),
            outcome: completion.outcome.clone(),
            base_generation: completion.base_generation,
            elapsed: completion.elapsed,
            invocation: completion.invocation.clone(),
        });
    }
    if keyed_stale {
        return Ok(CommitCompletionResult::progressed());
    }
    match completion.outcome {
        EvalCompletedWorkOutcome::Progressed | EvalCompletedWorkOutcome::BlockedOnNode(_) => {
            Ok(CommitCompletionResult::progressed())
        }
        EvalCompletedWorkOutcome::Pending(pending) => Ok(CommitCompletionResult {
            step: EvalSessionStep::Pending(pending),
            replayed: false,
        }),
        EvalCompletedWorkOutcome::Ready(value) => {
            if scheduler.resolve_child_dependency(
                completion.work.node_id,
                Ok(value.clone()),
                owner_commit_profile,
            )? {
                Ok(CommitCompletionResult::progressed())
            } else if !scheduler.queued_work().is_empty() || scheduler.has_waiting_work() {
                Ok(CommitCompletionResult::progressed())
            } else {
                scheduler.record_node_state(
                    ENTRY_WORK_NODE_ID,
                    EvalNodeState::Ready(Arc::new(value.clone())),
                );
                *completed = Some(Ok(value.clone()));
                Ok(CommitCompletionResult {
                    step: EvalSessionStep::Ready(value),
                    replayed: false,
                })
            }
        }
        EvalCompletedWorkOutcome::Poison(error) => {
            if scheduler.resolve_child_dependency(
                completion.work.node_id,
                Err(error.clone()),
                owner_commit_profile,
            )? {
                Ok(CommitCompletionResult::progressed())
            } else if !scheduler.queued_work().is_empty() || scheduler.has_waiting_work() {
                Ok(CommitCompletionResult::progressed())
            } else {
                scheduler
                    .record_node_state(ENTRY_WORK_NODE_ID, EvalNodeState::Poison(error.clone()));
                *completed = Some(Err(error.clone()));
                Ok(CommitCompletionResult {
                    step: EvalSessionStep::Poison(error),
                    replayed: false,
                })
            }
        }
    }
}

struct CommitCompletionResult {
    step: EvalSessionStep,
    replayed: bool,
}

impl CommitCompletionResult {
    fn progressed() -> Self {
        Self {
            step: EvalSessionStep::Progressed,
            replayed: false,
        }
    }

    fn replayed() -> Self {
        Self {
            step: EvalSessionStep::Progressed,
            replayed: true,
        }
    }
}

fn profile_label_for_record(record: &EvalNodeRecord) -> Arc<str> {
    match &record.kind {
        EvalNodeKind::SyntheticRoot => Arc::from("synthetic-root"),
        EvalNodeKind::ArgumentBinding { label } => Arc::from(format!("arg:{label}")),
        EvalNodeKind::Expr { kind } => Arc::from(format!("expr:{kind:?}")),
        EvalNodeKind::Keyed { resolver, key: _ } => Arc::from(format!("keyed:{resolver}")),
        EvalNodeKind::PendingToken { description } => Arc::from(format!("pending:{description}")),
    }
}

fn causal_profile_label_for_completed(
    completed: &EvalCompletedWork,
    record: Option<&EvalNodeRecord>,
) -> Arc<str> {
    if let Some(invocation) = &completed.invocation
        && completed.work.node_id == invocation.root_node_id
    {
        return Arc::from(format!("call:{}", invocation.callee));
    }
    record
        .map(profile_label_for_record)
        .unwrap_or_else(|| Arc::from("<unknown>"))
}

fn causal_profile_source_for_completed(
    completed: &EvalCompletedWork,
    record: Option<&EvalNodeRecord>,
) -> Option<SourceSpan> {
    if let Some(invocation) = &completed.invocation
        && completed.work.node_id == invocation.root_node_id
    {
        return Some(invocation.source.clone());
    }
    record.and_then(|record| record.source.clone())
}

fn format_profile_summary_report(profile: &EvalSessionProfileSummary, limit: usize) -> String {
    let mut lines = vec![format!(
        "eval2 profile total={} summary_entries={}",
        format_profile_duration(profile.total_time),
        profile.entries.len()
    )];

    for entry in profile.entries.iter().take(limit) {
        let avg = if entry.runs == 0 {
            Duration::default()
        } else {
            entry.total_time / (entry.runs as u32)
        };
        let source = entry
            .source
            .as_ref()
            .map(|source| format!("{source:?}"))
            .unwrap_or_else(|| "<unknown>".to_string());
        lines.push(format!(
            "  total={} avg={} max={} nodes={} runs={} work={:?} label={} source={}",
            format_profile_duration(entry.total_time),
            format_profile_duration(avg),
            format_profile_duration(entry.max_time),
            entry.node_count,
            entry.runs,
            entry.work_kind,
            entry.label,
            source,
        ));
    }

    lines.join("\n")
}

fn format_causal_profile_report(
    session: &EvalSession<'_, impl RuntimeHooks>,
    profile: &EvalSessionCausalProfile,
    limit: usize,
) -> String {
    let top_entries = profile.entries.iter().take(limit).collect::<Vec<_>>();
    let top_stacks = causal_profile_top_stacks(session, &top_entries);
    let mut lines = vec![format!(
        "eval2 causal profile total={} entries={}",
        format_profile_duration(profile.total_time),
        profile.entries.len(),
    )];

    for (index, entry) in top_entries.iter().enumerate() {
        let avg = if entry.runs == 0 {
            Duration::default()
        } else {
            entry.total_time / (entry.runs as u32)
        };
        let source = entry
            .source
            .as_ref()
            .map(|span| format!("{span:?}"))
            .unwrap_or_else(|| "<unknown>".to_string());
        lines.push(format!(
            "  total={} avg={} max={} runs={} work={:?} cause={:?} label={} source={}",
            format_profile_duration(entry.total_time),
            format_profile_duration(avg),
            format_profile_duration(entry.max_time),
            entry.runs,
            entry.work_kind,
            entry.cause_kind,
            entry.label,
            source,
        ));
        for stack in &top_stacks[index] {
            let avg = if stack.runs == 0 {
                Duration::default()
            } else {
                stack.total_time / (stack.runs as u32)
            };
            lines.push(format!(
                "    stack total={} avg={} max={} runs={} path={}",
                format_profile_duration(stack.total_time),
                format_profile_duration(avg),
                format_profile_duration(stack.max_time),
                stack.runs,
                format_depend_stack(session, &stack.depend_stack),
            ));
        }
    }

    lines.join("\n")
}

fn format_owner_commit_profile_report(profile: &EvalOwnerCommitProfile) -> String {
    let committed = profile.completions_committed.max(1);
    let avg_delta_items = profile.total_delta_items() / committed;
    let mut lines = vec![format!(
        "eval2 owner commit profile seen={} committed={} buffered={} replayed={} superseded={}",
        profile.completions_seen,
        profile.completions_committed,
        profile.completions_buffered,
        profile.completions_replayed,
        profile.completions_superseded,
    )];
    lines.push(format!(
        "  pending_resolutions={} waiter_wakeups={} resume_tasks_enqueued={} completed_invalidations={}",
        profile.pending_resolutions,
        profile.waiter_wakeups,
        profile.resume_tasks_enqueued,
        profile.completed_invalidations,
    ));
    lines.push(format!(
        "  delta_items={} avg_per_committed={} node_records={} keyed_nodes={} node_states={} dependency_edges={}",
        profile.total_delta_items(),
        avg_delta_items,
        profile.delta_node_records,
        profile.delta_keyed_nodes,
        profile.delta_node_states,
        profile.delta_dependency_edges,
    ));
    lines.push(format!(
        "  queued={} pending_entries={} retained_exprs={} learned_no_goods={} learned_search_no_goods={} trace_events={} local_completed_work={}",
        profile.delta_queued,
        profile.delta_pending_entries,
        profile.delta_retained_exprs,
        profile.delta_learned_no_goods,
        profile.delta_learned_search_no_goods,
        profile.delta_trace_events,
        profile.delta_local_completed_work,
    ));
    lines.join("\n")
}

fn causal_profile_top_stacks(
    session: &EvalSession<'_, impl RuntimeHooks>,
    entries: &[&EvalCausalProfileEntry],
) -> Vec<Vec<EvalCausalProfileStackEntry>> {
    let mut entry_by_key = HashMap::<
        (
            EvalWorkKind,
            EvalWorkCauseKind,
            Arc<str>,
            Option<SourceSpan>,
        ),
        usize,
    >::new();
    for (index, entry) in entries.iter().enumerate() {
        entry_by_key.insert(
            (
                entry.work_kind,
                entry.cause_kind,
                entry.label.clone(),
                entry.source.clone(),
            ),
            index,
        );
    }

    let mut stacks =
        vec![HashMap::<Arc<[EvalNodeId]>, EvalCausalProfileStackEntry>::new(); entries.len()];
    for completed in &session.completed_work {
        if completed.work.kind == EvalWorkKind::ResumePending {
            continue;
        }
        if let Some(invocation) = &completed.invocation
            && completed.work.node_id != invocation.root_node_id
        {
            continue;
        }

        let record = session.node_record(completed.work.node_id);
        let label = causal_profile_label_for_completed(completed, record);
        let source = causal_profile_source_for_completed(completed, record);
        let key = (
            completed.work.kind,
            completed.work.cause.kind,
            label,
            source,
        );
        let Some(&entry_index) = entry_by_key.get(&key) else {
            continue;
        };

        let stack = completed.work.cause.depend_stack.clone();
        let stack_entry = stacks[entry_index].entry(stack.clone()).or_insert_with(|| {
            EvalCausalProfileStackEntry {
                depend_stack: stack,
                runs: 0,
                total_time: Duration::default(),
                max_time: Duration::default(),
            }
        });
        stack_entry.runs += 1;
        stack_entry.total_time += completed.elapsed;
        stack_entry.max_time = stack_entry.max_time.max(completed.elapsed);
    }

    stacks
        .into_iter()
        .map(|stacks| {
            let mut stacks = stacks.into_values().collect::<Vec<_>>();
            stacks.sort_by(|left, right| {
                right
                    .total_time
                    .cmp(&left.total_time)
                    .then_with(|| right.max_time.cmp(&left.max_time))
                    .then_with(|| right.runs.cmp(&left.runs))
            });
            stacks.truncate(3);
            stacks
        })
        .collect()
}

fn format_depend_stack(
    session: &EvalSession<'_, impl RuntimeHooks>,
    stack: &[EvalNodeId],
) -> String {
    if stack.is_empty() {
        return "<root>".to_string();
    }
    stack
        .iter()
        .map(|node_id| format_depend_stack_frame(session, *node_id))
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn format_depend_stack_frame(
    session: &EvalSession<'_, impl RuntimeHooks>,
    node_id: EvalNodeId,
) -> String {
    let Some(record) = session.node_record(node_id) else {
        return format!("{node_id:?}@<unknown>");
    };
    let source = record
        .source
        .as_ref()
        .map(|span| format!("{span:?}"))
        .unwrap_or_else(|| "<unknown>".to_string());
    format!("{}@{}", profile_label_for_record(record), source)
}

fn compute_call_run_total_time(
    index: usize,
    runs: &[EvalCallProfileRun],
    children_by_parent: &BTreeMap<Option<(Option<EvalWorkId>, EvalNodeId)>, Vec<usize>>,
    cache: &mut BTreeMap<usize, Duration>,
) -> Duration {
    if let Some(cached) = cache.get(&index) {
        return *cached;
    }
    let run = &runs[index];
    let key = Some((run.invocation.root_work_id, run.invocation.root_node_id));
    let mut total = run.self_time;
    if let Some(children) = children_by_parent.get(&key) {
        for child in children {
            total += compute_call_run_total_time(*child, runs, children_by_parent, cache);
        }
    }
    cache.insert(index, total);
    total
}

fn summarize_call_runs(runs: &[EvalCallProfileRun]) -> Vec<EvalCallProfileSummaryEntry> {
    let mut entries = BTreeMap::<Arc<str>, EvalCallProfileSummaryEntry>::new();
    let mut inputs = BTreeMap::<Arc<str>, BTreeSet<u64>>::new();

    for run in runs {
        let key = run.invocation.callee.clone();
        let entry = entries
            .entry(key.clone())
            .or_insert_with(|| EvalCallProfileSummaryEntry {
                callee: run.invocation.callee.clone(),
                source: Some(run.invocation.source.clone()),
                runs: 0,
                distinct_input_count: 0,
                repeated_input_count: 0,
                self_time: Duration::default(),
                total_time: Duration::default(),
                max_total_time: Duration::default(),
            });
        entry.runs += 1;
        entry.self_time += run.self_time;
        entry.total_time += run.total_time;
        entry.max_total_time = entry.max_total_time.max(run.total_time);
        if entry.source.as_ref() != Some(&run.invocation.source) {
            entry.source = None;
        }
        inputs
            .entry(key)
            .or_default()
            .insert(run.invocation.args_fingerprint.clone());
    }

    let mut entries = entries.into_values().collect::<Vec<_>>();
    for entry in &mut entries {
        let key = entry.callee.clone();
        let distinct_input_count = inputs.get(&key).map_or(0, BTreeSet::len);
        entry.distinct_input_count = distinct_input_count;
        entry.repeated_input_count = entry.runs.saturating_sub(distinct_input_count);
    }
    entries
}

fn build_call_tree_entries(
    indices: &[usize],
    runs: &[EvalCallProfileRun],
    children_by_parent: &BTreeMap<Option<(Option<EvalWorkId>, EvalNodeId)>, Vec<usize>>,
) -> Vec<EvalCallTreeEntry> {
    let mut grouped = BTreeMap::<Arc<str>, Vec<usize>>::new();
    for index in indices {
        let run = &runs[*index];
        grouped
            .entry(run.invocation.callee.clone())
            .or_default()
            .push(*index);
    }

    let mut entries = grouped
        .into_iter()
        .map(|(_, group)| {
            let first = &runs[group[0]];
            let mut input_fingerprints = BTreeSet::<u64>::new();
            let mut child_indices = Vec::new();
            let mut self_time = Duration::default();
            let mut total_time = Duration::default();
            let mut max_total_time = Duration::default();
            let mut source = Some(first.invocation.source.clone());
            for index in &group {
                let run = &runs[*index];
                self_time += run.self_time;
                total_time += run.total_time;
                max_total_time = max_total_time.max(run.total_time);
                input_fingerprints.insert(run.invocation.args_fingerprint.clone());
                if source.as_ref() != Some(&run.invocation.source) {
                    source = None;
                }
                let key = Some((run.invocation.root_work_id, run.invocation.root_node_id));
                if let Some(children) = children_by_parent.get(&key) {
                    child_indices.extend(children.iter().copied());
                }
            }
            let mut children = build_call_tree_entries(&child_indices, runs, children_by_parent);
            children.sort_by(|left, right| {
                right
                    .total_time
                    .cmp(&left.total_time)
                    .then_with(|| right.self_time.cmp(&left.self_time))
                    .then_with(|| left.callee.cmp(&right.callee))
            });
            EvalCallTreeEntry {
                callee: first.invocation.callee.clone(),
                source,
                runs: group.len(),
                distinct_input_count: input_fingerprints.len(),
                repeated_input_count: group.len().saturating_sub(input_fingerprints.len()),
                self_time,
                total_time,
                max_total_time,
                children,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .total_time
            .cmp(&left.total_time)
            .then_with(|| right.self_time.cmp(&left.self_time))
            .then_with(|| left.callee.cmp(&right.callee))
    });
    entries
}

fn format_call_profile_report(profile: &EvalSessionCallProfile, limit: usize) -> String {
    let mut lines = vec![format!(
        "eval2 call profile attributed={} runs={} summary_entries={} roots={}",
        format_profile_duration(profile.attributed_time),
        profile.runs.len(),
        profile.summary.len(),
        profile.roots.len(),
    )];

    if !profile.summary.is_empty() {
        lines.push("hot callsites:".to_string());
        for entry in profile.summary.iter().take(limit) {
            lines.push(format!(
                "  total={} self={} max={} runs={} inputs={}/{} callee={} source={:?}",
                format_profile_duration(entry.total_time),
                format_profile_duration(entry.self_time),
                format_profile_duration(entry.max_total_time),
                entry.runs,
                entry.distinct_input_count,
                entry.runs,
                entry.callee,
                entry.source,
            ));
        }
    }

    if !profile.roots.is_empty() {
        lines.push("call tree:".to_string());
        format_call_tree_entries(&mut lines, &profile.roots, 1, limit);
    }

    lines.join("\n")
}

fn format_call_summary_report(summary: &EvalSessionCallSummary, limit: usize) -> String {
    let mut lines = vec![format!(
        "eval2 call summary attributed={} runs={} summary_entries={}",
        format_profile_duration(summary.attributed_time),
        summary.runs.len(),
        summary.summary.len(),
    )];

    if !summary.summary.is_empty() {
        lines.push("hot callsites:".to_string());
        for entry in summary.summary.iter().take(limit) {
            lines.push(format!(
                "  total={} self={} max={} runs={} inputs={}/{} callee={} source={:?}",
                format_profile_duration(entry.total_time),
                format_profile_duration(entry.self_time),
                format_profile_duration(entry.max_total_time),
                entry.runs,
                entry.distinct_input_count,
                entry.runs,
                entry.callee,
                entry.source,
            ));
        }
    }

    lines.join("\n")
}

fn format_call_tree_entries(
    lines: &mut Vec<String>,
    entries: &[EvalCallTreeEntry],
    depth: usize,
    limit: usize,
) {
    let indent = "  ".repeat(depth);
    for entry in entries.iter().take(limit) {
        lines.push(format!(
            "{indent}total={} self={} runs={} inputs={}/{} callee={} source={:?}",
            format_profile_duration(entry.total_time),
            format_profile_duration(entry.self_time),
            entry.runs,
            entry.distinct_input_count,
            entry.runs,
            entry.callee,
            entry.source,
        ));
        if !entry.children.is_empty() {
            format_call_tree_entries(lines, &entry.children, depth + 1, limit);
        }
    }
}

fn format_profile_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    if nanos >= 1_000_000_000 {
        format!("{:.3}s", duration.as_secs_f64())
    } else if nanos >= 1_000_000 {
        format!("{:.3}ms", nanos as f64 / 1_000_000.0)
    } else if nanos >= 1_000 {
        format!("{:.3}us", nanos as f64 / 1_000.0)
    } else {
        format!("{nanos}ns")
    }
}

fn step_after_buffering(
    scheduler: &EvalScheduler,
    completed: &Option<Result<Value, EvalError>>,
) -> EvalSessionStep {
    match completed {
        Some(Ok(value)) => EvalSessionStep::Ready(value.clone()),
        Some(Err(error)) => EvalSessionStep::Poison(error.clone()),
        None if !scheduler.queued_work().is_empty() => EvalSessionStep::Progressed,
        None => step_without_ready_work(scheduler, completed),
    }
}

fn completion_targets_superseded_work(scheduler: &EvalScheduler, work: &EvalQueuedWork) -> bool {
    matches!(
        scheduler.node_state(work.node_id),
        Some(EvalNodeState::Runnable {
            work: current_work,
            ..
        }) if *current_work != work.id
    )
}

fn completed_resolution(completed: &Option<Result<Value, EvalError>>) -> Option<Resolution> {
    match completed {
        Some(Ok(value)) => Some(Resolution::Ready(value.clone())),
        Some(Err(error)) => Some(Resolution::Poison(error.clone())),
        None => None,
    }
}

fn step_without_ready_work(
    scheduler: &EvalScheduler,
    completed: &Option<Result<Value, EvalError>>,
) -> EvalSessionStep {
    if let Some(resolution) = completed_resolution(completed) {
        return match resolution {
            Resolution::Ready(value) => EvalSessionStep::Ready(value),
            Resolution::Poison(error) => EvalSessionStep::Poison(error),
            Resolution::Pending(_) | Resolution::Waiting => unreachable!(),
        };
    }
    match scheduler.pending_nodes().into_iter().next() {
        Some(pending) => EvalSessionStep::Pending(pending),
        None if scheduler.has_waiting_work() => EvalSessionStep::WaitingOnInFlight,
        None => EvalSessionStep::Idle,
    }
}

fn resolution_without_ready_work(
    scheduler: &EvalScheduler,
    completed: &Option<Result<Value, EvalError>>,
) -> Resolution {
    if let Some(resolution) = completed_resolution(completed) {
        return resolution;
    }
    match scheduler.pending_nodes().into_iter().next() {
        Some(pending) => Resolution::Pending(pending),
        None if scheduler.has_waiting_work() => Resolution::Waiting,
        None => Resolution::Poison(EvalError::new(
            EvalErrorKind::Internal,
            "eval session has no ready work and no pending nodes",
        )),
    }
}

#[cfg(test)]
impl EvalWorkCompletion {
    pub(crate) fn inject_duplicate_queued_work(&mut self, record: EvalQueuedWork) {
        self.delta.scheduler.queued.push(ReadyTaskRecord {
            work: record.clone(),
            task: ReadyTask::StartEntry,
            branch_context: EvalBranchContext::default(),
        });
    }

    pub(crate) fn inject_node_state_read_for_validation(&mut self, node_id: EvalNodeId) {
        self.delta.reads.node_states.insert(node_id, None);
    }
}

#[cfg(test)]
impl<'runtime, Runtime: RuntimeHooks> EvalSession<'runtime, Runtime> {
    pub(crate) fn first_queued_work_for_testing(&self) -> EvalQueuedWork {
        self.scheduler
            .queued_work()
            .into_iter()
            .next()
            .expect("session should have queued work for duplicate-merge testing")
    }
}
