use std::collections::{BTreeMap, HashMap};

use thiserror::Error;
use vx_analysis::{
    AnalysisError, BodyId, ExecSliceError, ExecutableBody, ExecutableProgram, KernelOp, Operand,
    ResultSource, SingleSourceBridge, SlotId, build_minimal_analysis,
    lower_executable_program_for_entry_body,
};
use vx_services::RuntimeValue;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EvalValue {
    U64(u64),
    Bool(bool),
    String(String),
    Pair(Box<EvalValue>, Box<EvalValue>),
    Array(Vec<EvalValue>),
    CallableAddConst(Box<EvalValue>),
    Failure(String),
    Pending(String),
}

#[derive(Debug, Error)]
pub enum RuntimeEvalError {
    #[error("source has parse errors: {0:?}")]
    Parse(Vec<vx_parser::SyntaxError>),
    #[error("source has resolve errors: {0:?}")]
    Resolve(Vec<vx_resolve::ResolveError>),
    #[error("source has type errors: {0:?}")]
    Type(Vec<vx_resolve::TypeError>),
    #[error("source has capability errors: {0:?}")]
    Capability(Vec<vx_resolve::CapError>),
    #[error("analysis validation failed: {0:?}")]
    Analysis(AnalysisError),
    #[error("missing function `{0}`")]
    MissingFunction(String),
    #[error("failed to lower executable slice: {0:?}")]
    ExecSlice(ExecSliceError),
    #[error("arity mismatch in `{function}`: expected {expected} args, got {actual}")]
    ArityMismatch {
        function: String,
        expected: usize,
        actual: usize,
    },
    #[error("unsupported runtime boundary in `{function}`: {detail}")]
    UnsupportedBoundary { function: String, detail: String },
    #[error("unsupported operation in `{function}`: {detail}")]
    UnsupportedOperation { function: String, detail: String },
    #[error("missing slot s{slot} in `{function}`")]
    MissingSlot { function: String, slot: u32 },
    #[error("type mismatch in `{function}`: expected {expected}, found {found}")]
    TypeMismatch {
        function: String,
        expected: &'static str,
        found: &'static str,
    },
    #[error("negative integer literal {value} is not yet supported in `{function}`")]
    NegativeIntLiteral { function: String, value: i64 },
    #[error("function `{function}` produced a pending value: {label}")]
    PendingResult { function: String, label: String },
    #[error("function `{function}` failed: {message}")]
    FailureResult { function: String, message: String },
    #[error("function `{function}` returned an unsupported runtime value kind `{kind}`")]
    UnsupportedResultKind {
        function: String,
        kind: &'static str,
    },
}

pub fn evaluate_source_text_function(
    source: &str,
    function_name: &str,
    args: &[RuntimeValue],
) -> Result<RuntimeValue, RuntimeEvalError> {
    let bridge = checked_bridge(source)?;
    let body = bridge
        .bodies
        .iter()
        .find(|body| body.name == function_name)
        .ok_or_else(|| RuntimeEvalError::MissingFunction(function_name.to_owned()))?;
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().map_err(RuntimeEvalError::Analysis)?;
    let program = lower_executable_program_for_entry_body(&analysis, &bridge, body.id)
        .map_err(RuntimeEvalError::ExecSlice)?;

    let args = args
        .iter()
        .cloned()
        .map(eval_value_from_runtime)
        .collect::<Vec<_>>();
    let mut evaluator = ProgramEvaluator::new(&program);
    let value = evaluator.eval_body(function_name, body.id, &args)?;
    runtime_value_from_eval(function_name, value)
}

fn checked_bridge(source: &str) -> Result<SingleSourceBridge, RuntimeEvalError> {
    let bridge = SingleSourceBridge::from_source_text(source);
    if !bridge.parse_errors.is_empty() {
        return Err(RuntimeEvalError::Parse(bridge.parse_errors.clone()));
    }
    if !bridge.resolve_errors.is_empty() {
        return Err(RuntimeEvalError::Resolve(bridge.resolve_errors.clone()));
    }
    if !bridge.type_errors.is_empty() {
        return Err(RuntimeEvalError::Type(bridge.type_errors.clone()));
    }
    if !bridge.cap_errors.is_empty() {
        return Err(RuntimeEvalError::Capability(bridge.cap_errors.clone()));
    }
    Ok(bridge)
}

