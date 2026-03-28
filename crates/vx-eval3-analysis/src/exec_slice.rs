use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use vx_parser::{AstNode, Expr, FnDecl, Stmt, SyntaxKind};
use vx_resolve::SourceSpan;

use crate::{
    AnalysisResult, AnalyzedBody, BodyId, BridgeLocalBindingId, BridgeLocalBindingKind,
    ExecSliceError::UnsupportedShape, ExprId, KernelId, RuntimeBoundaryKind, RuntimeNode,
    RuntimeNodeId, SingleSourceBridge, SourceExpr, SourceLocalCalleeOrigin,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableProgram {
    pub entry: BodyId,
    pub bodies: BTreeMap<BodyId, ExecutableBody>,
    pub runtime_nodes: BTreeMap<RuntimeNodeId, ExecutableRuntimeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableBody {
    pub id: BodyId,
    pub kernel: KernelId,
    pub params: Vec<SlotId>,
    pub ops: Vec<KernelOp>,
    pub result_source: ResultSource,
    pub runtime_boundary_extractions: Vec<RuntimeNodeId>,
    pub local_call_dependencies: Vec<BodyId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableRuntimeNode {
    pub id: RuntimeNodeId,
    pub kind: RuntimeBoundaryKind,
    pub source: SourceExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultSource {
    Slot(SlotId),
    PendingRuntimeBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Slot(SlotId),
    Int(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelOp {
    AssignConstInt {
        dst: SlotId,
        source: SourceExpr,
        value: i64,
    },
    AssignAlias {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
    },
    AssignLazyPoison {
        dst: SlotId,
        source: SourceExpr,
        label: String,
    },
    AssignCallableAddConst {
        dst: SlotId,
        source: SourceExpr,
        addend: Operand,
    },
    ApplyCallable {
        dst: SlotId,
        source: SourceExpr,
        callable: Operand,
        arg: Operand,
    },
    MakePair {
        dst: SlotId,
        source: SourceExpr,
        first: Operand,
        second: Operand,
    },
    ProjectIndex {
        dst: SlotId,
        source: SourceExpr,
        pair: Operand,
        index: usize,
    },
    Add {
        dst: SlotId,
        source: SourceExpr,
        lhs: Operand,
        rhs: Operand,
    },
    CallBody {
        dst: SlotId,
        source: SourceExpr,
        callee: BodyId,
        args: Vec<Operand>,
    },
    AwaitRuntime {
        dst: SlotId,
        source: SourceExpr,
        node: RuntimeNodeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExecValue {
    Int(i64),
    Pair(Box<ExecValue>, Box<ExecValue>),
    Callable(CallableValue),
    LazyPoison(String),
    PendingRuntimeBoundaries(Vec<RuntimeNodeId>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CallableValue {
    AddConst(i64),
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
pub enum ExecSliceError {
    UnsupportedShape(String),
    MissingBody(BodyId),
    MissingSlot(SlotId),
    MissingExpr(SourceExpr),
    MissingBindingForUse(SourceExpr),
    MissingBindingInitializer(BridgeLocalBindingId),
    TypeMismatch(String),
    InvalidPairIndex(usize),
    InvalidProgram(String),
}

pub fn lower_executable_program(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) -> Result<ExecutableProgram, ExecSliceError> {
    let expr_nodes = expr_nodes_by_id(bridge);
    let fn_by_body = body_nodes_by_id(bridge)?;
    let runtime_nodes = executable_runtime_nodes(analysis);
    let mut bodies = BTreeMap::new();

    for analyzed in &analysis.bodies {
        let runtime_boundary_extractions = runtime_pending_for_body(analysis, analyzed.id);
        let kernel = analyzed
            .kernel_entries
            .first()
            .map(|entry| entry.kernel)
            .ok_or_else(|| {
                UnsupportedShape(format!("body {} has no kernel entry", analyzed.id.0))
            })?;

        let fn_decl = fn_by_body.get(&analyzed.id).ok_or_else(|| {
            UnsupportedShape(format!("no AST function for body {}", analyzed.id.0))
        })?;
        let entry_expr = analyzed
            .kernel_entries
            .first()
            .map(|entry| entry.source.expr)
            .ok_or_else(|| {
                UnsupportedShape(format!("body {} has no kernel entry", analyzed.id.0))
            })?;

        let lowered = BodyLowering::new(
            analyzed.id,
            analyzed,
            fn_decl.clone(),
            bridge,
            &expr_nodes,
            entry_expr,
        )?
        .lower()?;

        bodies.insert(
            analyzed.id,
            ExecutableBody {
                id: analyzed.id,
                kernel,
                params: lowered.params,
                ops: lowered.ops,
                result_source: ResultSource::Slot(lowered.result),
                runtime_boundary_extractions,
                local_call_dependencies: lowered.local_call_dependencies,
            },
        );
    }

    let entry = pick_entry_body(analysis, bridge)?;
    Ok(ExecutableProgram {
        entry,
        bodies,
        runtime_nodes,
    })
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

pub struct RuntimeSession<'a> {
    machine: RuntimeMachine<'a>,
    entry_key: BodyMemoKey,
}

impl<'a> RuntimeSession<'a> {
    pub fn new(program: &'a ExecutableProgram) -> Result<Self, ExecSliceError> {
        let entry_key = BodyMemoKey {
            body: program.entry,
            args: vec![],
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
}

struct RuntimeMachine<'a> {
    program: &'a ExecutableProgram,
    next_frame: u32,
    ready_frames: VecDeque<FrameId>,
    frame_owner: BTreeMap<FrameId, BodyMemoKey>,
    frames: BTreeMap<FrameId, Frame>,
    body_cells: BTreeMap<BodyMemoKey, CellState>,
    runtime_cells: BTreeMap<RuntimeMemoKey, CellState>,
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
            call_counts: BTreeMap::new(),
            wait_events: vec![],
            body_eval_counts: BTreeMap::new(),
        }
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
                self.complete_body(owner_key, result)?;
                continue;
            }

            let op = body.ops[frame.pc].clone();
            match op {
                KernelOp::AssignConstInt { dst, value, .. } => {
                    frame
                        .slots
                        .insert(dst, SlotValue::Ready(ExecValue::Int(value)));
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
                KernelOp::AwaitRuntime { dst, node, .. } => {
                    let runtime_key = RuntimeMemoKey { node };
                    let wait = WaitReason::RuntimeNode(runtime_key.clone());
                    frame.slots.insert(dst, SlotValue::Waiting(wait.clone()));
                    frame.continuation = Some(Continuation {
                        dst,
                        wait: wait.clone(),
                    });
                    self.wait_events.push(wait);
                    self.suspend_on_runtime(frame, runtime_key.clone())?;
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
            }
            Some(CellState::Poison(err)) => return Err(ExecSliceError::TypeMismatch(err.clone())),
            Some(CellState::Empty) | None => {
                self.runtime_cells.insert(
                    key,
                    CellState::Running {
                        waiters: vec![frame_id],
                    },
                );
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
            .insert(key, CellState::Ready(value.clone()));
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

fn expr_nodes_by_id(bridge: &SingleSourceBridge) -> HashMap<ExprId, Expr> {
    let mut span_to_expr = HashMap::<SourceSpan, ExprId>::new();
    for expr in &bridge.exprs {
        if expr.span.source == bridge.source_id {
            span_to_expr.insert(expr.span, expr.id);
        }
    }

    let mut by_id = HashMap::new();
    for expr in bridge
        .parsed
        .tree()
        .syntax()
        .descendants()
        .filter_map(Expr::cast)
    {
        let span = SourceSpan::new(bridge.source_id, expr.syntax().text_range());
        if let Some(expr_id) = span_to_expr.get(&span).copied() {
            by_id.insert(expr_id, expr);
        }
    }
    by_id
}

fn body_nodes_by_id(
    bridge: &SingleSourceBridge,
) -> Result<HashMap<BodyId, FnDecl>, ExecSliceError> {
    let mut body_by_span = HashMap::<SourceSpan, BodyId>::new();
    for body in &bridge.bodies {
        if body.span.source == bridge.source_id {
            body_by_span.insert(body.span, body.id);
        }
    }

    let mut fn_by_body = HashMap::new();
    for item in bridge.parsed.tree().items() {
        let vx_parser::Item::FnDecl(fn_decl) = item else {
            continue;
        };
        let span = SourceSpan::new(bridge.source_id, fn_decl.syntax().text_range());
        let Some(body_id) = body_by_span.get(&span).copied() else {
            continue;
        };
        fn_by_body.insert(body_id, fn_decl);
    }

    if fn_by_body.is_empty() {
        return Err(UnsupportedShape(
            "bridge has no lowerable function declarations".to_owned(),
        ));
    }
    Ok(fn_by_body)
}

fn executable_runtime_nodes(
    analysis: &AnalysisResult,
) -> BTreeMap<RuntimeNodeId, ExecutableRuntimeNode> {
    let mut nodes = BTreeMap::new();
    for node in &analysis.runtime_nodes {
        let Some(kind) = node.boundary_kind() else {
            continue;
        };
        let (id, source) = match node {
            RuntimeNode::Exec { id, source }
            | RuntimeNode::Discover { id, source }
            | RuntimeNode::SharedKeyed { id, source, .. } => (*id, source.clone()),
            RuntimeNode::Lazy { .. } => continue,
        };
        nodes.insert(id, ExecutableRuntimeNode { id, kind, source });
    }
    nodes
}

fn runtime_pending_for_body(analysis: &AnalysisResult, body: BodyId) -> Vec<RuntimeNodeId> {
    let mut pending = analysis
        .bodies
        .iter()
        .find(|candidate| candidate.id == body)
        .map(|candidate| {
            candidate
                .extracted_nodes
                .iter()
                .filter_map(|node| match node.reason {
                    crate::ExtractedNodeReason::RuntimeBoundary { .. } => Some(node.id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    pending.sort_by_key(|id| id.0);
    pending
}

fn pick_entry_body(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) -> Result<BodyId, ExecSliceError> {
    let mut mains = bridge
        .bodies
        .iter()
        .filter(|body| body.name == "main" && body.span.source == bridge.source_id)
        .map(|body| body.id)
        .collect::<Vec<_>>();
    mains.sort_by_key(|id| id.0);
    mains.dedup();
    if let Some(main) = mains.first().copied() {
        return Ok(main);
    }

    let mut roots = analysis
        .bodies
        .iter()
        .filter(|body| {
            body.result_observation == crate::Observation::NeedValue
                && body.incoming_use_observation == crate::Observation::NoObs
        })
        .map(|body| body.id)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        roots = analysis
            .bodies
            .iter()
            .filter(|body| body.result_observation == crate::Observation::NeedValue)
            .map(|body| body.id)
            .collect::<Vec<_>>();
    }
    roots.sort_by_key(|id| id.0);
    roots.first().copied().ok_or_else(|| {
        UnsupportedShape("analysis has no demanded entry body for execution".to_owned())
    })
}

struct LoweredBody {
    params: Vec<SlotId>,
    ops: Vec<KernelOp>,
    result: SlotId,
    local_call_dependencies: Vec<BodyId>,
}

struct BodyLowering<'a> {
    body: BodyId,
    analyzed: &'a AnalyzedBody,
    bridge: &'a SingleSourceBridge,
    expr_nodes: &'a HashMap<ExprId, Expr>,
    entry_expr: ExprId,
    use_binding_by_expr: HashMap<ExprId, BridgeLocalBindingId>,
    binding_initializers: HashMap<BridgeLocalBindingId, ExprId>,
    slot_by_binding: HashMap<BridgeLocalBindingId, SlotId>,
    runtime_node_by_expr: HashMap<ExprId, RuntimeNodeId>,
    params: Vec<SlotId>,
    next_slot: u32,
    ops: Vec<KernelOp>,
    materialized_bindings: HashSet<BridgeLocalBindingId>,
    lazy_fields_by_expr: HashMap<ExprId, BTreeSet<u32>>,
    local_call_dependencies: BTreeSet<BodyId>,
    awaited_runtime_nodes: BTreeSet<RuntimeNodeId>,
}

impl<'a> BodyLowering<'a> {
    fn new(
        body: BodyId,
        analyzed: &'a AnalyzedBody,
        fn_decl: FnDecl,
        bridge: &'a SingleSourceBridge,
        expr_nodes: &'a HashMap<ExprId, Expr>,
        entry_expr: ExprId,
    ) -> Result<Self, ExecSliceError> {
        let body_bindings = bridge
            .local_bindings
            .iter()
            .filter(|binding| binding.body == body)
            .collect::<Vec<_>>();

        let mut use_binding_by_expr = HashMap::new();
        for usage in &bridge.local_uses {
            if usage.body == body {
                use_binding_by_expr.insert(usage.expr, usage.binding);
            }
        }

        let mut slot_by_binding = HashMap::new();
        let mut next_slot = 0u32;
        for binding in &body_bindings {
            slot_by_binding.insert(binding.id, SlotId(next_slot));
            next_slot += 1;
        }

        let mut params = Vec::new();
        for binding in body_bindings
            .iter()
            .filter(|binding| binding.kind == BridgeLocalBindingKind::Parameter)
        {
            params.push(*slot_by_binding.get(&binding.id).expect("slot exists"));
        }
        params.sort_by_key(|slot| slot.0);

        let mut binding_initializers = HashMap::new();
        if let Some(block) = fn_decl.body() {
            let let_bindings = body_bindings
                .iter()
                .filter(|binding| binding.kind == BridgeLocalBindingKind::Let)
                .collect::<Vec<_>>();
            let mut assigned = HashSet::<BridgeLocalBindingId>::new();

            for stmt in block.statements() {
                let Stmt::Let(let_stmt) = stmt else {
                    continue;
                };
                let stmt_span = SourceSpan::new(bridge.source_id, let_stmt.syntax().text_range());
                let Some(binding_id) = let_bindings
                    .iter()
                    .filter(|binding| !assigned.contains(&binding.id))
                    .find(|binding| {
                        binding.name_span.source == stmt_span.source
                            && stmt_span.range.contains_range(binding.name_span.range)
                    })
                    .map(|binding| binding.id)
                else {
                    continue;
                };
                let Some(expr) = let_stmt.expr() else {
                    continue;
                };
                let span = SourceSpan::new(bridge.source_id, expr.syntax().text_range());
                let Some(expr_id) = bridge
                    .exprs
                    .iter()
                    .find(|candidate| candidate.span == span)
                    .map(|candidate| candidate.id)
                else {
                    continue;
                };
                binding_initializers.insert(binding_id, expr_id);
                assigned.insert(binding_id);
            }
        }

        let mut runtime_node_by_expr = HashMap::new();
        for extracted in &analyzed.extracted_nodes {
            if matches!(
                extracted.reason,
                crate::ExtractedNodeReason::RuntimeBoundary { .. }
            ) {
                runtime_node_by_expr.insert(extracted.source.expr, extracted.id);
            }
        }

        let mut lazy_fields_by_expr = HashMap::<ExprId, BTreeSet<u32>>::new();
        for extracted in &analyzed.extracted_nodes {
            if let crate::ExtractedNodeReason::LazyProductField {
                field: crate::FieldKey::Index(index),
            } = extracted.reason
            {
                lazy_fields_by_expr
                    .entry(extracted.source.expr)
                    .or_default()
                    .insert(index);
            }
        }

        Ok(Self {
            body,
            analyzed,
            bridge,
            expr_nodes,
            entry_expr,
            use_binding_by_expr,
            binding_initializers,
            slot_by_binding,
            runtime_node_by_expr,
            params,
            next_slot,
            ops: vec![],
            materialized_bindings: HashSet::new(),
            lazy_fields_by_expr,
            local_call_dependencies: BTreeSet::new(),
            awaited_runtime_nodes: BTreeSet::new(),
        })
    }

    fn lower(mut self) -> Result<LoweredBody, ExecSliceError> {
        let result_operand = self.lower_expr(self.entry_expr)?;
        let result = self.ensure_slot(result_operand, self.source_for(self.entry_expr));
        Ok(LoweredBody {
            params: self.params,
            ops: self.ops,
            result,
            local_call_dependencies: self.local_call_dependencies.into_iter().collect(),
        })
    }

    fn source_for(&self, expr_id: ExprId) -> SourceExpr {
        SourceExpr {
            body: self.body,
            expr: expr_id,
        }
    }

    fn expr(&self, expr_id: ExprId) -> Result<Expr, ExecSliceError> {
        self.expr_nodes
            .get(&expr_id)
            .cloned()
            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))
    }

    fn expr_id(&self, expr: &Expr) -> Option<ExprId> {
        let span = SourceSpan::new(self.bridge.source_id, expr.syntax().text_range());
        self.bridge
            .exprs
            .iter()
            .find(|candidate| candidate.span == span)
            .map(|candidate| candidate.id)
    }

    fn alloc_temp(&mut self) -> SlotId {
        let slot = SlotId(self.next_slot);
        self.next_slot += 1;
        slot
    }

    fn ensure_slot(&mut self, operand: Operand, source: SourceExpr) -> SlotId {
        match operand {
            Operand::Slot(slot) => slot,
            Operand::Int(value) => {
                let slot = self.alloc_temp();
                self.ops.push(KernelOp::AssignConstInt {
                    dst: slot,
                    source,
                    value,
                });
                slot
            }
        }
    }

    fn materialize_binding(
        &mut self,
        binding: BridgeLocalBindingId,
    ) -> Result<SlotId, ExecSliceError> {
        let slot = *self
            .slot_by_binding
            .get(&binding)
            .ok_or_else(|| UnsupportedShape(format!("missing slot for binding {}", binding.0)))?;

        if self.materialized_bindings.contains(&binding) {
            return Ok(slot);
        }

        let Some(init_expr) = self.binding_initializers.get(&binding).copied() else {
            self.materialized_bindings.insert(binding);
            return Ok(slot);
        };

        let source = self.source_for(init_expr);
        let value = self.lower_expr(init_expr)?;
        if !(matches!(value, Operand::Slot(existing) if existing == slot)) {
            self.ops.push(KernelOp::AssignAlias {
                dst: slot,
                source,
                value,
            });
        }
        self.materialized_bindings.insert(binding);
        Ok(slot)
    }

    fn lower_expr(&mut self, expr_id: ExprId) -> Result<Operand, ExecSliceError> {
        let expr = self.expr(expr_id)?;
        match expr {
            Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or_else(|| UnsupportedShape("empty paren expression".to_owned()))?;
                let inner_id = self
                    .expr_id(&inner)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                self.lower_expr(inner_id)
            }
            Expr::Where(where_expr) => {
                let callee = where_expr.callee().ok_or_else(|| {
                    UnsupportedShape("where expression missing callee".to_owned())
                })?;
                let callee_id = self
                    .expr_id(&callee)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                self.lower_expr(callee_id)
            }
            Expr::Block(block) => {
                let statements = block.statements().collect::<Vec<_>>();
                let last_expr = statements.iter().rev().find_map(|stmt| match stmt {
                    Stmt::Expr(expr) => Some(expr.clone()),
                    Stmt::Return(ret) => ret.expr(),
                    _ => None,
                });
                let Some(last_expr) = last_expr else {
                    return Err(UnsupportedShape(
                        "block has no result expression".to_owned(),
                    ));
                };
                let last_id = self
                    .expr_id(&last_expr)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                self.lower_expr(last_id)
            }
            Expr::Literal(literal) => {
                let token = literal
                    .token()
                    .ok_or_else(|| UnsupportedShape("literal without token".to_owned()))?;
                if token.kind() != SyntaxKind::INT {
                    return Err(UnsupportedShape(format!(
                        "only INT literals supported, got `{}`",
                        token.text()
                    )));
                }
                let value = token
                    .text()
                    .replace('_', "")
                    .parse::<i64>()
                    .map_err(|_| UnsupportedShape(format!("invalid int `{}`", token.text())))?;
                Ok(Operand::Int(value))
            }
            Expr::NameRef(_) => {
                let binding = self.use_binding_by_expr.get(&expr_id).copied().ok_or(
                    ExecSliceError::MissingBindingForUse(self.source_for(expr_id)),
                )?;
                let slot = self.materialize_binding(binding)?;
                Ok(Operand::Slot(slot))
            }
            Expr::Binary(binary) => {
                let is_plus = binary
                    .syntax()
                    .children_with_tokens()
                    .filter_map(|child| child.into_token())
                    .any(|token| token.kind() == SyntaxKind::PLUS);
                if !is_plus {
                    return Err(UnsupportedShape(format!(
                        "only + binary is supported in exec slice (expr {})",
                        expr_id.0
                    )));
                }

                let lhs = binary
                    .lhs()
                    .ok_or_else(|| UnsupportedShape("binary lhs missing".to_owned()))?;
                let rhs = binary
                    .rhs()
                    .ok_or_else(|| UnsupportedShape("binary rhs missing".to_owned()))?;
                let lhs = self.lower_expr(
                    self.expr_id(&lhs)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?,
                )?;
                let rhs = self.lower_expr(
                    self.expr_id(&rhs)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?,
                )?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::Add {
                    dst,
                    source: self.source_for(expr_id),
                    lhs,
                    rhs,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Tuple(tuple) => {
                let items = tuple.items().collect::<Vec<_>>();
                if items.len() != 2 {
                    return Err(UnsupportedShape(format!(
                        "only 2-item tuples supported, got {}",
                        items.len()
                    )));
                }

                let mut operands = Vec::with_capacity(2);
                for (index, item) in items.iter().enumerate() {
                    let item_id = self
                        .expr_id(item)
                        .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                    match self.lower_expr(item_id) {
                        Ok(value) => operands.push(value),
                        Err(UnsupportedShape(_))
                            if self
                                .lazy_fields_by_expr
                                .get(&expr_id)
                                .is_some_and(|fields| fields.contains(&(index as u32))) =>
                        {
                            let lazy = self.alloc_temp();
                            self.ops.push(KernelOp::AssignLazyPoison {
                                dst: lazy,
                                source: self.source_for(item_id),
                                label: format!("lazy-field-{}:{}", expr_id.0, index),
                            });
                            operands.push(Operand::Slot(lazy));
                        }
                        Err(err) => return Err(err),
                    }
                }

                let dst = self.alloc_temp();
                self.ops.push(KernelOp::MakePair {
                    dst,
                    source: self.source_for(expr_id),
                    first: operands[0].clone(),
                    second: operands[1].clone(),
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Field(field) => {
                let field_name = field
                    .field_name()
                    .ok_or_else(|| UnsupportedShape("field name missing".to_owned()))?
                    .text()
                    .to_owned();
                let index = field_name
                    .strip_prefix('_')
                    .ok_or_else(|| UnsupportedShape(format!("unsupported field `{field_name}`")))?
                    .parse::<usize>()
                    .map_err(|_| UnsupportedShape(format!("invalid field `{field_name}`")))?;

                let receiver = field
                    .receiver()
                    .ok_or_else(|| UnsupportedShape("field receiver missing".to_owned()))?;

                if let Expr::NameRef(_) = receiver
                    && let Some(receiver_id) = self.expr_id(&receiver)
                    && let Some(binding) = self.use_binding_by_expr.get(&receiver_id).copied()
                    && let Some(tuple_expr) = self.binding_initializers.get(&binding).copied()
                    && let Expr::Tuple(tuple) = self.expr(tuple_expr)?
                {
                    let items = tuple.items().collect::<Vec<_>>();
                    if index < items.len() {
                        let selected_id = self
                            .expr_id(&items[index])
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(tuple_expr)))?;
                        return self.lower_expr(selected_id);
                    }
                }

                let receiver_id = self
                    .expr_id(&receiver)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                let pair = self.lower_expr(receiver_id)?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::ProjectIndex {
                    dst,
                    source: self.source_for(expr_id),
                    pair,
                    index,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Closure(closure) => {
                let mut params = closure.parameters();
                let Some(param) = params.next() else {
                    return Err(UnsupportedShape(
                        "closure requires one parameter".to_owned(),
                    ));
                };
                if params.next().is_some() {
                    return Err(UnsupportedShape(
                        "only single-parameter closure supported".to_owned(),
                    ));
                }
                let param_name = param
                    .name()
                    .ok_or_else(|| UnsupportedShape("closure parameter missing".to_owned()))?
                    .text()
                    .to_owned();
                let body = closure
                    .body()
                    .ok_or_else(|| UnsupportedShape("closure body missing".to_owned()))?;
                let body_id = self
                    .expr_id(&body)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                let Expr::Binary(binary) = self.expr(body_id)? else {
                    return Err(UnsupportedShape(
                        "only closure body `x + captured` supported".to_owned(),
                    ));
                };
                let lhs = binary
                    .lhs()
                    .ok_or_else(|| UnsupportedShape("closure lhs missing".to_owned()))?;
                let rhs = binary
                    .rhs()
                    .ok_or_else(|| UnsupportedShape("closure rhs missing".to_owned()))?;
                let addend = if is_param_name_ref(&lhs, &param_name) {
                    self.lower_expr(
                        self.expr_id(&rhs)
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(body_id)))?,
                    )?
                } else if is_param_name_ref(&rhs, &param_name) {
                    self.lower_expr(
                        self.expr_id(&lhs)
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(body_id)))?,
                    )?
                } else {
                    return Err(UnsupportedShape(
                        "closure add must include parameter on one side".to_owned(),
                    ));
                };

                let dst = self.alloc_temp();
                self.ops.push(KernelOp::AssignCallableAddConst {
                    dst,
                    source: self.source_for(expr_id),
                    addend,
                });
                Ok(Operand::Slot(dst))
            }
            Expr::Call(call) => {
                if let Some(node) = self.runtime_node_by_expr.get(&expr_id).copied() {
                    let dst = self.alloc_temp();
                    self.ops.push(KernelOp::AwaitRuntime {
                        dst,
                        source: self.source_for(expr_id),
                        node,
                    });
                    self.awaited_runtime_nodes.insert(node);
                    return Ok(Operand::Slot(dst));
                }

                let args = call
                    .positional_args()
                    .into_iter()
                    .map(|arg| {
                        let id = self
                            .expr_id(&arg)
                            .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                        self.lower_expr(id)
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                let source = self.source_for(expr_id);
                if let Some(origin) = self.analyzed.callee_local_origins.get(&expr_id) {
                    match origin {
                        SourceLocalCalleeOrigin::TopLevelBody(callee) => {
                            self.local_call_dependencies.insert(*callee);
                            let dst = self.alloc_temp();
                            self.ops.push(KernelOp::CallBody {
                                dst,
                                source,
                                callee: *callee,
                                args,
                            });
                            return Ok(Operand::Slot(dst));
                        }
                        SourceLocalCalleeOrigin::LocalClosure(_)
                        | SourceLocalCalleeOrigin::LocalDef(_) => {}
                    }
                }

                if args.len() != 1 {
                    return Err(UnsupportedShape(format!(
                        "callable apply supports one arg, got {}",
                        args.len()
                    )));
                }
                let callee_expr = call
                    .callee()
                    .ok_or_else(|| UnsupportedShape("call has no callee".to_owned()))?;
                let callee_id = self
                    .expr_id(&callee_expr)
                    .ok_or(ExecSliceError::MissingExpr(self.source_for(expr_id)))?;
                let callable = self.lower_expr(callee_id)?;
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::ApplyCallable {
                    dst,
                    source,
                    callable,
                    arg: args[0].clone(),
                });
                Ok(Operand::Slot(dst))
            }
            other => Err(UnsupportedShape(format!(
                "expr kind {:?} is outside restricted executable slice",
                other
            ))),
        }
    }
}

fn is_param_name_ref(expr: &Expr, name: &str) -> bool {
    let Expr::NameRef(name_ref) = expr else {
        return false;
    };
    name_ref.token().is_some_and(|token| token.text() == name)
}
