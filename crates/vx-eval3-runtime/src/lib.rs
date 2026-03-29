use std::collections::{BTreeMap, VecDeque};
use vx_platform_exec::resolve_tool_from_path;

use vx_eval3_analysis::{
    BodyId, BridgeLocalBindingId, DiscoverBoundaryPayload, ExecBoundaryPayload, ExecSliceError,
    ExecutableBody, ExecutableClosure, ExecutableProgram, ExprId, KernelId, KernelOp, Operand,
    ResultSource, RuntimeBoundaryInputSpec, RuntimeBoundaryKind, RuntimeBoundaryPayload,
    RuntimeField, RuntimeMemberOp, RuntimeNodeId, SharedKeyedBoundaryPayload, SlotId, SourceExpr,
    TreeFilePath,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExecValue {
    Int(i64),
    Bool(bool),
    String(String),
    Host(HostValue),
    Array(Vec<ExecValue>),
    SandboxExemption(SandboxExemptionValue),
    SandboxTreeRoot(SandboxTreeRootValue),
    Blob(BlobValue),
    Tree(TreeValue),
    ProcessResult { exit_code: i64, stdout: String },
    Pair(Box<ExecValue>, Box<ExecValue>),
    Callable(CallableValue),
    LazyPoison(String),
    PendingRuntimeBoundaries(Vec<RuntimeNodeId>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CallableValue {
    AddConst(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostValue {
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SandboxExemptionValue {
    ExecSubpath(String),
    ReadSubpath(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SandboxTreeRootValue {
    pub exec_node: RuntimeNodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobValue {
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeValue {
    pub files: BTreeMap<String, BlobValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyMemoKey {
    pub body: BodyId,
    pub args: Vec<ExecValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeMemoKey {
    pub node: RuntimeNodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellState {
    Empty,
    Running { waiters: Vec<FrameId> },
    Ready(ExecValue),
    Poison(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub id: FrameId,
    pub body: BodyId,
    pub pc: usize,
    pub slots: BTreeMap<SlotId, SlotValue>,
    pub continuation: Option<Continuation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotValue {
    Unset,
    Ready(ExecValue),
    Waiting(WaitReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitReason {
    BodyCall(BodyMemoKey),
    RuntimeNode(RuntimeMemoKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continuation {
    pub dst: SlotId,
    pub wait: WaitReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    pub entry_result: ExecValue,
    pub body_results: BTreeMap<BodyId, ExecValue>,
    pub call_counts: BTreeMap<(BodyId, ExprId), usize>,
    pub body_cells: BTreeMap<BodyMemoKey, CellState>,
    pub runtime_cells: BTreeMap<RuntimeMemoKey, CellState>,
    pub wait_events: Vec<WaitReason>,
    pub body_eval_counts: BTreeMap<BodyMemoKey, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedEval {
    pub blocked_runtime_nodes: Vec<RuntimeNodeId>,
    pub body_cells: BTreeMap<BodyMemoKey, CellState>,
    pub runtime_cells: BTreeMap<RuntimeMemoKey, CellState>,
    pub wait_events: Vec<WaitReason>,
    pub call_counts: BTreeMap<(BodyId, ExprId), usize>,
    pub body_eval_counts: BTreeMap<BodyMemoKey, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalRunResult {
    Complete(EvalOutcome),
    Blocked(BlockedEval),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryRequest {
    Exec(ExecBoundaryRequest),
    Discover(DiscoverBoundaryRequest),
    SharedKeyed(SharedKeyedBoundaryRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecBoundaryRequest {
    pub node: RuntimeNodeId,
    pub source: SourceExpr,
    pub payload: ExecBoundaryPayload,
    pub inputs: ExecBoundaryInputs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverBoundaryRequest {
    pub node: RuntimeNodeId,
    pub source: SourceExpr,
    pub payload: DiscoverBoundaryPayload,
    pub inputs: DiscoverBoundaryInputs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedKeyedBoundaryRequest {
    pub node: RuntimeNodeId,
    pub source: SourceExpr,
    pub payload: SharedKeyedBoundaryPayload,
    pub inputs: SharedKeyedBoundaryInputs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecBoundaryInputs {
    pub positional_inputs: Vec<BoundaryPositionalInputValue>,
    pub coarse_mounts: Vec<BoundaryPositionalInputValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverBoundaryInputs {
    pub positional_inputs: Vec<BoundaryPositionalInputValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedKeyedBoundaryInputs {
    pub positional_inputs: Vec<BoundaryPositionalInputValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryPositionalInputValue {
    RuntimeValue {
        binding: BridgeLocalBindingId,
        value: ExecValue,
    },
    MissingBinding {
        binding: BridgeLocalBindingId,
    },
    IntLiteral(i64),
    StringLiteral(String),
    Closure {
        source: SourceExpr,
        closure: ExecutableClosure,
    },
    Array(Vec<BoundaryPositionalInputValue>),
    OpaqueExpr(SourceExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeBoundaryInputs {
    positional_inputs: Vec<BoundaryPositionalInputValue>,
    coarse_mounts: Vec<BoundaryPositionalInputValue>,
}

pub trait BoundaryDriver {
    fn resolve_boundary(&mut self, request: &BoundaryRequest) -> Result<ExecValue, ExecSliceError>;
}

pub fn eval_executable_program(program: &ExecutableProgram) -> Result<EvalOutcome, ExecSliceError> {
    let mut session = RuntimeSession::new(program)?;
    match session.run_until_blocked()? {
        EvalRunResult::Complete(outcome) => Ok(outcome),
        EvalRunResult::Blocked(blocked) => Err(ExecSliceError::TypeMismatch(format!(
            "evaluation blocked on runtime nodes {:?}",
            blocked.blocked_runtime_nodes
        ))),
    }
}

pub fn eval_executable_program_with_driver<D: BoundaryDriver>(
    program: &ExecutableProgram,
    driver: &mut D,
) -> Result<EvalOutcome, ExecSliceError> {
    let mut session = RuntimeSession::new(program)?;
    session.run_with_driver(driver)
}

pub fn eval_executable_closure(
    closure: &ExecutableClosure,
    args: Vec<ExecValue>,
) -> Result<ExecValue, ExecSliceError> {
    let body_id = BodyId(0);
    let program = ExecutableProgram {
        entry: body_id,
        bodies: BTreeMap::from([(
            body_id,
            ExecutableBody {
                id: body_id,
                kernel: KernelId(0),
                params: closure.params.clone(),
                ops: closure.ops.clone(),
                result_source: closure.result_source.clone(),
                runtime_boundary_extractions: vec![],
                local_call_dependencies: vec![],
            },
        )]),
        runtime_nodes: BTreeMap::new(),
    };
    let mut session = RuntimeSession::new_with_entry_args(&program, args)?;
    match session.run_until_blocked()? {
        EvalRunResult::Complete(outcome) => Ok(outcome.entry_result),
        EvalRunResult::Blocked(blocked) => Err(ExecSliceError::InvalidProgram(format!(
            "closure evaluation blocked on runtime nodes {:?}",
            blocked.blocked_runtime_nodes
        ))),
    }
}

pub struct RuntimeSession<'a> {
    machine: RuntimeMachine<'a>,
    entry_key: BodyMemoKey,
}

impl<'a> RuntimeSession<'a> {
    pub fn new(program: &'a ExecutableProgram) -> Result<Self, ExecSliceError> {
        Self::new_with_entry_args(program, vec![])
    }

    pub fn new_with_entry_args(
        program: &'a ExecutableProgram,
        entry_args: Vec<ExecValue>,
    ) -> Result<Self, ExecSliceError> {
        let entry_key = BodyMemoKey {
            body: program.entry,
            args: entry_args,
        };
        let mut machine = RuntimeMachine::new(program);
        machine
            .body_cells
            .insert(entry_key.clone(), CellState::Empty);
        machine.start_body_frame(entry_key.clone())?;
        Ok(Self { machine, entry_key })
    }

    pub fn run_until_blocked(&mut self) -> Result<EvalRunResult, ExecSliceError> {
        self.machine.run_scheduler()?;
        if let Some(CellState::Ready(entry)) = self.machine.body_cells.get(&self.entry_key) {
            return Ok(EvalRunResult::Complete(
                self.machine.complete_outcome(entry.clone()),
            ));
        }
        Ok(EvalRunResult::Blocked(self.machine.blocked_outcome()))
    }

    pub fn complete_runtime_node(
        &mut self,
        node: RuntimeNodeId,
        value: ExecValue,
    ) -> Result<(), ExecSliceError> {
        self.machine
            .complete_runtime(RuntimeMemoKey { node }, value)
    }

    pub fn run_with_driver<D: BoundaryDriver>(
        &mut self,
        driver: &mut D,
    ) -> Result<EvalOutcome, ExecSliceError> {
        loop {
            match self.run_until_blocked()? {
                EvalRunResult::Complete(outcome) => return Ok(outcome),
                EvalRunResult::Blocked(blocked) => {
                    if blocked.blocked_runtime_nodes.is_empty() {
                        return Err(ExecSliceError::InvalidProgram(
                            "session blocked without runtime nodes".to_owned(),
                        ));
                    }
                    for node in blocked.blocked_runtime_nodes {
                        let request = self.machine.boundary_request(node)?;
                        let value = driver.resolve_boundary(&request)?;
                        self.complete_runtime_node(node, value)?;
                    }
                }
            }
        }
    }
}

struct RuntimeMachine<'a> {
    program: &'a ExecutableProgram,
    next_frame: u32,
    ready_frames: VecDeque<FrameId>,
    frame_owner: BTreeMap<FrameId, BodyMemoKey>,
    frames: BTreeMap<FrameId, Frame>,
    body_cells: BTreeMap<BodyMemoKey, CellState>,
    runtime_cells: BTreeMap<RuntimeMemoKey, CellState>,
    runtime_pending_inputs: BTreeMap<RuntimeMemoKey, RuntimeBoundaryInputs>,
    call_counts: BTreeMap<(BodyId, ExprId), usize>,
    wait_events: Vec<WaitReason>,
    body_eval_counts: BTreeMap<BodyMemoKey, usize>,
}

impl<'a> RuntimeMachine<'a> {
    fn new(program: &'a ExecutableProgram) -> Self {
        let mut runtime_cells = BTreeMap::new();
        for node in program.runtime_nodes.keys() {
            runtime_cells.insert(RuntimeMemoKey { node: *node }, CellState::Empty);
        }
        Self {
            program,
            next_frame: 0,
            ready_frames: VecDeque::new(),
            frame_owner: BTreeMap::new(),
            frames: BTreeMap::new(),
            body_cells: BTreeMap::new(),
            runtime_cells,
            runtime_pending_inputs: BTreeMap::new(),
            call_counts: BTreeMap::new(),
            wait_events: vec![],
            body_eval_counts: BTreeMap::new(),
        }
    }

    fn boundary_request(&self, node: RuntimeNodeId) -> Result<BoundaryRequest, ExecSliceError> {
        let key = RuntimeMemoKey { node };
        let runtime_node = self.program.runtime_nodes.get(&node).ok_or_else(|| {
            ExecSliceError::InvalidProgram(format!("missing runtime node {}", node.0))
        })?;
        let inputs = self
            .runtime_pending_inputs
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                ExecSliceError::InvalidProgram(format!(
                    "runtime node {} missing pending concrete boundary inputs",
                    node.0
                ))
            })?;
        let request = match (runtime_node.kind, &runtime_node.payload) {
            (RuntimeBoundaryKind::Exec, RuntimeBoundaryPayload::Exec(payload)) => {
                BoundaryRequest::Exec(ExecBoundaryRequest {
                    node,
                    source: runtime_node.source.clone(),
                    payload: payload.clone(),
                    inputs: ExecBoundaryInputs {
                        positional_inputs: inputs.positional_inputs.clone(),
                        coarse_mounts: inputs.coarse_mounts.clone(),
                    },
                })
            }
            (RuntimeBoundaryKind::Discover, RuntimeBoundaryPayload::Discover(payload)) => {
                BoundaryRequest::Discover(DiscoverBoundaryRequest {
                    node,
                    source: runtime_node.source.clone(),
                    payload: payload.clone(),
                    inputs: DiscoverBoundaryInputs {
                        positional_inputs: inputs.positional_inputs.clone(),
                    },
                })
            }
            (RuntimeBoundaryKind::SharedKeyed, RuntimeBoundaryPayload::SharedKeyed(payload)) => {
                BoundaryRequest::SharedKeyed(SharedKeyedBoundaryRequest {
                    node,
                    source: runtime_node.source.clone(),
                    payload: payload.clone(),
                    inputs: SharedKeyedBoundaryInputs {
                        positional_inputs: inputs.positional_inputs.clone(),
                    },
                })
            }
            (kind, payload) => {
                return Err(ExecSliceError::InvalidProgram(format!(
                    "runtime node {} payload mismatch for boundary kind {:?}: {:?}",
                    node.0, kind, payload
                )));
            }
        };
        Ok(request)
    }

    fn start_body_frame(&mut self, key: BodyMemoKey) -> Result<(), ExecSliceError> {
        let body = self
            .program
            .bodies
            .get(&key.body)
            .ok_or(ExecSliceError::MissingBody(key.body))?;
        if body.params.len() != key.args.len() {
            return Err(ExecSliceError::TypeMismatch(format!(
                "missing args for body {}",
                key.body.0
            )));
        }

        self.body_cells
            .insert(key.clone(), CellState::Running { waiters: vec![] });
        *self.body_eval_counts.entry(key.clone()).or_insert(0) += 1;

        let frame_id = FrameId(self.next_frame);
        self.next_frame += 1;
        let mut slots = BTreeMap::<SlotId, SlotValue>::new();
        for (index, slot) in body.params.iter().enumerate() {
            slots.insert(*slot, SlotValue::Ready(key.args[index].clone()));
        }
        let frame = Frame {
            id: frame_id,
            body: body.id,
            pc: 0,
            slots,
            continuation: None,
        };
        self.frames.insert(frame_id, frame);
        self.frame_owner.insert(frame_id, key);
        self.ready_frames.push_back(frame_id);
        Ok(())
    }

    fn complete_outcome(&self, entry_result: ExecValue) -> EvalOutcome {
        let body_results = self
            .body_cells
            .iter()
            .filter_map(|(key, state)| match state {
                CellState::Ready(value) => Some((key.body, value.clone())),
                CellState::Empty | CellState::Running { .. } | CellState::Poison(_) => None,
            })
            .collect::<BTreeMap<_, _>>();
        EvalOutcome {
            entry_result,
            body_results,
            call_counts: self.call_counts.clone(),
            body_cells: self.body_cells.clone(),
            runtime_cells: self.runtime_cells.clone(),
            wait_events: self.wait_events.clone(),
            body_eval_counts: self.body_eval_counts.clone(),
        }
    }

    fn blocked_outcome(&self) -> BlockedEval {
        let mut blocked_runtime_nodes = self
            .runtime_cells
            .iter()
            .filter_map(|(key, state)| match state {
                CellState::Running { .. } => Some(key.node),
                CellState::Empty | CellState::Ready(_) | CellState::Poison(_) => None,
            })
            .collect::<Vec<_>>();
        blocked_runtime_nodes.sort_by_key(|id| id.0);
        blocked_runtime_nodes.dedup();
        BlockedEval {
            blocked_runtime_nodes,
            body_cells: self.body_cells.clone(),
            runtime_cells: self.runtime_cells.clone(),
            wait_events: self.wait_events.clone(),
            call_counts: self.call_counts.clone(),
            body_eval_counts: self.body_eval_counts.clone(),
        }
    }

    fn run_scheduler(&mut self) -> Result<(), ExecSliceError> {
        while let Some(frame_id) = self.ready_frames.pop_front() {
            let mut frame = if let Some(frame) = self.frames.remove(&frame_id) {
                frame
            } else {
                continue;
            };
            let body = self
                .program
                .bodies
                .get(&frame.body)
                .ok_or(ExecSliceError::MissingBody(frame.body))?;
            if frame.pc >= body.ops.len() {
                let owner_key = self.frame_owner.remove(&frame_id).ok_or_else(|| {
                    ExecSliceError::InvalidProgram("frame without owner".to_owned())
                })?;
                let result = match &body.result_source {
                    ResultSource::Slot(slot) => match frame.slots.get(slot) {
                        Some(SlotValue::Ready(value)) => value.clone(),
                        Some(SlotValue::Unset) | None => {
                            return Err(ExecSliceError::MissingSlot(*slot));
                        }
                        Some(SlotValue::Waiting(reason)) => {
                            return Err(ExecSliceError::TypeMismatch(format!(
                                "result slot still waiting: {:?}",
                                reason
                            )));
                        }
                    },
                    ResultSource::PendingRuntimeBoundary => ExecValue::PendingRuntimeBoundaries(
                        body.runtime_boundary_extractions.clone(),
                    ),
                };
                if let ExecValue::LazyPoison(message) = &result {
                    return Err(ExecSliceError::TypeMismatch(message.clone()));
                }
                self.complete_body(owner_key, result)?;
                continue;
            }

            let op = body.ops[frame.pc].clone();
            match op {
                KernelOp::AssignHostLocal { dst, .. } => {
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::Host(HostValue::Local)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::AssignSandboxTreeRoot { dst, node, .. } => {
                    frame.slots.insert(
                        dst,
                        SlotValue::Ready(ExecValue::SandboxTreeRoot(SandboxTreeRootValue {
                            exec_node: node,
                        })),
                    );
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::AssignConstInt { dst, value, .. } => {
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::Int(value)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::AssignConstString { dst, value, .. } => {
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::String(value)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::AssignAlias { dst, value, .. } => {
                    let value = resolve_slot_operand(&value, &frame.slots)?;
                    frame.slots.insert(dst, SlotValue::Ready(value));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::AssignLazyPoison { dst, label, .. } => {
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::LazyPoison(label)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::AssignCallableAddConst { dst, addend, .. } => {
                    let addend = resolve_slot_operand(&addend, &frame.slots)?;
                    let addend_int = as_int(&addend, "callable addend")?;
                    frame.slots.insert(
                        dst,
                        SlotValue::Ready(ExecValue::Callable(CallableValue::AddConst(addend_int))),
                    );
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::ApplyCallable {
                    dst, callable, arg, ..
                } => {
                    let callable = resolve_slot_operand(&callable, &frame.slots)?;
                    let arg = resolve_slot_operand(&arg, &frame.slots)?;
                    let result = apply_callable(&callable, arg)?;
                    frame.slots.insert(dst, SlotValue::Ready(result));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::MakePair {
                    dst, first, second, ..
                } => {
                    let first = resolve_slot_operand(&first, &frame.slots)?;
                    let second = resolve_slot_operand(&second, &frame.slots)?;
                    frame.slots.insert(
                        dst,
                        SlotValue::Ready(ExecValue::Pair(Box::new(first), Box::new(second))),
                    );
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::ProjectIndex {
                    dst, pair, index, ..
                } => {
                    let pair = resolve_slot_operand(&pair, &frame.slots)?;
                    let projected = match pair {
                        ExecValue::Pair(first, second) => match index {
                            0 => *first,
                            1 => *second,
                            _ => return Err(ExecSliceError::InvalidPairIndex(index)),
                        },
                        other => {
                            return Err(ExecSliceError::TypeMismatch(format!(
                                "project expects pair, got {:?}",
                                other
                            )));
                        }
                    };
                    frame.slots.insert(dst, SlotValue::Ready(projected));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::Add { dst, lhs, rhs, .. } => {
                    let lhs = as_int(&resolve_slot_operand(&lhs, &frame.slots)?, "add lhs")?;
                    let rhs = as_int(&resolve_slot_operand(&rhs, &frame.slots)?, "add rhs")?;
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::Int(lhs + rhs)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::NotEqual { dst, lhs, rhs, .. } => {
                    let lhs = resolve_slot_operand(&lhs, &frame.slots)?;
                    let rhs = resolve_slot_operand(&rhs, &frame.slots)?;
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::Bool(lhs != rhs)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::ProjectField {
                    dst, value, field, ..
                } => {
                    let value = resolve_slot_operand(&value, &frame.slots)?;
                    let projected = match (value, field) {
                        (ExecValue::ProcessResult { exit_code, .. }, RuntimeField::ExitCode) => {
                            ExecValue::Int(exit_code)
                        }
                        (ExecValue::ProcessResult { stdout, .. }, RuntimeField::Stdout) => {
                            ExecValue::String(stdout)
                        }
                        (other, _) => {
                            return Err(ExecSliceError::TypeMismatch(format!(
                                "unsupported runtime field projection on {:?}",
                                other
                            )));
                        }
                    };
                    frame.slots.insert(dst, SlotValue::Ready(projected));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::CallMember {
                    dst,
                    receiver,
                    method,
                    args,
                    ..
                } => {
                    let receiver = resolve_slot_operand(&receiver, &frame.slots)?;
                    let args = args
                        .iter()
                        .map(|arg| resolve_slot_operand(arg, &frame.slots))
                        .collect::<Result<Vec<_>, _>>()?;
                    let result = call_member(receiver, method, &args)?;
                    frame.slots.insert(dst, SlotValue::Ready(result));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::BuildTree { dst, entries, .. } => {
                    let mut files = BTreeMap::new();
                    for (TreeFilePath(path), value) in entries {
                        let value = resolve_slot_operand(&value, &frame.slots)?;
                        let blob = match value {
                            ExecValue::Blob(blob) => blob,
                            ExecValue::String(text) => BlobValue {
                                bytes: text.into_bytes(),
                            },
                            other => {
                                return Err(ExecSliceError::TypeMismatch(format!(
                                    "tree entry `{path}` expected blob-like value, got {:?}",
                                    other
                                )));
                            }
                        };
                        files.insert(path, blob);
                    }
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::Tree(TreeValue { files })));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::MakeArray { dst, items, .. } => {
                    let items = items
                        .iter()
                        .map(|item| resolve_slot_operand(item, &frame.slots))
                        .collect::<Result<Vec<_>, _>>()?;
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::Array(items)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::Select {
                    dst,
                    condition,
                    then_value,
                    else_value,
                    ..
                } => {
                    let condition = as_bool(
                        &resolve_slot_operand(&condition, &frame.slots)?,
                        "if condition",
                    )?;
                    let selected = if condition {
                        resolve_slot_operand(&then_value, &frame.slots)?
                    } else {
                        resolve_slot_operand(&else_value, &frame.slots)?
                    };
                    frame.slots.insert(dst, SlotValue::Ready(selected));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::AssignFailure { dst, message, .. } => {
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::LazyPoison(message)));
                    frame.pc += 1;
                    self.frames.insert(frame_id, frame);
                    self.ready_frames.push_back(frame_id);
                }
                KernelOp::CallBody {
                    dst,
                    source,
                    callee,
                    args,
                } => {
                    let arg_values = args
                        .iter()
                        .map(|arg| resolve_slot_operand(arg, &frame.slots))
                        .collect::<Result<Vec<_>, _>>()?;
                    *self
                        .call_counts
                        .entry((source.body, source.expr))
                        .or_insert(0) += 1;
                    let call_key = BodyMemoKey {
                        body: callee,
                        args: arg_values,
                    };
                    let wait = WaitReason::BodyCall(call_key.clone());
                    frame.slots.insert(dst, SlotValue::Waiting(wait.clone()));
                    frame.continuation = Some(Continuation {
                        dst,
                        wait: wait.clone(),
                    });
                    self.wait_events.push(wait);
                    let suspended = self.suspend_on_body(frame, call_key)?;
                    if !suspended {
                        self.ready_frames.push_back(frame_id);
                    }
                }
                KernelOp::AwaitRuntime {
                    dst,
                    node,
                    positional_inputs,
                    coarse_mounts,
                    ..
                } => {
                    let resolved_inputs = RuntimeBoundaryInputs {
                        positional_inputs: resolve_positional_inputs(
                            &positional_inputs,
                            &frame.slots,
                        )?,
                        coarse_mounts: resolve_positional_inputs(&coarse_mounts, &frame.slots)?,
                    };
                    let runtime_key = RuntimeMemoKey { node };
                    let wait = WaitReason::RuntimeNode(runtime_key.clone());
                    frame.slots.insert(dst, SlotValue::Waiting(wait.clone()));
                    frame.continuation = Some(Continuation {
                        dst,
                        wait: wait.clone(),
                    });
                    self.wait_events.push(wait);
                    self.suspend_on_runtime(frame, runtime_key.clone(), resolved_inputs)?;
                }
            }
        }
        Ok(())
    }

    fn suspend_on_body(&mut self, frame: Frame, key: BodyMemoKey) -> Result<bool, ExecSliceError> {
        let frame_id = frame.id;
        self.frames.insert(frame_id, frame);
        let state = self.body_cells.get(&key).cloned();
        match state {
            Some(CellState::Ready(value)) => {
                self.resume_frame_from_body(frame_id, key, value)?;
                Ok(false)
            }
            Some(CellState::Running { .. }) => {
                if let Some(CellState::Running { waiters }) = self.body_cells.get_mut(&key) {
                    waiters.push(frame_id);
                }
                Ok(true)
            }
            Some(CellState::Poison(err)) => Err(ExecSliceError::TypeMismatch(err.clone())),
            Some(CellState::Empty) | None => {
                self.start_body_frame(key.clone())?;
                if let Some(CellState::Running { waiters }) = self.body_cells.get_mut(&key) {
                    waiters.push(frame_id);
                }
                Ok(true)
            }
        }
    }

    fn suspend_on_runtime(
        &mut self,
        frame: Frame,
        key: RuntimeMemoKey,
        resolved_inputs: RuntimeBoundaryInputs,
    ) -> Result<(), ExecSliceError> {
        let frame_id = frame.id;
        self.frames.insert(frame_id, frame);
        let state = self.runtime_cells.get(&key).cloned();
        match state {
            Some(CellState::Ready(value)) => {
                self.resume_frame_from_runtime(frame_id, value)?;
            }
            Some(CellState::Running { .. }) => {
                if let Some(CellState::Running { waiters }) = self.runtime_cells.get_mut(&key) {
                    waiters.push(frame_id);
                }
                self.runtime_pending_inputs
                    .entry(key.clone())
                    .or_insert(resolved_inputs);
            }
            Some(CellState::Poison(err)) => return Err(ExecSliceError::TypeMismatch(err.clone())),
            Some(CellState::Empty) | None => {
                self.runtime_cells.insert(
                    key.clone(),
                    CellState::Running {
                        waiters: vec![frame_id],
                    },
                );
                self.runtime_pending_inputs.insert(key, resolved_inputs);
            }
        }
        Ok(())
    }

    fn complete_body(&mut self, key: BodyMemoKey, value: ExecValue) -> Result<(), ExecSliceError> {
        let waiters = match self.body_cells.remove(&key) {
            Some(CellState::Running { waiters }) => waiters,
            Some(CellState::Ready(_)) | Some(CellState::Empty) | None => vec![],
            Some(CellState::Poison(err)) => return Err(ExecSliceError::TypeMismatch(err)),
        };
        self.body_cells
            .insert(key.clone(), CellState::Ready(value.clone()));
        for waiter in waiters {
            self.resume_frame_from_body(waiter, key.clone(), value.clone())?;
        }
        Ok(())
    }

    fn complete_runtime(
        &mut self,
        key: RuntimeMemoKey,
        value: ExecValue,
    ) -> Result<(), ExecSliceError> {
        let waiters = match self.runtime_cells.remove(&key) {
            Some(CellState::Running { waiters }) => waiters,
            Some(CellState::Ready(_)) | Some(CellState::Empty) | None => vec![],
            Some(CellState::Poison(err)) => return Err(ExecSliceError::TypeMismatch(err)),
        };
        self.runtime_cells
            .insert(key.clone(), CellState::Ready(value.clone()));
        self.runtime_pending_inputs.remove(&key);
        for waiter in waiters {
            self.resume_frame_from_runtime(waiter, value.clone())?;
        }
        Ok(())
    }

    fn resume_frame_from_body(
        &mut self,
        frame_id: FrameId,
        key: BodyMemoKey,
        value: ExecValue,
    ) -> Result<(), ExecSliceError> {
        let mut frame = self.frames.remove(&frame_id).ok_or_else(|| {
            ExecSliceError::InvalidProgram(format!("missing frame {}", frame_id.0))
        })?;
        let Some(cont) = frame.continuation.take() else {
            return Err(ExecSliceError::InvalidProgram(
                "missing continuation for body resume".to_owned(),
            ));
        };
        match cont.wait {
            WaitReason::BodyCall(wait_key) if wait_key == key => {
                frame.slots.insert(cont.dst, SlotValue::Ready(value));
                frame.pc += 1;
                self.frames.insert(frame_id, frame);
                self.ready_frames.push_back(frame_id);
                Ok(())
            }
            other => Err(ExecSliceError::InvalidProgram(format!(
                "body resume mismatch: {:?}",
                other
            ))),
        }
    }

    fn resume_frame_from_runtime(
        &mut self,
        frame_id: FrameId,
        value: ExecValue,
    ) -> Result<(), ExecSliceError> {
        let mut frame = self.frames.remove(&frame_id).ok_or_else(|| {
            ExecSliceError::InvalidProgram(format!("missing frame {}", frame_id.0))
        })?;
        let Some(cont) = frame.continuation.take() else {
            return Err(ExecSliceError::InvalidProgram(
                "missing continuation for runtime resume".to_owned(),
            ));
        };
        match cont.wait {
            WaitReason::RuntimeNode(_) => {
                frame.slots.insert(cont.dst, SlotValue::Ready(value));
                frame.pc += 1;
                self.frames.insert(frame_id, frame);
                self.ready_frames.push_back(frame_id);
                Ok(())
            }
            other => Err(ExecSliceError::InvalidProgram(format!(
                "runtime resume mismatch: {:?}",
                other
            ))),
        }
    }
}

fn resolve_positional_inputs(
    inputs: &[RuntimeBoundaryInputSpec],
    slots: &BTreeMap<SlotId, SlotValue>,
) -> Result<Vec<BoundaryPositionalInputValue>, ExecSliceError> {
    inputs
        .iter()
        .map(|input| {
            Ok(match input {
                RuntimeBoundaryInputSpec::Binding { binding, slot } => {
                    BoundaryPositionalInputValue::RuntimeValue {
                        binding: *binding,
                        value: resolve_slot_operand(&Operand::Slot(*slot), slots)?,
                    }
                }
                RuntimeBoundaryInputSpec::UnresolvedBinding { binding } => {
                    BoundaryPositionalInputValue::MissingBinding { binding: *binding }
                }
                RuntimeBoundaryInputSpec::IntLiteral(value) => {
                    BoundaryPositionalInputValue::IntLiteral(*value)
                }
                RuntimeBoundaryInputSpec::StringLiteral(value) => {
                    BoundaryPositionalInputValue::StringLiteral(value.clone())
                }
                RuntimeBoundaryInputSpec::Closure { source, closure } => {
                    BoundaryPositionalInputValue::Closure {
                        source: source.clone(),
                        closure: closure.clone(),
                    }
                }
                RuntimeBoundaryInputSpec::Array(items) => {
                    BoundaryPositionalInputValue::Array(resolve_positional_inputs(items, slots)?)
                }
                RuntimeBoundaryInputSpec::OpaqueExpr { source } => {
                    BoundaryPositionalInputValue::OpaqueExpr(source.clone())
                }
            })
        })
        .collect::<Result<Vec<_>, ExecSliceError>>()
}

fn resolve_slot_operand(
    operand: &Operand,
    slots: &BTreeMap<SlotId, SlotValue>,
) -> Result<ExecValue, ExecSliceError> {
    match operand {
        Operand::Slot(slot) => match slots.get(slot) {
            Some(SlotValue::Ready(value)) => Ok(value.clone()),
            Some(SlotValue::Unset) | None => Err(ExecSliceError::MissingSlot(*slot)),
            Some(SlotValue::Waiting(reason)) => Err(ExecSliceError::TypeMismatch(format!(
                "slot {:?} still waiting: {:?}",
                slot, reason
            ))),
        },
        Operand::Int(value) => Ok(ExecValue::Int(*value)),
        Operand::String(value) => Ok(ExecValue::String(value.clone())),
    }
}

fn as_int(value: &ExecValue, context: &str) -> Result<i64, ExecSliceError> {
    match value {
        ExecValue::Int(int) => Ok(*int),
        other => Err(ExecSliceError::TypeMismatch(format!(
            "{context} expected int, got {:?}",
            other
        ))),
    }
}

fn as_bool(value: &ExecValue, context: &str) -> Result<bool, ExecSliceError> {
    match value {
        ExecValue::Bool(value) => Ok(*value),
        other => Err(ExecSliceError::TypeMismatch(format!(
            "{context} expected bool, got {:?}",
            other
        ))),
    }
}

fn call_member(
    receiver: ExecValue,
    method: RuntimeMemberOp,
    args: &[ExecValue],
) -> Result<ExecValue, ExecSliceError> {
    match (receiver, method) {
        (ExecValue::String(text), RuntimeMemberOp::Collect) => {
            if !args.is_empty() {
                return Err(ExecSliceError::UnsupportedShape(
                    "collect() does not take arguments".to_owned(),
                ));
            }
            Ok(ExecValue::String(text))
        }
        (ExecValue::String(text), RuntimeMemberOp::Blob) => {
            if !args.is_empty() {
                return Err(ExecSliceError::UnsupportedShape(
                    "blob() does not take arguments".to_owned(),
                ));
            }
            Ok(ExecValue::Blob(BlobValue {
                bytes: text.into_bytes(),
            }))
        }
        (ExecValue::Blob(blob), RuntimeMemberOp::Blob) => {
            if !args.is_empty() {
                return Err(ExecSliceError::UnsupportedShape(
                    "blob() does not take arguments".to_owned(),
                ));
            }
            Ok(ExecValue::Blob(blob))
        }
        (ExecValue::Host(HostValue::Local), RuntimeMemberOp::Which) => {
            if args.len() != 1 {
                return Err(ExecSliceError::UnsupportedShape(
                    "which(program) expects one argument".to_owned(),
                ));
            }
            let ExecValue::String(program) = &args[0] else {
                return Err(ExecSliceError::TypeMismatch(
                    "which(program) expects string argument".to_owned(),
                ));
            };
            let resolved = resolve_tool_from_path(program).ok_or_else(|| {
                ExecSliceError::TypeMismatch(format!(
                    "could not resolve tool `{program}` from PATH"
                ))
            })?;
            Ok(ExecValue::String(resolved))
        }
        (ExecValue::Host(HostValue::Local), RuntimeMemberOp::ExecSubpath) => {
            if args.len() != 1 {
                return Err(ExecSliceError::UnsupportedShape(
                    "exec_subpath(path) expects one argument".to_owned(),
                ));
            }
            let ExecValue::String(path) = &args[0] else {
                return Err(ExecSliceError::TypeMismatch(
                    "exec_subpath(path) expects string argument".to_owned(),
                ));
            };
            Ok(ExecValue::SandboxExemption(
                SandboxExemptionValue::ExecSubpath(path.clone()),
            ))
        }
        (ExecValue::Host(HostValue::Local), RuntimeMemberOp::ReadSubpath) => {
            if args.len() != 1 {
                return Err(ExecSliceError::UnsupportedShape(
                    "read_subpath(path) expects one argument".to_owned(),
                ));
            }
            let ExecValue::String(path) = &args[0] else {
                return Err(ExecSliceError::TypeMismatch(
                    "read_subpath(path) expects string argument".to_owned(),
                ));
            };
            Ok(ExecValue::SandboxExemption(
                SandboxExemptionValue::ReadSubpath(path.clone()),
            ))
        }
        (other, _) => Err(ExecSliceError::TypeMismatch(format!(
            "unsupported runtime member call on {:?}",
            other
        ))),
    }
}

fn apply_callable(callable: &ExecValue, arg: ExecValue) -> Result<ExecValue, ExecSliceError> {
    let arg = as_int(&arg, "call arg")?;
    match callable {
        ExecValue::Callable(CallableValue::AddConst(value)) => Ok(ExecValue::Int(arg + value)),
        other => Err(ExecSliceError::TypeMismatch(format!(
            "apply expects callable, got {:?}",
            other
        ))),
    }
}