fn eval_value_from_runtime(value: RuntimeValue) -> EvalValue {
    match value {
        RuntimeValue::U64(value) => EvalValue::U64(value),
        RuntimeValue::Bool(value) => EvalValue::Bool(value),
        RuntimeValue::String(value) => EvalValue::String(value),
    }
}

fn runtime_value_from_eval(
    function_name: &str,
    value: EvalValue,
) -> Result<RuntimeValue, RuntimeEvalError> {
    match value {
        EvalValue::U64(value) => Ok(RuntimeValue::U64(value)),
        EvalValue::Bool(value) => Ok(RuntimeValue::Bool(value)),
        EvalValue::String(value) => Ok(RuntimeValue::String(value)),
        EvalValue::Pending(label) => Err(RuntimeEvalError::PendingResult {
            function: function_name.to_owned(),
            label,
        }),
        EvalValue::Failure(message) => Err(RuntimeEvalError::FailureResult {
            function: function_name.to_owned(),
            message,
        }),
        other => Err(RuntimeEvalError::UnsupportedResultKind {
            function: function_name.to_owned(),
            kind: other.kind_name(),
        }),
    }
}

struct ProgramEvaluator<'a> {
    program: &'a ExecutableProgram,
    memo: HashMap<(BodyId, Vec<EvalValue>), EvalValue>,
}

impl<'a> ProgramEvaluator<'a> {
    fn new(program: &'a ExecutableProgram) -> Self {
        Self {
            program,
            memo: HashMap::new(),
        }
    }

