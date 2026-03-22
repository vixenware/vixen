use super::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeReady {
    pub value: Value,
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
    pub elapsed: Duration,
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

#[derive(Debug, Clone)]
pub struct EvalSessionCheckpoint {
    pub(super) branch_context: EvalBranchContext,
    pub(super) scheduler: EvalScheduler,
    pub(super) trace_len: usize,
    pub(super) completed_work_len: usize,
    pub(super) completed: Option<Result<Value, EvalError>>,
}

pub struct EvalPreparedWork<'runtime, Runtime: RuntimeHooks> {
    pub(super) program: Arc<CompiledProgram>,
    pub(super) entry: String,
    pub(super) args: Vec<Value>,
    pub(super) runtime: &'runtime Runtime,
    pub(super) place: String,
    pub(super) branch_context: EvalBranchContext,
    pub(super) wave_id: EvalWaveId,
    pub(super) lease_sequence: u64,
    pub(super) scheduler: EvalScheduler,
    pub(super) record: ReadyTaskRecord,
}

#[derive(Debug, Clone)]
pub struct EvalWorkCompletion {
    pub(super) branch_context: EvalBranchContext,
    pub(super) wave_id: EvalWaveId,
    pub(super) lease_sequence: u64,
    pub(super) work: EvalQueuedWork,
    pub(super) outcome: EvalCompletedWorkOutcome,
    pub(super) delta: EvalSessionDelta,
    pub(super) elapsed: Duration,
}

#[derive(Debug, Clone)]
pub(super) struct EvalSessionDelta {
    pub(super) scheduler: EvalSchedulerDelta,
    pub(super) trace_events: Vec<EvalEvent>,
    pub(super) completed_invalidated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EvalSchedulerDelta {
    pub(super) next_id: u64,
    pub(super) next_work_id: u64,
    pub(super) node_records: Vec<EvalNodeRecord>,
    pub(super) keyed_nodes: Vec<KeyedNodeMemo>,
    pub(super) node_states: Vec<(EvalNodeId, EvalNodeState)>,
    pub(super) dependency_edges: Vec<EvalDependencyEdge>,
    pub(super) queued: Vec<ReadyTaskRecord>,
    pub(super) pending_resumes: Vec<(u64, EvalSuspension)>,
    pub(super) pending_kinds: Vec<(u64, PendingKind)>,
    pub(super) retained_exprs: Vec<(EvalNodeId, RetainedExprTask)>,
    pub(super) learned_no_goods: Vec<LearnedNoGood>,
    pub(super) learned_search_no_goods: Vec<LearnedSearchNoGood>,
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
    pub(super) pending_resumes: BTreeMap<u64, Vec<EvalSuspension>>,
    pub(super) pending_kinds: BTreeMap<u64, PendingKind>,
    pub(super) retained_exprs: BTreeMap<EvalNodeId, RetainedExprTask>,
    pub(super) learned_no_goods: Vec<LearnedNoGood>,
    pub(super) learned_search_no_goods: Vec<LearnedSearchNoGood>,
}

impl RecordedSchedulerDelta {
    pub(super) fn into_delta(self) -> EvalSchedulerDelta {
        let pending_resumes = self
            .pending_resumes
            .into_iter()
            .flat_map(|(pending_id, suspensions)| {
                suspensions
                    .into_iter()
                    .map(move |suspension| (pending_id, suspension))
            })
            .collect();
        EvalSchedulerDelta {
            next_id: self.next_id,
            next_work_id: self.next_work_id,
            node_records: self.node_records.into_values().collect(),
            keyed_nodes: self.keyed_nodes.into_values().collect(),
            node_states: self.node_states.into_iter().collect(),
            dependency_edges: self.dependency_edges,
            queued: self.queued,
            pending_resumes,
            pending_kinds: self.pending_kinds.into_iter().collect(),
            retained_exprs: self.retained_exprs.into_iter().collect(),
            learned_no_goods: self.learned_no_goods,
            learned_search_no_goods: self.learned_search_no_goods,
        }
    }
}
