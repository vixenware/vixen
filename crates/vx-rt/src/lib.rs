use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use thiserror::Error;
use vx_analysis::{BodyId, ExecutableProgram, KernelOp, Operand, ResultSource, SlotId};

#[derive(Debug, Error)]
pub enum RtError {
    #[error("missing body {0:?}")]
    MissingBody(BodyId),
    #[error("arity mismatch: body expects {expected}, got {actual}")]
    ArityMismatch { expected: usize, actual: usize },
    #[error("missing slot {0:?}")]
    MissingSlot(SlotId),
    #[error("no producer for slot {0:?}")]
    NoProducer(SlotId),
    #[error("type error: expected {expected}, got {actual}")]
    TypeError {
        expected: &'static str,
        actual: String,
    },
    #[error("unsupported op: {0}")]
    Unsupported(String),
    #[error("explicit failure: {0}")]
    Failure(String),
    #[error("negative int literal {0}")]
    NegativeInt(i64),
}

#[derive(Debug, Clone)]
pub enum RtValue {
    U64(u64),
    Bool(bool),
    String(String),
    Pair(Thunk, Thunk),
    CallableAddConst(Box<RtValue>),
    Closure(BodyId, Vec<RtValue>),
    Thunk(Thunk),
    Failure(String),
}

impl RtValue {
    fn kind_name(&self) -> &'static str {
        match self {
            RtValue::U64(_) => "u64",
            RtValue::Bool(_) => "bool",
            RtValue::String(_) => "string",
            RtValue::Pair(_, _) => "pair",
            RtValue::CallableAddConst(_) => "callable_add_const",
            RtValue::Closure(_, _) => "closure",
            RtValue::Thunk(_) => "thunk",
            RtValue::Failure(_) => "failure",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Thunk(Rc<RefCell<ThunkInner>>);

#[derive(Debug)]
enum ThunkInner {
    Ready(RtValue),
    Suspended {
        body: BodyId,
        captures: Vec<RtValue>,
    },
}

impl Thunk {
    fn ready(val: RtValue) -> Self {
        Thunk(Rc::new(RefCell::new(ThunkInner::Ready(val))))
    }

    fn suspended(body: BodyId, captures: Vec<RtValue>) -> Self {
        Thunk(Rc::new(RefCell::new(ThunkInner::Suspended {
            body,
            captures,
        })))
    }

    fn force(&self, program: &ExecutableProgram) -> Result<RtValue, RtError> {
        {
            let inner = self.0.borrow();
            if let ThunkInner::Ready(v) = &*inner {
                return Ok(v.clone());
            }
        }
        // Suspended — extract, evaluate, memoize
        let (body, captures) = {
            let inner = self.0.borrow();
            match &*inner {
                ThunkInner::Suspended { body, captures } => (*body, captures.clone()),
                ThunkInner::Ready(v) => return Ok(v.clone()),
            }
        };
        let val = eval_body(program, body, &captures)?;
        *self.0.borrow_mut() = ThunkInner::Ready(val.clone());
        Ok(val)
    }
}

fn force_scalar(val: RtValue, program: &ExecutableProgram) -> Result<RtValue, RtError> {
    match val {
        RtValue::Thunk(t) => {
            let forced = t.force(program)?;
            force_scalar(forced, program)
        }
        other => Ok(other),
    }
}

fn expect_u64(val: &RtValue) -> Result<u64, RtError> {
    match val {
        RtValue::U64(n) => Ok(*n),
        other => Err(RtError::TypeError {
            expected: "u64",
            actual: other.kind_name().to_owned(),
        }),
    }
}

fn expect_bool(val: &RtValue) -> Result<bool, RtError> {
    match val {
        RtValue::Bool(b) => Ok(*b),
        other => Err(RtError::TypeError {
            expected: "bool",
            actual: other.kind_name().to_owned(),
        }),
    }
}

pub fn eval_program(program: &ExecutableProgram, args: &[RtValue]) -> Result<RtValue, RtError> {
    let result = eval_body(program, program.entry, args)?;
    force_scalar(result, program)
}

fn eval_body(
    program: &ExecutableProgram,
    body_id: BodyId,
    args: &[RtValue],
) -> Result<RtValue, RtError> {
    let body = program
        .bodies
        .get(&body_id)
        .ok_or(RtError::MissingBody(body_id))?;
    if body.params.len() != args.len() {
        return Err(RtError::ArityMismatch {
            expected: body.params.len(),
            actual: args.len(),
        });
    }

    let mut slots: BTreeMap<SlotId, RtValue> = BTreeMap::new();
    for (slot, val) in body.params.iter().copied().zip(args.iter().cloned()) {
        slots.insert(slot, val);
    }

    let producers: HashMap<SlotId, &KernelOp> =
        body.ops.iter().map(|op| (produced_slot(op), op)).collect();

    match &body.result_source {
        ResultSource::Slot(slot) => read_slot(program, &producers, &mut slots, *slot),
        ResultSource::PendingRuntimeBoundary => {
            Err(RtError::Unsupported("PendingRuntimeBoundary".to_owned()))
        }
    }
}

fn read_slot(
    program: &ExecutableProgram,
    producers: &HashMap<SlotId, &KernelOp>,
    slots: &mut BTreeMap<SlotId, RtValue>,
    slot: SlotId,
) -> Result<RtValue, RtError> {
    if let Some(val) = slots.get(&slot) {
        return Ok(val.clone());
    }
    let op = producers.get(&slot).ok_or(RtError::NoProducer(slot))?;
    let val = eval_op(program, producers, slots, op)?;
    slots.insert(slot, val.clone());
    Ok(val)
}

fn resolve_operand(
    program: &ExecutableProgram,
    producers: &HashMap<SlotId, &KernelOp>,
    slots: &mut BTreeMap<SlotId, RtValue>,
    operand: &Operand,
) -> Result<RtValue, RtError> {
    match operand {
        Operand::Slot(s) => read_slot(program, producers, slots, *s),
        Operand::Int(n) => {
            if *n < 0 {
                Err(RtError::NegativeInt(*n))
            } else {
                Ok(RtValue::U64(*n as u64))
            }
        }
        Operand::String(s) => Ok(RtValue::String(s.clone())),
    }
}

fn resolve_and_force(
    program: &ExecutableProgram,
    producers: &HashMap<SlotId, &KernelOp>,
    slots: &mut BTreeMap<SlotId, RtValue>,
    operand: &Operand,
) -> Result<RtValue, RtError> {
    let val = resolve_operand(program, producers, slots, operand)?;
    force_scalar(val, program)
}

fn eval_op(
    program: &ExecutableProgram,
    producers: &HashMap<SlotId, &KernelOp>,
    slots: &mut BTreeMap<SlotId, RtValue>,
    op: &KernelOp,
) -> Result<RtValue, RtError> {
    match op {
        KernelOp::AssignConstInt { value, .. } => {
            if *value < 0 {
                Err(RtError::NegativeInt(*value))
            } else {
                Ok(RtValue::U64(*value as u64))
            }
        }
        KernelOp::AssignConstString { value, .. } => Ok(RtValue::String(value.clone())),
        KernelOp::AssignAlias { value, .. } => resolve_operand(program, producers, slots, value),
        KernelOp::AssignCallableAddConst { addend, .. } => {
            let addend = resolve_operand(program, producers, slots, addend)?;
            Ok(RtValue::CallableAddConst(Box::new(addend)))
        }

        KernelOp::ApplyCallable { callable, arg, .. } => {
            let callable = resolve_and_force(program, producers, slots, callable)?;
            let arg = resolve_and_force(program, producers, slots, arg)?;
            match callable {
                RtValue::CallableAddConst(addend) => Ok(RtValue::U64(
                    expect_u64(&arg)?.wrapping_add(expect_u64(&addend)?),
                )),
                other => Err(RtError::TypeError {
                    expected: "callable",
                    actual: other.kind_name().to_owned(),
                }),
            }
        }

        KernelOp::AssignLazyPoison { label, .. } => {
            Ok(RtValue::Failure(format!("lazy poison: {label}")))
        }

        KernelOp::AllocThunk {
            thunk_body,
            captures,
            ..
        } => {
            let mut capture_vals = Vec::with_capacity(captures.len());
            for cap in captures {
                capture_vals.push(resolve_operand(program, producers, slots, cap)?);
            }
            Ok(RtValue::Thunk(Thunk::suspended(*thunk_body, capture_vals)))
        }

        KernelOp::MakePair { first, second, .. } => {
            let first_val = resolve_operand(program, producers, slots, first)?;
            let second_val = resolve_operand(program, producers, slots, second)?;
            // Wrap in thunks: if already a thunk, keep it; if ready, wrap as ready thunk
            let first_thunk = match first_val {
                RtValue::Thunk(t) => t,
                v => Thunk::ready(v),
            };
            let second_thunk = match second_val {
                RtValue::Thunk(t) => t,
                v => Thunk::ready(v),
            };
            Ok(RtValue::Pair(first_thunk, second_thunk))
        }

        KernelOp::ProjectIndex { pair, index, .. } => {
            let pair_val = resolve_operand(program, producers, slots, pair)?;
            let pair_val = force_scalar(pair_val, program)?;
            match pair_val {
                RtValue::Pair(first, second) => match index {
                    0 => first.force(program),
                    1 => second.force(program),
                    n => Err(RtError::Unsupported(format!("pair index {n}"))),
                },
                other => Err(RtError::TypeError {
                    expected: "pair",
                    actual: other.kind_name().to_owned(),
                }),
            }
        }

        KernelOp::Add { lhs, rhs, .. } => {
            let lhs = resolve_and_force(program, producers, slots, lhs)?;
            let rhs = resolve_and_force(program, producers, slots, rhs)?;
            Ok(RtValue::U64(
                expect_u64(&lhs)?.wrapping_add(expect_u64(&rhs)?),
            ))
        }

        KernelOp::NotEqual { lhs, rhs, .. } => {
            let lhs = resolve_and_force(program, producers, slots, lhs)?;
            let rhs = resolve_and_force(program, producers, slots, rhs)?;
            Ok(RtValue::Bool(expect_u64(&lhs)? != expect_u64(&rhs)?))
        }

        KernelOp::Select {
            condition,
            then_value,
            else_value,
            ..
        } => {
            let cond = resolve_and_force(program, producers, slots, condition)?;
            if expect_bool(&cond)? {
                resolve_operand(program, producers, slots, then_value)
            } else {
                resolve_operand(program, producers, slots, else_value)
            }
        }

        KernelOp::CallBody { callee, args, .. } => {
            let mut arg_vals = Vec::with_capacity(args.len());
            for arg in args {
                arg_vals.push(resolve_operand(program, producers, slots, arg)?);
            }
            eval_body(program, *callee, &arg_vals)
        }

        KernelOp::AssignFailure { message, .. } => {
            let msg = resolve_and_force(program, producers, slots, message)?;
            match msg {
                RtValue::String(s) => Err(RtError::Failure(s)),
                other => Err(RtError::Failure(format!("{:?}", other))),
            }
        }

        KernelOp::ConcatStrings { parts, .. } => {
            let mut result = String::new();
            for part in parts {
                let val = resolve_and_force(program, producers, slots, part)?;
                match val {
                    RtValue::String(s) => result.push_str(&s),
                    RtValue::U64(n) => result.push_str(&n.to_string()),
                    other => {
                        return Err(RtError::TypeError {
                            expected: "string or u64",
                            actual: other.kind_name().to_owned(),
                        });
                    }
                }
            }
            Ok(RtValue::String(result))
        }

        // Stubs for ops not yet needed
        _ => Err(RtError::Unsupported(format!("{op:?}"))),
    }
}

fn produced_slot(op: &KernelOp) -> SlotId {
    match op {
        KernelOp::AssignHostLocal { dst, .. }
        | KernelOp::AssignSandboxTreeRoot { dst, .. }
        | KernelOp::AssignConstInt { dst, .. }
        | KernelOp::AssignConstString { dst, .. }
        | KernelOp::MakeBlob { dst, .. }
        | KernelOp::AssignAlias { dst, .. }
        | KernelOp::AssignLazyPoison { dst, .. }
        | KernelOp::AllocThunk { dst, .. }
        | KernelOp::AssignCallableAddConst { dst, .. }
        | KernelOp::ApplyCallable { dst, .. }
        | KernelOp::MakePair { dst, .. }
        | KernelOp::ProjectIndex { dst, .. }
        | KernelOp::Add { dst, .. }
        | KernelOp::NotEqual { dst, .. }
        | KernelOp::ProjectField { dst, .. }
        | KernelOp::CallMember { dst, .. }
        | KernelOp::BuildTree { dst, .. }
        | KernelOp::MakeArray { dst, .. }
        | KernelOp::ConcatStrings { dst, .. }
        | KernelOp::StringLen { dst, .. }
        | KernelOp::StringSlice { dst, .. }
        | KernelOp::Select { dst, .. }
        | KernelOp::AssignFailure { dst, .. }
        | KernelOp::CallBody { dst, .. }
        | KernelOp::AwaitRuntime { dst, .. } => *dst,
    }
}
