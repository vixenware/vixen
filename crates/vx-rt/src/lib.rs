use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use thiserror::Error;
use vx_analysis::{BodyId, ExecutableProgram, KernelOp, Operand, ResultSource, SlotId};
use vx_vir::compile::CompileError;

#[derive(Debug, Error)]
pub enum RtError {
    #[error("compilation failed: {0}")]
    Compile(#[from] CompileError),
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
    Struct(Vec<(String, Thunk)>),
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
            RtValue::Struct(_) => "struct",
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

fn rt_eq(a: &RtValue, b: &RtValue) -> Result<bool, RtError> {
    match (a, b) {
        (RtValue::U64(a), RtValue::U64(b)) => Ok(a == b),
        (RtValue::Bool(a), RtValue::Bool(b)) => Ok(a == b),
        (RtValue::String(a), RtValue::String(b)) => Ok(a == b),
        _ => Err(RtError::TypeError {
            expected: a.kind_name(),
            actual: b.kind_name().to_owned(),
        }),
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

/// Full pipeline: source text → VIR → semantic passes → lower → demand-driven eval.
pub fn eval_source(
    source: &str,
    function_name: &str,
    args: &[RtValue],
) -> Result<RtValue, RtError> {
    let program = vx_vir::compile::compile_source(source, function_name)?;
    eval_program(&program, args)
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
        KernelOp::AssignConstBool { value, .. } => Ok(RtValue::Bool(*value)),
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

        KernelOp::AllocClosure { body, captures, .. } => {
            let mut capture_vals = Vec::with_capacity(captures.len());
            for cap in captures {
                capture_vals.push(resolve_operand(program, producers, slots, cap)?);
            }
            Ok(RtValue::Closure(*body, capture_vals))
        }

        KernelOp::ApplyClosure { closure, args, .. } => {
            let closure_val = resolve_and_force(program, producers, slots, closure)?;
            match closure_val {
                RtValue::Closure(body_id, capture_vals) => {
                    let mut all_args = capture_vals;
                    for arg in args {
                        all_args.push(resolve_operand(program, producers, slots, arg)?);
                    }
                    eval_body(program, body_id, &all_args)
                }
                other => Err(RtError::TypeError {
                    expected: "closure",
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

        KernelOp::MakeStruct { fields, .. } => {
            let mut thunk_fields = Vec::with_capacity(fields.len());
            for (name, operand) in fields {
                let val = resolve_operand(program, producers, slots, operand)?;
                let thunk = match val {
                    RtValue::Thunk(t) => t,
                    v => Thunk::ready(v),
                };
                thunk_fields.push((name.clone(), thunk));
            }
            Ok(RtValue::Struct(thunk_fields))
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

        KernelOp::ProjectNamedField { value, field, .. } => {
            let val = resolve_operand(program, producers, slots, value)?;
            let val = force_scalar(val, program)?;
            match val {
                RtValue::Struct(fields) => {
                    let (_, thunk) =
                        fields
                            .iter()
                            .find(|(name, _)| name == field)
                            .ok_or_else(|| RtError::TypeError {
                                expected: "field",
                                actual: format!("no field `{field}` in struct"),
                            })?;
                    thunk.force(program)
                }
                other => Err(RtError::TypeError {
                    expected: "struct",
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

        KernelOp::Equal { lhs, rhs, .. } => {
            let lhs = resolve_and_force(program, producers, slots, lhs)?;
            let rhs = resolve_and_force(program, producers, slots, rhs)?;
            Ok(RtValue::Bool(rt_eq(&lhs, &rhs)?))
        }

        KernelOp::NotEqual { lhs, rhs, .. } => {
            let lhs = resolve_and_force(program, producers, slots, lhs)?;
            let rhs = resolve_and_force(program, producers, slots, rhs)?;
            Ok(RtValue::Bool(!rt_eq(&lhs, &rhs)?))
        }

        KernelOp::Sub { lhs, rhs, .. } => {
            let lhs = resolve_and_force(program, producers, slots, lhs)?;
            let rhs = resolve_and_force(program, producers, slots, rhs)?;
            Ok(RtValue::U64(
                expect_u64(&lhs)?.wrapping_sub(expect_u64(&rhs)?),
            ))
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

        KernelOp::StringLen { value, .. } => {
            let val = resolve_and_force(program, producers, slots, value)?;
            match val {
                RtValue::String(s) => Ok(RtValue::U64(s.len() as u64)),
                other => Err(RtError::TypeError {
                    expected: "string",
                    actual: other.kind_name().to_owned(),
                }),
            }
        }

        KernelOp::StringSlice {
            value, start, end, ..
        } => {
            let val = resolve_and_force(program, producers, slots, value)?;
            let s = match val {
                RtValue::String(s) => s,
                other => {
                    return Err(RtError::TypeError {
                        expected: "string",
                        actual: other.kind_name().to_owned(),
                    });
                }
            };
            let start_val =
                expect_u64(&resolve_and_force(program, producers, slots, start)?)? as usize;
            let end_val = expect_u64(&resolve_and_force(program, producers, slots, end)?)? as usize;
            if start_val > s.len() || end_val > s.len() || start_val > end_val {
                return Err(RtError::Failure(format!(
                    "string slice out of bounds: [{start_val}..{end_val}] on string of length {}",
                    s.len()
                )));
            }
            Ok(RtValue::String(s[start_val..end_val].to_string()))
        }

        KernelOp::MakeArray { items, .. } => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                let val = resolve_operand(program, producers, slots, item)?;
                values.push(val);
            }
            // Arrays aren't a first-class RtValue yet — represent as a struct
            // with indexed fields for now.
            let thunk_fields: Vec<(String, Thunk)> = values
                .into_iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), Thunk::ready(v)))
                .collect();
            Ok(RtValue::Struct(thunk_fields))
        }

        KernelOp::AwaitRuntime { .. } => {
            // Runtime boundaries require host integration (file system, process execution).
            // In the unit test evaluator, they cannot be executed.
            Err(RtError::Unsupported(
                "AwaitRuntime requires host integration".into(),
            ))
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
        | KernelOp::AllocClosure { dst, .. }
        | KernelOp::ApplyClosure { dst, .. }
        | KernelOp::MakePair { dst, .. }
        | KernelOp::MakeStruct { dst, .. }
        | KernelOp::ProjectIndex { dst, .. }
        | KernelOp::ProjectNamedField { dst, .. }
        | KernelOp::Add { dst, .. }
        | KernelOp::AssignConstBool { dst, .. }
        | KernelOp::Equal { dst, .. }
        | KernelOp::NotEqual { dst, .. }
        | KernelOp::Sub { dst, .. }
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
