use std::collections::{HashMap, HashSet, VecDeque};
use std::thread;

use crate::node::NodeId;
use crate::poison::PoisonError;
use crate::value::Value;
use crate::worker::Engine;

pub(crate) struct Scheduler<'e> {
    engine: &'e Engine,
    initial_stack: Vec<NodeId>,
    deps: HashMap<NodeId, HashSet<NodeId>>,
    rdeps: HashMap<NodeId, Vec<NodeId>>,
    ready: VecDeque<NodeId>,
    ready_set: HashSet<NodeId>,
    demanded: HashSet<NodeId>,
    completed: HashSet<NodeId>,
    demand_paths: HashMap<NodeId, Vec<NodeId>>,
}

impl<'e> Scheduler<'e> {
    pub(crate) fn new(engine: &'e Engine) -> Self {
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

    pub(crate) fn with_initial_stack(engine: &'e Engine, initial_stack: Vec<NodeId>) -> Self {
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

    pub(crate) fn resolve_value(mut self, mut value: Value) -> Result<Value, PoisonError> {
        loop {
            let inspected = self.engine.inspect_value(value)?;
            if inspected.pending.is_empty() {
                if inspected.errors.is_empty() {
                    return Ok(inspected.value);
                }
                let stack = self.prefixed_stack(&[]);
                return Err(self.engine.scheduler_aggregate_poison_errors(
                    "forcing dependencies",
                    &stack,
                    inspected.errors,
                ));
            }

            let pending = inspected.pending.iter().copied().collect::<Vec<_>>();
            for node_id in &pending {
                let mut stack = self.demand_paths.get(node_id).cloned().unwrap_or_default();
                self.demand_node(*node_id, &mut stack)?;
            }

            if self.ready.is_empty() {
                let mut unresolved = Vec::new();
                for node_id in pending {
                    if !self.refresh_completion(node_id)? {
                        unresolved.push(node_id);
                    }
                }
                if !unresolved.is_empty() {
                    let stack = self.prefixed_stack(&[]);
                    return Err(self.engine.scheduler_stalled(&stack, &unresolved));
                }
            }

            self.run_ready_nodes()?;
            value = inspected.value;
        }
    }

    pub(crate) fn resolve_node_shallow(mut self, node_id: NodeId) -> Result<Value, PoisonError> {
        let mut stack = Vec::new();
        self.demand_node(node_id, &mut stack)?;
        while !self.refresh_completion(node_id)? {
            if self.ready.is_empty() {
                let stack = self.prefixed_stack(&[]);
                return Err(self.engine.scheduler_stalled(&stack, &[node_id]));
            }
            self.run_ready_nodes()?;
        }

        match self.engine.scheduler_node_state(node_id)? {
            crate::worker::SchedulerNodeState::Resolved(value) => Ok(value),
            crate::worker::SchedulerNodeState::Poisoned(error) => Err(error),
            crate::worker::SchedulerNodeState::Pending => {
                let stack = self.prefixed_stack(&[]);
                Err(self.engine.scheduler_stalled(&stack, &[node_id]))
            }
        }
    }

    fn demand_node(&mut self, node_id: NodeId, stack: &mut Vec<NodeId>) -> Result<(), PoisonError> {
        if self.refresh_completion(node_id)? || self.demanded.contains(&node_id) {
            return Ok(());
        }
        if self.initial_stack.contains(&node_id) || stack.contains(&node_id) {
            let stack = self.prefixed_stack(stack);
            return Err(self.engine.scheduler_circular_dependency(&stack, node_id));
        }
        if self.initial_stack.len() + stack.len() + 1 > self.engine.scheduler_max_demand_depth() {
            let stack = self.prefixed_stack(stack);
            return Err(self.engine.scheduler_demand_depth_exceeded(&stack, node_id));
        }

        self.demand_paths
            .entry(node_id)
            .or_insert_with(|| stack.clone());
        stack.push(node_id);
        let mut unresolved = HashSet::new();
        for dep in self.engine.scheduler_node_deps(node_id)? {
            self.demand_node(dep, stack)?;
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

    fn run_ready_nodes(&mut self) -> Result<(), PoisonError> {
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
                vec![(
                    node_id,
                    self.engine
                        .scheduler_execute_pending_node_with_chain(node_id, &chain),
                )]
            } else {
                let engine = self.engine;
                let stalled_batch = batch.clone();
                let demand_paths = self.demand_paths.clone();
                thread::scope(|scope| {
                    let handles = batch
                        .iter()
                        .copied()
                        .map(|node_id| {
                            let chain = demand_paths.get(&node_id).cloned().unwrap_or_default();
                            scope.spawn(move || {
                                (
                                    node_id,
                                    engine
                                        .scheduler_execute_pending_node_with_chain(node_id, &chain),
                                )
                            })
                        })
                        .collect::<Vec<_>>();

                    let mut results = Vec::with_capacity(handles.len());
                    for handle in handles {
                        match handle.join() {
                            Ok(result) => results.push(result),
                            Err(_) => {
                                let stack = self.prefixed_stack(&[]);
                                return Err(engine.scheduler_stalled(&stack, &stalled_batch));
                            }
                        }
                    }
                    Ok::<_, PoisonError>(results)
                })?
            };

            for (node_id, result) in results {
                if let Ok(value) = &result {
                    let inspected = self.engine.inspect_value(value.clone())?;
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
                        self.recompute_node(waiter)?;
                    }
                }
            }
        }

        Ok(())
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
        match self.engine.scheduler_node_state(node_id)? {
            crate::worker::SchedulerNodeState::Pending => Ok(false),
            crate::worker::SchedulerNodeState::Resolved(_)
            | crate::worker::SchedulerNodeState::Poisoned(_) => {
                self.completed.insert(node_id);
                Ok(true)
            }
        }
    }

    fn recompute_node(&mut self, node_id: NodeId) -> Result<(), PoisonError> {
        if self.refresh_completion(node_id)? {
            self.deps.remove(&node_id);
            return Ok(());
        }

        let mut path = self.demand_paths.get(&node_id).cloned().unwrap_or_default();
        if self.initial_stack.len() + path.len() + 1 > self.engine.scheduler_max_demand_depth() {
            let stack = self.prefixed_stack(&path);
            return Err(self.engine.scheduler_demand_depth_exceeded(&stack, node_id));
        }

        path.push(node_id);
        let mut unresolved = HashSet::new();
        for dep in self.engine.scheduler_node_deps(node_id)? {
            self.demand_node(dep, &mut path)?;
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

    fn add_reverse_dep(&mut self, dep: NodeId, waiter: NodeId) {
        let waiters = self.rdeps.entry(dep).or_default();
        if !waiters.contains(&waiter) {
            waiters.push(waiter);
        }
    }

    fn prefixed_stack(&self, stack: &[NodeId]) -> Vec<NodeId> {
        let mut out = self.initial_stack.clone();
        out.extend(stack.iter().copied());
        out
    }
}
