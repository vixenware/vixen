use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::exec::BoxFuture;
use crate::node::NodeId;
use crate::poison::PoisonError;
use crate::value::Value;
use crate::worker::{DEMAND_STACK, Engine};

pub(crate) struct Scheduler {
    engine: Arc<Engine>,
    initial_stack: Vec<NodeId>,
    deps: HashMap<NodeId, HashSet<NodeId>>,
    rdeps: HashMap<NodeId, Vec<NodeId>>,
    ready: VecDeque<NodeId>,
    ready_set: HashSet<NodeId>,
    demanded: HashSet<NodeId>,
    completed: HashSet<NodeId>,
    demand_paths: HashMap<NodeId, Vec<NodeId>>,
}

impl Scheduler {
    pub(crate) fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            initial_stack: Vec::new(),
            deps: HashMap::new(),
            rdeps: HashMap::new(),
            ready: VecDeque::new(),
            ready_set: HashSet::new(),
            demanded: HashSet::new(),
            completed: HashSet::new(),
            demand_paths: HashMap::new(),
        }
    }

    pub(crate) fn with_initial_stack(engine: Arc<Engine>, initial_stack: Vec<NodeId>) -> Self {
        Self {
            engine,
            initial_stack,
            deps: HashMap::new(),
            rdeps: HashMap::new(),
            ready: VecDeque::new(),
            ready_set: HashSet::new(),
            demanded: HashSet::new(),
            completed: HashSet::new(),
            demand_paths: HashMap::new(),
        }
    }

    pub(crate) async fn resolve_value(mut self, mut value: Value) -> Result<Value, PoisonError> {
        loop {
            let inspected = self.engine.inspect_value(value).await?;
            if inspected.pending.is_empty() {
                if inspected.errors.is_empty() {
                    return Ok(inspected.value);
                }
                let stack = self.prefixed_stack(&[]);
                return Err(self.engine.aggregate_poison_errors_for_stack(
                    "forcing dependencies",
                    &stack,
                    inspected.errors,
                ));
            }

            let pending = inspected.pending.iter().copied().collect::<Vec<_>>();
            for node_id in &pending {
                let mut stack = self.demand_paths.get(node_id).cloned().unwrap_or_default();
                self.demand_node(*node_id, &mut stack).await?;
            }

            if self.ready.is_empty() {
                if self.advance_external_progress().await? {
                    value = inspected.value;
                    continue;
                }
                self.recompute_incomplete_demanded().await?;
                if !self.ready.is_empty() {
                    self.run_ready_nodes().await?;
                    value = inspected.value;
                    continue;
                }
                let mut unresolved = Vec::new();
                for node_id in inspected.pending.iter().copied() {
                    if !self.refresh_completion(node_id)? {
                        unresolved.push(node_id);
                    }
                }
                if !unresolved.is_empty() {
                    let stack = self.prefixed_stack(&[]);
                    return Err(self.engine.stalled_error(&stack, &unresolved));
                }
            }

            self.run_ready_nodes().await?;
            value = inspected.value;
        }
    }

    pub(crate) async fn resolve_node_shallow(
        mut self,
        node_id: NodeId,
    ) -> Result<Value, PoisonError> {
        let mut stack = Vec::new();
        self.demand_node(node_id, &mut stack).await?;
        while !self.refresh_completion(node_id)? {
            if self.ready.is_empty() {
                if self.advance_external_progress().await? {
                    continue;
                }
                if self.refresh_completion(node_id)? {
                    continue;
                }
                self.recompute_incomplete_demanded().await?;
                if self.refresh_completion(node_id)? {
                    continue;
                }
                if !self.ready.is_empty() {
                    continue;
                }
                let stack = self.prefixed_stack(&[]);
                return Err(self.engine.stalled_error(&stack, &[node_id]));
            }
            self.run_ready_nodes().await?;
        }

        match self.engine.node_state(node_id)? {
            crate::worker::SchedulerNodeState::Resolved(value) => Ok(value),
            crate::worker::SchedulerNodeState::Poisoned(error) => Err(error),
            crate::worker::SchedulerNodeState::Pending
            | crate::worker::SchedulerNodeState::Evaluating => {
                let stack = self.prefixed_stack(&[]);
                Err(self.engine.stalled_error(&stack, &[node_id]))
            }
        }
    }

    async fn demand_node(
        &mut self,
        node_id: NodeId,
        stack: &mut Vec<NodeId>,
    ) -> Result<(), PoisonError> {
        if self.refresh_completion(node_id)? || self.demanded.contains(&node_id) {
            return Ok(());
        }
        if self.initial_stack.contains(&node_id) || stack.contains(&node_id) {
            let stack = self.prefixed_stack(stack);
            return Err(self.engine.circular_dependency_error(&stack, node_id));
        }
        if self.initial_stack.len() + stack.len() + 1 > self.engine.max_demand_depth_limit() {
            let stack = self.prefixed_stack(stack);
            return Err(self.engine.demand_depth_exceeded_error(&stack, node_id));
        }

        self.demand_paths
            .entry(node_id)
            .or_insert_with(|| stack.clone());
        stack.push(node_id);
        let mut unresolved = HashSet::new();
        for dep in self.engine.node_deps(node_id).await? {
            Box::pin(self.demand_node(dep, stack)).await?;
            if !self.refresh_completion(dep)? {
                unresolved.insert(dep);
                self.add_reverse_dep(dep, node_id);
            }
        }
        let popped = stack.pop();
        debug_assert_eq!(popped, Some(node_id));

        self.demanded.insert(node_id);
        if unresolved.is_empty() {
            self.enqueue_ready(node_id)?;
        } else {
            self.deps.insert(node_id, unresolved);
        }

        Ok(())
    }

    fn run_ready_nodes(&mut self) -> BoxFuture<'_, Result<(), PoisonError>> {
        Box::pin(async move {
            while !self.ready.is_empty() {
                let mut batch = Vec::new();
                while let Some(node_id) = self.ready.pop_front() {
                    self.ready_set.remove(&node_id);
                    if !self.refresh_completion(node_id)? {
                        batch.push(node_id);
                    }
                }
                if batch.is_empty() {
                    continue;
                }

                let results = if batch.len() == 1 {
                    let node_id = batch[0];
                    let chain = self.demand_paths.get(&node_id).cloned().unwrap_or_default();
                    let result = DEMAND_STACK
                        .scope(RefCell::new(Vec::new()), async {
                            self.engine
                                .execute_pending_node_with_chain(node_id, &chain)
                                .await
                        })
                        .await;
                    vec![(node_id, result)]
                } else {
                    let mut join_set = tokio::task::JoinSet::new();
                    let node_ids: Vec<_> = batch.iter().copied().collect();
                    for &node_id in &node_ids {
                        let chain = self.demand_paths.get(&node_id).cloned().unwrap_or_default();
                        let engine = Arc::clone(&self.engine);
                        join_set.spawn(async move {
                            let result = DEMAND_STACK
                                .scope(RefCell::new(Vec::new()), async {
                                    engine
                                        .execute_pending_node_with_chain(node_id, &chain)
                                        .await
                                })
                                .await;
                            (node_id, result)
                        });
                    }
                    let mut results = Vec::with_capacity(join_set.len());
                    while let Some(join_result) = join_set.join_next().await {
                        match join_result {
                            Ok(result) => results.push(result),
                            Err(join_error) => {
                                if join_error.is_panic() {
                                    std::panic::resume_unwind(join_error.into_panic());
                                }
                                let stack = self.prefixed_stack(&[]);
                                return Err(self.engine.stalled_error(&stack, &node_ids));
                            }
                        }
                    }
                    results
                };

                for (node_id, result) in results {
                    self.finish_completed_node(node_id, result).await?;
                }
            }

            Ok(())
        })
    }

    fn enqueue_ready(&mut self, node_id: NodeId) -> Result<(), PoisonError> {
        if self.refresh_completion(node_id)? {
            return Ok(());
        }
        if self.ready_set.insert(node_id) {
            self.ready.push_back(node_id);
        }
        Ok(())
    }

    fn refresh_completion(&mut self, node_id: NodeId) -> Result<bool, PoisonError> {
        match self.engine.node_state(node_id)? {
            crate::worker::SchedulerNodeState::Pending
            | crate::worker::SchedulerNodeState::Evaluating => Ok(false),
            crate::worker::SchedulerNodeState::Resolved(_)
            | crate::worker::SchedulerNodeState::Poisoned(_) => {
                self.completed.insert(node_id);
                Ok(true)
            }
        }
    }

    async fn recompute_node(&mut self, node_id: NodeId) -> Result<(), PoisonError> {
        if self.refresh_completion(node_id)? {
            self.deps.remove(&node_id);
            return Ok(());
        }

        let mut path = self.demand_paths.get(&node_id).cloned().unwrap_or_default();
        if self.initial_stack.len() + path.len() + 1 > self.engine.max_demand_depth_limit() {
            let stack = self.prefixed_stack(&path);
            return Err(self.engine.demand_depth_exceeded_error(&stack, node_id));
        }

        path.push(node_id);
        let mut unresolved = HashSet::new();
        for dep in self.engine.node_deps(node_id).await? {
            self.demand_node(dep, &mut path).await?;
            if !self.refresh_completion(dep)? {
                unresolved.insert(dep);
                self.add_reverse_dep(dep, node_id);
            }
        }
        let popped = path.pop();
        debug_assert_eq!(popped, Some(node_id));

        if unresolved.is_empty() {
            self.deps.remove(&node_id);
            self.enqueue_ready(node_id)?;
        } else {
            self.deps.insert(node_id, unresolved);
        }

        Ok(())
    }

    async fn recompute_incomplete_demanded(&mut self) -> Result<(), PoisonError> {
        for node_id in self.demanded.iter().copied().collect::<Vec<_>>() {
            if self.completed.contains(&node_id) {
                continue;
            }
            self.recompute_node(node_id).await?;
        }
        Ok(())
    }

    fn add_reverse_dep(&mut self, dep: NodeId, waiter: NodeId) {
        let waiters = self.rdeps.entry(dep).or_default();
        if !waiters.contains(&waiter) {
            waiters.push(waiter);
        }
    }

    async fn advance_external_progress(&mut self) -> Result<bool, PoisonError> {
        for node_id in self.demanded.iter().copied().collect::<Vec<_>>() {
            if self.completed.contains(&node_id) {
                continue;
            }
            match self.engine.node_state(node_id)? {
                crate::worker::SchedulerNodeState::Resolved(value) => {
                    self.finish_completed_node(node_id, Ok(value)).await?;
                    return Ok(true);
                }
                crate::worker::SchedulerNodeState::Poisoned(error) => {
                    self.finish_completed_node(node_id, Err(error)).await?;
                    return Ok(true);
                }
                crate::worker::SchedulerNodeState::Evaluating => {
                    self.engine.wait_for_node_completion(node_id).await?;
                    let result = match self.engine.node_state(node_id)? {
                        crate::worker::SchedulerNodeState::Resolved(value) => Ok(value),
                        crate::worker::SchedulerNodeState::Poisoned(error) => Err(error),
                        crate::worker::SchedulerNodeState::Pending
                        | crate::worker::SchedulerNodeState::Evaluating => {
                            return Err(self.engine.internal_error(&format!(
                                "node {} remained in-flight after waiting",
                                node_id.0
                            )));
                        }
                    };
                    self.finish_completed_node(node_id, result).await?;
                    return Ok(true);
                }
                crate::worker::SchedulerNodeState::Pending => {}
            }
        }

        Ok(false)
    }

    async fn finish_completed_node(
        &mut self,
        node_id: NodeId,
        result: Result<Value, PoisonError>,
    ) -> Result<(), PoisonError> {
        if let Ok(value) = &result {
            let inspected = self.engine.inspect_value(value.clone()).await?;
            let mut path = self.demand_paths.get(&node_id).cloned().unwrap_or_default();
            path.push(node_id);
            for pending_id in inspected.pending {
                self.demand_paths
                    .entry(pending_id)
                    .or_insert_with(|| path.clone());
            }
        }
        self.completed.insert(node_id);
        if let Some(waiters) = self.rdeps.remove(&node_id) {
            for waiter in waiters {
                self.recompute_node(waiter).await?;
            }
        }
        Ok(())
    }

    fn prefixed_stack(&self, stack: &[NodeId]) -> Vec<NodeId> {
        let mut out = self.initial_stack.clone();
        out.extend(stack.iter().copied());
        out
    }
}
