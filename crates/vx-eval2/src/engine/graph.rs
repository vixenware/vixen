use super::*;
use crate::value::compare_value_reps;
use std::cmp::Ordering;
use std::mem;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeReady {
    pub value: Value,
    pub events: Vec<EvalEventKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeExecRequest {
    pub args: Vec<Value>,
    pub env: Vec<(Arc<str>, Value)>,
    pub mounts: Vec<(Value, Value)>,
    pub writable: Vec<Value>,
    pub create_dirs: Vec<Value>,
    pub executor: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeExecReady {
    pub process: Value,
    pub vfs: Value,
    pub events: Vec<EvalEventKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCallOutcome {
    Unsupported,
    Ready(Value),
    ReadyWithEvents(RuntimeReady),
    Pending { description: Arc<str> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSlotPrepare {
    Reused(RuntimeReady),
    Resolve { current: Option<Value> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalNodeState {
    Ready(Arc<Value>),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EvalWaveId(pub u64);

#[derive(Debug, Clone, Default)]
pub struct EvalBranchContext {
    pub(super) key: Arc<str>,
    assumptions: Arc<[(Arc<str>, Value)]>,
}

impl EvalBranchContext {
    pub fn new(key: impl Into<Arc<str>>) -> Self {
        Self {
            key: key.into(),
            assumptions: Arc::from([]),
        }
    }

    pub fn with_assumption(&self, name: impl Into<Arc<str>>, value: Value) -> Self {
        let name = name.into();
        let mut assumptions = self.assumptions.iter().cloned().collect::<Vec<_>>();
        match assumptions.binary_search_by(|(existing, _)| existing.as_ref().cmp(name.as_ref())) {
            Ok(index) => assumptions[index] = (name, value),
            Err(index) => assumptions.insert(index, (name, value)),
        }
        Self {
            key: self.key.clone(),
            assumptions: Arc::from(assumptions),
        }
    }

    pub fn assumption(&self, name: &str) -> Option<&Value> {
        self.assumptions
            .iter()
            .find(|(existing, _)| existing.as_ref() == name)
            .map(|(_, value)| value)
    }
}

impl PartialEq for EvalBranchContext {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.assumptions.len() == other.assumptions.len()
            && self.assumptions.iter().zip(other.assumptions.iter()).all(
                |((left_name, left_value), (right_name, right_value))| {
                    left_name == right_name && left_value.rep == right_value.rep
                },
            )
    }
}

impl Eq for EvalBranchContext {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvalWorkKind {
    StartEntry,
    EvaluateExpr,
    EvaluateKeyed,
    ResumePending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalSessionStep {
    Progressed,
    Pending(PendingRef),
    WaitingOnInFlight,
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
    pub base_generation: u64,
    pub elapsed: Duration,
    pub invocation: Option<EvalAuthoredCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalBufferedWork {
    pub wave_id: EvalWaveId,
    pub lease_sequence: u64,
    pub work: EvalQueuedWork,
    pub outcome: EvalCompletedWorkOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalLeasedWorkState {
    InFlight,
    Buffered(EvalCompletedWorkOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalLeasedWork {
    pub lease_sequence: u64,
    pub work: EvalQueuedWork,
    pub state: EvalLeasedWorkState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalActiveWave {
    pub wave_id: EvalWaveId,
    pub work: Vec<EvalLeasedWork>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalWorkState {
    Queued,
    InFlight,
    Buffered(EvalCompletedWorkOutcome),
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
    Keyed { resolver: Arc<str>, key: Value },
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
pub enum EvalKeyedNodeUpdateReason {
    Created,
    JoinedWidened,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalKeyedNode {
    pub node_id: EvalNodeId,
    pub resolver_name: Arc<str>,
    pub key: Value,
    pub joined: Value,
    pub branch_context: EvalBranchContext,
    pub revision: u64,
    pub active_revision: Option<u64>,
    pub evaluated_revision: Option<u64>,
    pub last_update: EvalKeyedNodeUpdateReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyedNodeMemo {
    pub(super) node_id: EvalNodeId,
    pub(super) resolver: FunctionRef,
    pub(super) key: Value,
    pub(super) joined: Value,
    pub(super) branch_context: EvalBranchContext,
    pub(super) revision: u64,
    pub(super) active_revision: Option<u64>,
    pub(super) evaluated_revision: Option<u64>,
    pub(super) last_update: EvalKeyedNodeUpdateReason,
}

impl KeyedNodeMemo {
    pub(super) fn public(&self) -> EvalKeyedNode {
        EvalKeyedNode {
            node_id: self.node_id,
            resolver_name: self.resolver.name.clone(),
            key: self.key.clone(),
            joined: self.joined.clone(),
            branch_context: self.branch_context.clone(),
            revision: self.revision,
            active_revision: self.active_revision,
            evaluated_revision: self.evaluated_revision,
            last_update: self.last_update.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyedResolverIndexKey {
    source_id: Arc<str>,
    name: Arc<str>,
    parameters: Arc<[CompiledParameter]>,
    has_keyword_parameters: bool,
    body: Option<ExprId>,
    self_name: Option<Arc<str>>,
}

impl From<&FunctionRef> for KeyedResolverIndexKey {
    fn from(value: &FunctionRef) -> Self {
        Self {
            source_id: value.source_id.clone(),
            name: value.name.clone(),
            parameters: value.parameters.clone(),
            has_keyword_parameters: value.has_keyword_parameters,
            body: value.body.map(crate::program::ExprBodyRef::expr_id),
            self_name: value.self_name.clone(),
        }
    }
}

impl PartialOrd for KeyedResolverIndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for KeyedResolverIndexKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.source_id
            .cmp(&other.source_id)
            .then_with(|| self.name.cmp(&other.name))
            .then_with(|| {
                self.has_keyword_parameters
                    .cmp(&other.has_keyword_parameters)
            })
            .then_with(|| self.body.cmp(&other.body))
            .then_with(|| self.self_name.cmp(&other.self_name))
            .then_with(|| compare_compiled_parameter_slices(&self.parameters, &other.parameters))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyedBranchContextIndexKey {
    key: Arc<str>,
    assumptions: Arc<[(Arc<str>, ValueRep)]>,
}

impl From<&EvalBranchContext> for KeyedBranchContextIndexKey {
    fn from(value: &EvalBranchContext) -> Self {
        Self {
            key: value.key.clone(),
            assumptions: Arc::from(
                value
                    .assumptions
                    .iter()
                    .map(|(name, value)| (name.clone(), value.rep.clone()))
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

impl PartialOrd for KeyedBranchContextIndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for KeyedBranchContextIndexKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .cmp(&other.key)
            .then_with(|| compare_branch_assumption_slices(&self.assumptions, &other.assumptions))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyedLookupIndexKey {
    resolver: KeyedResolverIndexKey,
    key: ValueRep,
    branch_context: KeyedBranchContextIndexKey,
}

impl KeyedLookupIndexKey {
    pub(super) fn new(
        resolver: &FunctionRef,
        key: &Value,
        branch_context: &EvalBranchContext,
    ) -> Self {
        Self {
            resolver: resolver.into(),
            key: key.rep.clone(),
            branch_context: branch_context.into(),
        }
    }
}

impl PartialOrd for KeyedLookupIndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for KeyedLookupIndexKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.resolver
            .cmp(&other.resolver)
            .then_with(|| self.branch_context.cmp(&other.branch_context))
            .then_with(|| compare_value_reps(&self.key, &other.key))
    }
}

fn compare_branch_assumption_slices(
    lhs: &[(Arc<str>, ValueRep)],
    rhs: &[(Arc<str>, ValueRep)],
) -> Ordering {
    for ((lhs_name, lhs_value), (rhs_name, rhs_value)) in lhs.iter().zip(rhs.iter()) {
        let name_order = lhs_name.cmp(rhs_name);
        if name_order != Ordering::Equal {
            return name_order;
        }
        let value_order = compare_value_reps(lhs_value, rhs_value);
        if value_order != Ordering::Equal {
            return value_order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn compare_compiled_parameter_slices(
    lhs: &[CompiledParameter],
    rhs: &[CompiledParameter],
) -> Ordering {
    for (lhs, rhs) in lhs.iter().zip(rhs.iter()) {
        let label_order = lhs.label.cmp(&rhs.label);
        if label_order != Ordering::Equal {
            return label_order;
        }
        let name_order = lhs.name.cmp(&rhs.name);
        if name_order != Ordering::Equal {
            return name_order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LearnedNoGood {
    pub(super) branch_context: EvalBranchContext,
    pub(super) assumption: Arc<str>,
    pub(super) candidates: Arc<[Value]>,
    pub(super) start_index: usize,
    pub(super) end_index: usize,
    pub(super) resolver: FunctionRef,
    pub(super) error: EvalError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LearnedSearchNoGood {
    pub(super) branch_context: EvalBranchContext,
    pub(super) assumption: Arc<str>,
    pub(super) candidates: Arc<[Value]>,
    pub(super) resolver: FunctionRef,
    pub(super) error: EvalError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalExplainTerminal {
    Runnable {
        node_id: EvalNodeId,
        work: EvalWorkId,
        pending: Option<PendingRef>,
    },
    Buffered {
        node_id: EvalNodeId,
        work: EvalWorkId,
        pending: Option<PendingRef>,
        lease_sequence: u64,
        outcome: EvalCompletedWorkOutcome,
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
    pub keyed: Option<EvalKeyedNode>,
    pub state: Option<EvalNodeState>,
    pub dependencies: Vec<EvalNodeId>,
    pub dependents: Vec<EvalNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalGraphSnapshot {
    pub branch_context: EvalBranchContext,
    pub nodes: Vec<EvalGraphNode>,
    pub work: Vec<EvalWorkRecord>,
    pub active_wave: Option<EvalActiveWave>,
    pub pending: Vec<PendingRef>,
    pub completed: Option<Result<Value, EvalError>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSessionStats {
    pub graph_node_count: usize,
    pub node_record_count: usize,
    pub node_state_count: usize,
    pub keyed_node_count: usize,
    pub dependency_edge_count: usize,
    pub queued_work_count: usize,
    pub in_flight_work_count: usize,
    pub buffered_work_count: usize,
    pub pending_count: usize,
    pub completed_work_count: usize,
    pub trace_event_count: usize,
    pub learned_no_good_count: usize,
    pub widened_no_good_count: usize,
    pub learned_search_no_good_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalAuthoredCallFrame {
    pub root_node_id: EvalNodeId,
    pub root_work_id: Option<EvalWorkId>,
    pub callee: Arc<str>,
    pub args_fingerprint: Arc<str>,
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalAuthoredCall {
    pub root_node_id: EvalNodeId,
    pub root_work_id: Option<EvalWorkId>,
    pub caller_node_id: Option<EvalNodeId>,
    pub caller_work_id: Option<EvalWorkId>,
    pub callee: Arc<str>,
    pub args_fingerprint: Arc<str>,
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalProfileEntry {
    pub node_id: EvalNodeId,
    pub work_kind: EvalWorkKind,
    pub label: Arc<str>,
    pub source: Option<SourceSpan>,
    pub runs: usize,
    pub total_time: Duration,
    pub max_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSessionProfile {
    pub total_time: Duration,
    pub entries: Vec<EvalProfileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalProfileSummaryEntry {
    pub work_kind: EvalWorkKind,
    pub label: Arc<str>,
    pub source: Option<SourceSpan>,
    pub node_count: usize,
    pub runs: usize,
    pub total_time: Duration,
    pub max_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSessionProfileSummary {
    pub total_time: Duration,
    pub entries: Vec<EvalProfileSummaryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCallProfileRun {
    pub invocation: EvalAuthoredCall,
    pub work_count: usize,
    pub self_time: Duration,
    pub total_time: Duration,
    pub max_work_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCallProfileSummaryEntry {
    pub callee: Arc<str>,
    pub source: Option<SourceSpan>,
    pub runs: usize,
    pub distinct_input_count: usize,
    pub repeated_input_count: usize,
    pub self_time: Duration,
    pub total_time: Duration,
    pub max_total_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCallTreeEntry {
    pub callee: Arc<str>,
    pub source: Option<SourceSpan>,
    pub runs: usize,
    pub distinct_input_count: usize,
    pub repeated_input_count: usize,
    pub self_time: Duration,
    pub total_time: Duration,
    pub max_total_time: Duration,
    pub children: Vec<EvalCallTreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSessionCallProfile {
    pub attributed_time: Duration,
    pub runs: Vec<EvalCallProfileRun>,
    pub summary: Vec<EvalCallProfileSummaryEntry>,
    pub roots: Vec<EvalCallTreeEntry>,
}

#[derive(Debug, Clone)]
pub struct EvalSessionCheckpoint {
    pub(super) branch_context: EvalBranchContext,
    pub(super) scheduler: EvalScheduler,
    pub(super) trace_len: usize,
    pub(super) completed_work_len: usize,
    pub(super) completed: Option<Result<Value, EvalError>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EvalNodeTable<T: Clone> {
    dense: Vector<Option<T>>,
    sparse: OrdMap<EvalNodeId, T>,
    len: usize,
}

impl<T: Clone> Default for EvalNodeTable<T> {
    fn default() -> Self {
        Self {
            dense: Vector::new(),
            sparse: OrdMap::new(),
            len: 0,
        }
    }
}

impl<T: Clone> EvalNodeTable<T> {
    fn is_dense(node_id: EvalNodeId) -> bool {
        (node_id.0 >> 63) == 0
    }

    fn ensure_dense_len(&mut self, len: usize) {
        while self.dense.len() < len {
            self.dense.push_back(None);
        }
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn get(&self, node_id: &EvalNodeId) -> Option<&T> {
        if Self::is_dense(*node_id) {
            self.dense.get(node_id.0 as usize)?.as_ref()
        } else {
            self.sparse.get(node_id)
        }
    }

    pub(super) fn get_mut(&mut self, node_id: &EvalNodeId) -> Option<&mut T> {
        if Self::is_dense(*node_id) {
            self.dense.get_mut(node_id.0 as usize)?.as_mut()
        } else {
            self.sparse.get_mut(node_id)
        }
    }

    pub(super) fn insert(&mut self, node_id: EvalNodeId, value: T) -> Option<T> {
        if Self::is_dense(node_id) {
            let index = node_id.0 as usize;
            self.ensure_dense_len(index + 1);
            let slot = self
                .dense
                .get_mut(index)
                .expect("dense node slot must exist after extending");
            let previous = slot.replace(value);
            if previous.is_none() {
                self.len += 1;
            }
            previous
        } else if let Some(existing) = self.sparse.get_mut(&node_id) {
            Some(mem::replace(existing, value))
        } else {
            self.sparse.insert(node_id, value);
            self.len += 1;
            None
        }
    }

    pub(super) fn contains_key(&self, node_id: &EvalNodeId) -> bool {
        self.get(node_id).is_some()
    }

    pub(super) fn remove(&mut self, node_id: &EvalNodeId) -> Option<T> {
        if Self::is_dense(*node_id) {
            let slot = self.dense.get_mut(node_id.0 as usize)?;
            let previous = slot.take();
            if previous.is_some() {
                self.len -= 1;
            }
            previous
        } else {
            let previous = self.sparse.remove(node_id);
            if previous.is_some() {
                self.len -= 1;
            }
            previous
        }
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = EvalNodeId> + '_ {
        let dense = self
            .dense
            .iter()
            .enumerate()
            .filter_map(|(index, value)| value.as_ref().map(|_| EvalNodeId(index as u64)));
        let sparse = self.sparse.keys().copied();
        dense.chain(sparse)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (EvalNodeId, &T)> + '_ {
        let dense = self.dense.iter().enumerate().filter_map(|(index, value)| {
            value
                .as_ref()
                .map(|value| (EvalNodeId(index as u64), value))
        });
        let sparse = self.sparse.iter().map(|(node_id, value)| (*node_id, value));
        dense.chain(sparse)
    }
}

#[derive(Debug, Clone)]
pub(super) struct EvalSchedulerBase {
    pub(super) generation: u64,
    pub(super) next_id: u64,
    pub(super) next_work_id: u64,
    pub(super) node_states: EvalNodeTable<EvalNodeState>,
    pub(super) node_records: EvalNodeTable<EvalNodeRecord>,
    pub(super) keyed_nodes: EvalNodeTable<KeyedNodeMemo>,
    pub(super) keyed_lookup_index: OrdMap<KeyedLookupIndexKey, Vector<EvalNodeId>>,
    pub(super) dependency_edges: Vector<EvalDependencyEdge>,
    pub(super) pending_resumes: EvalNodeTable<Vec<EvalSuspension>>,
    pub(super) pending_kinds: EvalNodeTable<PendingKind>,
    pub(super) retained_exprs: EvalNodeTable<RetainedExprTask>,
    pub(super) learned_no_goods: Vector<LearnedNoGood>,
    pub(super) learned_search_no_goods: Vector<LearnedSearchNoGood>,
}

impl Default for EvalSchedulerBase {
    fn default() -> Self {
        Self {
            generation: 0,
            next_id: 1,
            next_work_id: 1,
            node_states: EvalNodeTable::default(),
            node_records: EvalNodeTable::default(),
            keyed_nodes: EvalNodeTable::default(),
            keyed_lookup_index: OrdMap::new(),
            dependency_edges: Vector::new(),
            pending_resumes: EvalNodeTable::default(),
            pending_kinds: EvalNodeTable::default(),
            retained_exprs: EvalNodeTable::default(),
            learned_no_goods: Vector::new(),
            learned_search_no_goods: Vector::new(),
        }
    }
}

impl EvalSchedulerBase {
    pub(super) fn keyed_lookup_candidates(
        &self,
        resolver: &FunctionRef,
        key: &Value,
        branch_context: &EvalBranchContext,
    ) -> Option<&Vector<EvalNodeId>> {
        let lookup_key = KeyedLookupIndexKey::new(resolver, key, branch_context);
        self.keyed_lookup_index.get(&lookup_key)
    }

    pub(super) fn insert_keyed_node(&mut self, memo: KeyedNodeMemo) -> Option<KeyedNodeMemo> {
        let lookup_key = KeyedLookupIndexKey::new(&memo.resolver, &memo.key, &memo.branch_context);
        let previous = self.keyed_nodes.insert(memo.node_id, memo.clone());
        if previous.is_none() {
            if let Some(bucket) = self.keyed_lookup_index.get_mut(&lookup_key) {
                bucket.push_back(memo.node_id);
            } else {
                let mut bucket = Vector::new();
                bucket.push_back(memo.node_id);
                self.keyed_lookup_index.insert(lookup_key, bucket);
            }
        }
        previous
    }
}

#[derive(Debug, Clone)]
pub(super) struct EvalPreparedSnapshot {
    pub(super) base: Arc<EvalSchedulerBase>,
    pub(super) lease_namespace: Option<u64>,
    pub(super) baseline_ready_tasks: Arc<[EvalWorkId]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EvalKeyedLookupValidation {
    pub(super) resolver: FunctionRef,
    pub(super) key: Value,
    pub(super) branch_context: EvalBranchContext,
    pub(super) result: Option<EvalNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EvalLearnedNoGoodValidation {
    pub(super) branch_context: EvalBranchContext,
    pub(super) assumption: Arc<str>,
    pub(super) candidates: Arc<[Value]>,
    pub(super) index: usize,
    pub(super) resolver: FunctionRef,
    pub(super) result: Option<EvalError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EvalLearnedSearchNoGoodValidation {
    pub(super) branch_context: EvalBranchContext,
    pub(super) assumption: Arc<str>,
    pub(super) candidates: Arc<[Value]>,
    pub(super) resolver: FunctionRef,
    pub(super) result: Option<EvalError>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct EvalSnapshotReadSet {
    pub(super) node_records: BTreeMap<EvalNodeId, Option<EvalNodeRecord>>,
    pub(super) node_states: BTreeMap<EvalNodeId, Option<EvalNodeState>>,
    pub(super) pending_kinds: BTreeMap<EvalNodeId, Option<PendingKind>>,
    pub(super) keyed_nodes: BTreeMap<EvalNodeId, Option<KeyedNodeMemo>>,
    pub(super) retained_exprs: BTreeMap<EvalNodeId, Option<RetainedExprTask>>,
    pub(super) dependency_reads: Vec<(EvalNodeId, Vec<EvalNodeId>)>,
    pub(super) keyed_lookups: Vec<EvalKeyedLookupValidation>,
    pub(super) learned_no_good_lookups: Vec<EvalLearnedNoGoodValidation>,
    pub(super) learned_search_no_good_lookups: Vec<EvalLearnedSearchNoGoodValidation>,
}

pub struct EvalPreparedWork<'runtime, Runtime: RuntimeHooks> {
    pub(super) program: Arc<CompiledProgram>,
    pub(super) entry: Arc<str>,
    pub(super) args: Arc<[Value]>,
    pub(super) runtime: &'runtime Runtime,
    pub(super) place: Arc<str>,
    pub(super) branch_context: EvalBranchContext,
    pub(super) wave_id: EvalWaveId,
    pub(super) lease_sequence: u64,
    pub(super) base_generation: u64,
    pub(super) scheduler: EvalPreparedSnapshot,
    pub(super) record: ReadyTaskRecord,
    pub(super) call_profiling: bool,
}

#[derive(Debug, Clone)]
pub struct EvalWorkCompletion {
    pub(super) branch_context: EvalBranchContext,
    pub(super) wave_id: EvalWaveId,
    pub(super) lease_sequence: u64,
    pub(super) base_generation: u64,
    pub(super) record: ReadyTaskRecord,
    pub(super) work: EvalQueuedWork,
    pub(super) outcome: EvalCompletedWorkOutcome,
    pub(super) delta: EvalSessionDelta,
    pub(super) elapsed: Duration,
    pub(super) invocation: Option<EvalAuthoredCall>,
}

#[derive(Debug, Clone)]
pub(super) struct EvalSessionDelta {
    pub(super) scheduler: RecordedSchedulerDelta,
    pub(super) reads: EvalSnapshotReadSet,
    pub(super) trace_events: Vec<EvalEvent>,
    pub(super) completed_invalidated: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RecordedSchedulerDelta {
    pub(super) next_id: u64,
    pub(super) next_work_id: u64,
    pub(super) node_records: BTreeMap<EvalNodeId, EvalNodeRecord>,
    pub(super) keyed_nodes: BTreeMap<EvalNodeId, KeyedNodeMemo>,
    pub(super) node_states: BTreeMap<EvalNodeId, EvalNodeState>,
    pub(super) dependency_edges: Vec<EvalDependencyEdge>,
    pub(super) queued: Vec<ReadyTaskRecord>,
    pub(super) pending_resumes: BTreeMap<EvalNodeId, Vec<EvalSuspension>>,
    pub(super) pending_kinds: BTreeMap<EvalNodeId, PendingKind>,
    pub(super) retained_exprs: BTreeMap<EvalNodeId, RetainedExprTask>,
    pub(super) learned_no_goods: Vec<LearnedNoGood>,
    pub(super) learned_search_no_goods: Vec<LearnedSearchNoGood>,
}