    fn eval_body(
        &mut self,
        function_name: &str,
        body_id: BodyId,
        args: &[EvalValue],
    ) -> Result<EvalValue, RuntimeEvalError> {
        let cache_key = (body_id, args.to_vec());
        if let Some(value) = self.memo.get(&cache_key) {
            return Ok(value.clone());
        }

        let body = self.program.bodies.get(&body_id).ok_or_else(|| {
            RuntimeEvalError::UnsupportedOperation {
                function: function_name.to_owned(),
                detail: format!("missing lowered body {}", body_id.0),
            }
        })?;
        if body.params.len() != args.len() {
            return Err(RuntimeEvalError::ArityMismatch {
                function: function_name.to_owned(),
                expected: body.params.len(),
                actual: args.len(),
            });
        }
        if !body.runtime_boundary_extractions.is_empty() {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!(
                    "body {} has runtime boundary extractions {:?}",
                    body_id.0, body.runtime_boundary_extractions
                ),
            });
        }

        let mut slots = BTreeMap::new();
        for (slot, value) in body.params.iter().copied().zip(args.iter().cloned()) {
            slots.insert(slot, value);
        }

        for op in &body.ops {
            self.exec_op(function_name, body, &mut slots, op)?;
        }

        let value = self.read_result(function_name, body, &slots)?;
        self.memo.insert(cache_key, value.clone());
        Ok(value)
    }

    fn exec_op(
        &mut self,
        function_name: &str,
        _body: &ExecutableBody,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        op: &KernelOp,
    ) -> Result<(), RuntimeEvalError> {
        match op {
            KernelOp::AssignConstInt { dst, value, .. } => {
                slots.insert(*dst, self.int_to_value(function_name, *value)?);
            }
            KernelOp::AssignConstString { dst, value, .. } => {
                slots.insert(*dst, EvalValue::String(value.clone()));
            }
            KernelOp::AssignAlias { dst, value, .. } => {
                slots.insert(*dst, self.resolve_operand(function_name, slots, value)?);
            }
            KernelOp::AssignLazyPoison { dst, label, .. } => {
                slots.insert(*dst, EvalValue::Pending(label.clone()));
            }
            KernelOp::AssignCallableAddConst { dst, addend, .. } => {
                let addend = self.resolve_operand(function_name, slots, addend)?;
                slots.insert(*dst, EvalValue::CallableAddConst(Box::new(addend)));
            }
            KernelOp::ApplyCallable {
                dst, callable, arg, ..
            } => {
                let callable = self.resolve_operand(function_name, slots, callable)?;
                let arg = self.resolve_operand(function_name, slots, arg)?;
                let value = match callable {
                    EvalValue::CallableAddConst(addend) => EvalValue::U64(
                        self.expect_u64(function_name, &arg)?
                            + self.expect_u64(function_name, &addend)?,
                    ),
                    other => {
                        return Err(RuntimeEvalError::TypeMismatch {
                            function: function_name.to_owned(),
                            expected: "callable",
                            found: other.kind_name(),
                        });
                    }
                };
                slots.insert(*dst, value);
            }
            KernelOp::MakePair {
                dst, first, second, ..
            } => {
                let first = self.resolve_operand(function_name, slots, first)?;
                let second = self.resolve_operand(function_name, slots, second)?;
                slots.insert(*dst, EvalValue::Pair(Box::new(first), Box::new(second)));
            }
            KernelOp::ProjectIndex {
                dst, pair, index, ..
            } => {
                let pair = self.resolve_operand(function_name, slots, pair)?;
                let value = match pair {
                    EvalValue::Pair(first, second) => match index {
                        0 => *first,
                        1 => *second,
                        other => {
                            return Err(RuntimeEvalError::UnsupportedOperation {
                                function: function_name.to_owned(),
                                detail: format!("pair index {other} out of bounds"),
                            });
                        }
                    },
                    other => {
                        return Err(RuntimeEvalError::TypeMismatch {
                            function: function_name.to_owned(),
                            expected: "pair",
                            found: other.kind_name(),
                        });
                    }
                };
                slots.insert(*dst, value);
            }
            KernelOp::Add { dst, lhs, rhs, .. } => {
                let lhs = self.resolve_operand(function_name, slots, lhs)?;
                let rhs = self.resolve_operand(function_name, slots, rhs)?;
                slots.insert(
                    *dst,
                    EvalValue::U64(
                        self.expect_u64(function_name, &lhs)?
                            .wrapping_add(self.expect_u64(function_name, &rhs)?),
                    ),
                );
            }
            KernelOp::NotEqual { dst, lhs, rhs, .. } => {
                let lhs = self.resolve_operand(function_name, slots, lhs)?;
                let rhs = self.resolve_operand(function_name, slots, rhs)?;
                slots.insert(*dst, EvalValue::Bool(lhs != rhs));
            }
            KernelOp::MakeArray { dst, items, .. } => {
                let items = items
                    .iter()
                    .map(|item| self.resolve_operand(function_name, slots, item))
                    .collect::<Result<Vec<_>, _>>()?;
                slots.insert(*dst, EvalValue::Array(items));
            }
            KernelOp::ConcatStrings { dst, parts, .. } => {
                let mut out = String::new();
                for part in parts {
                    let value = self.resolve_operand(function_name, slots, part)?;
                    match value {
                        EvalValue::String(value) => out.push_str(&value),
                        EvalValue::U64(value) => out.push_str(&value.to_string()),
                        EvalValue::Bool(value) => out.push_str(&value.to_string()),
                        other => {
                            return Err(RuntimeEvalError::TypeMismatch {
                                function: function_name.to_owned(),
                                expected: "string-coercible",
                                found: other.kind_name(),
                            });
                        }
                    }
                }
                slots.insert(*dst, EvalValue::String(out));
            }
            KernelOp::Select {
                dst,
                condition,
                then_value,
                else_value,
                ..
            } => {
                let condition = self.resolve_operand(function_name, slots, condition)?;
                let value = if self.expect_bool(function_name, &condition)? {
                    self.resolve_operand(function_name, slots, then_value)?
                } else {
                    self.resolve_operand(function_name, slots, else_value)?
                };
                slots.insert(*dst, value);
            }
            KernelOp::AssignFailure { dst, message, .. } => {
                slots.insert(*dst, EvalValue::Failure(message.clone()));
            }
            KernelOp::CallBody {
                dst, callee, args, ..
            } => {
                let args = args
                    .iter()
                    .map(|arg| self.resolve_operand(function_name, slots, arg))
                    .collect::<Result<Vec<_>, _>>()?;
                let value = self.eval_body(function_name, *callee, &args)?;
                slots.insert(*dst, value);
            }
            KernelOp::AssignHostLocal { .. }
            | KernelOp::AssignSandboxTreeRoot { .. }
            | KernelOp::ProjectField { .. }
            | KernelOp::CallMember { .. }
            | KernelOp::BuildTree { .. }
            | KernelOp::AwaitRuntime { .. } => {
                return Err(RuntimeEvalError::UnsupportedBoundary {
                    function: function_name.to_owned(),
                    detail: format!("op {op:?}"),
                });
            }
        }

        Ok(())
    }

    fn read_result(
        &self,
        function_name: &str,
        body: &ExecutableBody,
        slots: &BTreeMap<SlotId, EvalValue>,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match body.result_source {
            ResultSource::Slot(slot) => self.read_slot(function_name, slots, slot),
            ResultSource::PendingRuntimeBoundary => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "pending runtime boundary result".to_owned(),
            }),
        }
    }

    fn resolve_operand(
        &self,
        function_name: &str,
        slots: &BTreeMap<SlotId, EvalValue>,
        operand: &Operand,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match operand {
            Operand::Slot(slot) => self.read_slot(function_name, slots, *slot),
            Operand::Int(value) => self.int_to_value(function_name, *value),
            Operand::String(value) => Ok(EvalValue::String(value.clone())),
        }
    }

    fn read_slot(
        &self,
        function_name: &str,
        slots: &BTreeMap<SlotId, EvalValue>,
        slot: SlotId,
    ) -> Result<EvalValue, RuntimeEvalError> {
        slots
            .get(&slot)
            .cloned()
            .ok_or_else(|| RuntimeEvalError::MissingSlot {
                function: function_name.to_owned(),
                slot: slot.0,
            })
    }

    fn int_to_value(&self, function_name: &str, value: i64) -> Result<EvalValue, RuntimeEvalError> {
        u64::try_from(value)
            .map(EvalValue::U64)
            .map_err(|_| RuntimeEvalError::NegativeIntLiteral {
                function: function_name.to_owned(),
                value,
            })
    }

    fn expect_u64(&self, function_name: &str, value: &EvalValue) -> Result<u64, RuntimeEvalError> {
        match value {
            EvalValue::U64(value) => Ok(*value),
            other => Err(RuntimeEvalError::TypeMismatch {
                function: function_name.to_owned(),
                expected: "u64",
                found: other.kind_name(),
            }),
        }
    }

    fn expect_bool(
        &self,
        function_name: &str,
        value: &EvalValue,
    ) -> Result<bool, RuntimeEvalError> {
        match value {
            EvalValue::Bool(value) => Ok(*value),
            other => Err(RuntimeEvalError::TypeMismatch {
                function: function_name.to_owned(),
                expected: "bool",
                found: other.kind_name(),
            }),
        }
    }
}

impl EvalValue {
    const fn kind_name(&self) -> &'static str {
        match self {
            Self::U64(_) => "u64",
            Self::Bool(_) => "bool",
            Self::String(_) => "string",
            Self::Pair(_, _) => "pair",
            Self::Array(_) => "array",
            Self::CallableAddConst(_) => "callable",
            Self::Failure(_) => "failure",
            Self::Pending(_) => "pending",
        }
    }
}

#[cfg(test)]
mod tests {
    use vx_services::RuntimeValue;

    use super::evaluate_source_text_function;

    const SOURCE_NON_STRICT_CALL: &str = r#"
fn greet(name: String) -> String (
  `hello {name}`
)

fn build(name: String) -> String (
  greet(name)
)
"#;

    #[test]
    fn evaluates_non_strict_top_level_body_call() {
        let value = evaluate_source_text_function(
            SOURCE_NON_STRICT_CALL,
            "build",
            &[RuntimeValue::String("serde".to_owned())],
        )
        .expect("build should evaluate");

        assert_eq!(value, RuntimeValue::String("hello serde".to_owned()));
    }
}
