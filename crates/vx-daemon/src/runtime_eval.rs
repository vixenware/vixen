use std::collections::{BTreeMap, HashMap};

use thiserror::Error;
use vx_analysis::{
    AnalysisError, BodyId, ExecSliceError, ExecutableClosure, ExecutableProgram, KernelOp, Operand,
    ResultSource, RuntimeBoundaryInputSpec, RuntimeBoundaryKind, RuntimeBoundaryPayload,
    RuntimeField, RuntimeMemberOp, SingleSourceBridge, SlotId, build_minimal_analysis,
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
    Host,
    Process { exit_code: u64, stdout: String },
    ProcessStdout(String),
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
        let mut slots = BTreeMap::new();
        for (slot, value) in body.params.iter().copied().zip(args.iter().cloned()) {
            slots.insert(slot, value);
        }
        let producers = body
            .ops
            .iter()
            .map(|op| (produced_slot(op), op))
            .collect::<HashMap<_, _>>();

        let value = self.read_result(
            function_name,
            body.result_source.clone(),
            &producers,
            &mut slots,
        )?;
        self.memo.insert(cache_key, value.clone());
        Ok(value)
    }

    fn eval_op(
        &mut self,
        function_name: &str,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        op: &KernelOp,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match op {
            KernelOp::AssignConstInt { value, .. } => self.int_to_value(function_name, *value),
            KernelOp::AssignConstString { value, .. } => Ok(EvalValue::String(value.clone())),
            KernelOp::AssignAlias { value, .. } => {
                self.resolve_operand(function_name, producers, slots, value)
            }
            KernelOp::AssignLazyPoison { label, .. } => Ok(EvalValue::Pending(label.clone())),
            KernelOp::AssignCallableAddConst { addend, .. } => {
                let addend = self.resolve_operand(function_name, producers, slots, addend)?;
                Ok(EvalValue::CallableAddConst(Box::new(addend)))
            }
            KernelOp::ApplyCallable { callable, arg, .. } => {
                let callable = self.resolve_operand(function_name, producers, slots, callable)?;
                let arg = self.resolve_operand(function_name, producers, slots, arg)?;
                match callable {
                    EvalValue::CallableAddConst(addend) => Ok(EvalValue::U64(
                        self.expect_u64(function_name, &arg)?
                            + self.expect_u64(function_name, &addend)?,
                    )),
                    other => Err(RuntimeEvalError::TypeMismatch {
                        function: function_name.to_owned(),
                        expected: "callable",
                        found: other.kind_name(),
                    }),
                }
            }
            KernelOp::MakePair { first, second, .. } => {
                let first = self.resolve_operand(function_name, producers, slots, first)?;
                let second = self.resolve_operand(function_name, producers, slots, second)?;
                Ok(EvalValue::Pair(Box::new(first), Box::new(second)))
            }
            KernelOp::ProjectIndex { pair, index, .. } => {
                let pair = self.resolve_operand(function_name, producers, slots, pair)?;
                match pair {
                    EvalValue::Pair(first, second) => match index {
                        0 => Ok(*first),
                        1 => Ok(*second),
                        other => Err(RuntimeEvalError::UnsupportedOperation {
                            function: function_name.to_owned(),
                            detail: format!("pair index {other} out of bounds"),
                        }),
                    },
                    other => Err(RuntimeEvalError::TypeMismatch {
                        function: function_name.to_owned(),
                        expected: "pair",
                        found: other.kind_name(),
                    }),
                }
            }
            KernelOp::Add { lhs, rhs, .. } => {
                let lhs = self.resolve_operand(function_name, producers, slots, lhs)?;
                let rhs = self.resolve_operand(function_name, producers, slots, rhs)?;
                Ok(EvalValue::U64(
                    self.expect_u64(function_name, &lhs)?
                        .wrapping_add(self.expect_u64(function_name, &rhs)?),
                ))
            }
            KernelOp::NotEqual { lhs, rhs, .. } => {
                let lhs = self.resolve_operand(function_name, producers, slots, lhs)?;
                let rhs = self.resolve_operand(function_name, producers, slots, rhs)?;
                Ok(EvalValue::Bool(lhs != rhs))
            }
            KernelOp::MakeArray { items, .. } => {
                let items = items
                    .iter()
                    .map(|item| self.resolve_operand(function_name, producers, slots, item))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(EvalValue::Array(items))
            }
            KernelOp::ConcatStrings { parts, .. } => {
                let mut out = String::new();
                for part in parts {
                    let value = self.resolve_operand(function_name, producers, slots, part)?;
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
                Ok(EvalValue::String(out))
            }
            KernelOp::StringLen { value, .. } => {
                let value = self.resolve_operand(function_name, producers, slots, value)?;
                let value = self.expect_string(function_name, &value)?;
                Ok(EvalValue::U64(value.len() as u64))
            }
            KernelOp::StringSlice {
                value, start, end, ..
            } => {
                let value = self.resolve_operand(function_name, producers, slots, value)?;
                let value = self.expect_string(function_name, &value)?;
                let start = self.resolve_operand(function_name, producers, slots, start)?;
                let end = self.resolve_operand(function_name, producers, slots, end)?;
                let start = (self.expect_u64(function_name, &start)? as usize).min(value.len());
                let end = (self.expect_u64(function_name, &end)? as usize).min(value.len());
                let start = start.min(end);
                let slice = value.get(start..end).ok_or_else(|| {
                    RuntimeEvalError::UnsupportedOperation {
                        function: function_name.to_owned(),
                        detail: format!("string slice {start}..{end} must lie on utf-8 boundaries"),
                    }
                })?;
                Ok(EvalValue::String(slice.to_owned()))
            }
            KernelOp::Select {
                condition,
                then_value,
                else_value,
                ..
            } => {
                let condition = self.resolve_operand(function_name, producers, slots, condition)?;
                if self.expect_bool(function_name, &condition)? {
                    self.resolve_operand(function_name, producers, slots, then_value)
                } else {
                    self.resolve_operand(function_name, producers, slots, else_value)
                }
            }
            KernelOp::AssignFailure { message, .. } => Ok(EvalValue::Failure(message.clone())),
            KernelOp::CallBody { callee, args, .. } => {
                let args = args
                    .iter()
                    .map(|arg| self.resolve_operand(function_name, producers, slots, arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.eval_body(function_name, *callee, &args)
            }
            KernelOp::ProjectField { value, field, .. } => {
                let value = self.resolve_operand(function_name, producers, slots, value)?;
                self.eval_project_field(function_name, value, *field)
            }
            KernelOp::CallMember {
                receiver,
                method,
                args,
                ..
            } => {
                let receiver = self.resolve_operand(function_name, producers, slots, receiver)?;
                let args = args
                    .iter()
                    .map(|arg| self.resolve_operand(function_name, producers, slots, arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.eval_member_call(function_name, receiver, *method, args)
            }
            KernelOp::AwaitRuntime {
                node,
                positional_inputs,
                coarse_mounts,
                ..
            } => self.eval_runtime_boundary(
                function_name,
                *node,
                positional_inputs,
                coarse_mounts,
                producers,
                slots,
            ),
            KernelOp::AssignHostLocal { .. }
            | KernelOp::AssignSandboxTreeRoot { .. }
            | KernelOp::BuildTree { .. } => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("op {op:?}"),
            }),
        }
    }

    fn read_result(
        &mut self,
        function_name: &str,
        result_source: ResultSource,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match result_source {
            ResultSource::Slot(slot) => self.read_slot(function_name, producers, slots, slot),
            ResultSource::PendingRuntimeBoundary => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "pending runtime boundary result".to_owned(),
            }),
        }
    }

    fn resolve_operand(
        &mut self,
        function_name: &str,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        operand: &Operand,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match operand {
            Operand::Slot(slot) => self.read_slot(function_name, producers, slots, *slot),
            Operand::Int(value) => self.int_to_value(function_name, *value),
            Operand::String(value) => Ok(EvalValue::String(value.clone())),
        }
    }

    fn read_slot(
        &mut self,
        function_name: &str,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        slot: SlotId,
    ) -> Result<EvalValue, RuntimeEvalError> {
        if let Some(value) = slots.get(&slot).cloned() {
            return Ok(value);
        }
        let op = producers
            .get(&slot)
            .copied()
            .ok_or_else(|| RuntimeEvalError::MissingSlot {
                function: function_name.to_owned(),
                slot: slot.0,
            })?;
        let value = self.eval_op(function_name, producers, slots, op)?;
        slots.insert(slot, value.clone());
        Ok(value)
    }

    fn eval_closure(
        &mut self,
        function_name: &str,
        closure: &ExecutableClosure,
        args: &[EvalValue],
    ) -> Result<EvalValue, RuntimeEvalError> {
        if closure.params.len() != args.len() {
            return Err(RuntimeEvalError::ArityMismatch {
                function: function_name.to_owned(),
                expected: closure.params.len(),
                actual: args.len(),
            });
        }

        let mut slots = BTreeMap::new();
        for (slot, value) in closure.params.iter().copied().zip(args.iter().cloned()) {
            slots.insert(slot, value);
        }
        let producers = closure
            .ops
            .iter()
            .map(|op| (produced_slot(op), op))
            .collect::<HashMap<_, _>>();
        self.read_result(
            function_name,
            closure.result_source.clone(),
            &producers,
            &mut slots,
        )
    }

    fn eval_member_call(
        &mut self,
        function_name: &str,
        receiver: EvalValue,
        method: RuntimeMemberOp,
        args: Vec<EvalValue>,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match (receiver, method) {
            (EvalValue::ProcessStdout(stdout), RuntimeMemberOp::Collect) => {
                let [] = args.as_slice() else {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "stdout.collect expects no arguments".to_owned(),
                    });
                };
                Ok(EvalValue::String(stdout))
            }
            (EvalValue::Host, RuntimeMemberOp::Which) => {
                let [name] = args.as_slice() else {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "host.which expects exactly one argument".to_owned(),
                    });
                };
                let name = self.expect_string(function_name, name)?;
                let path = find_host_tool(name).ok_or_else(|| RuntimeEvalError::FailureResult {
                    function: function_name.to_owned(),
                    message: format!("host.which could not find `{name}`"),
                })?;
                Ok(EvalValue::String(path))
            }
            (receiver, method) => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!(
                    "unsupported member call {method:?} on {}",
                    receiver.kind_name()
                ),
            }),
        }
    }

    fn eval_project_field(
        &mut self,
        function_name: &str,
        value: EvalValue,
        field: RuntimeField,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match (value, field) {
            (EvalValue::Process { exit_code, .. }, RuntimeField::ExitCode) => {
                Ok(EvalValue::U64(exit_code))
            }
            (EvalValue::Process { stdout, .. }, RuntimeField::Stdout) => {
                Ok(EvalValue::ProcessStdout(stdout))
            }
            (value, field) => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!(
                    "unsupported field projection {field:?} on {}",
                    value.kind_name()
                ),
            }),
        }
    }

    fn eval_runtime_boundary(
        &mut self,
        function_name: &str,
        node: vx_analysis::RuntimeNodeId,
        positional_inputs: &[RuntimeBoundaryInputSpec],
        coarse_mounts: &[RuntimeBoundaryInputSpec],
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<EvalValue, RuntimeEvalError> {
        if !coarse_mounts.is_empty() {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "coarse_mounts are not yet supported in local runtime eval".to_owned(),
            });
        }

        let runtime_node = self.program.runtime_nodes.get(&node).ok_or_else(|| {
            RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("missing runtime node {}", node.0),
            }
        })?;

        match (&runtime_node.kind, &runtime_node.payload) {
            (RuntimeBoundaryKind::Exec, RuntimeBoundaryPayload::Exec(_payload)) => {
                let argv = positional_inputs
                    .first()
                    .ok_or_else(|| RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "exec boundary is missing argv input".to_owned(),
                    })
                    .and_then(|input| {
                        self.resolve_exec_argv(function_name, input, producers, slots)
                    })?;
                if argv.is_empty() {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "exec argv must not be empty".to_owned(),
                    });
                }
                self.run_local_exec(function_name, &argv)
            }
            (RuntimeBoundaryKind::Discover, RuntimeBoundaryPayload::Discover(_payload)) => {
                let closure = positional_inputs
                    .iter()
                    .find_map(|input| match input {
                        RuntimeBoundaryInputSpec::Closure { closure, .. } => Some(closure),
                        _ => None,
                    })
                    .ok_or_else(|| RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "discover boundary is missing probe closure".to_owned(),
                    })?;
                self.eval_closure(function_name, closure, &[EvalValue::Host])
            }
            (kind, payload) => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("runtime boundary {kind:?} with payload {payload:?}"),
            }),
        }
    }

    fn resolve_exec_argv(
        &mut self,
        function_name: &str,
        input: &RuntimeBoundaryInputSpec,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<Vec<String>, RuntimeEvalError> {
        let RuntimeBoundaryInputSpec::Array(items) = input else {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("exec argv must be an array input, got {input:?}"),
            });
        };
        items
            .iter()
            .map(|item| self.resolve_runtime_string_input(function_name, item, producers, slots))
            .collect()
    }

    fn resolve_runtime_string_input(
        &mut self,
        function_name: &str,
        input: &RuntimeBoundaryInputSpec,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<String, RuntimeEvalError> {
        match input {
            RuntimeBoundaryInputSpec::Binding { slot, .. } => {
                let value = self.read_slot(function_name, producers, slots, *slot)?;
                Ok(self.expect_string(function_name, &value)?.to_owned())
            }
            RuntimeBoundaryInputSpec::IntLiteral(value) => Ok(value.to_string()),
            RuntimeBoundaryInputSpec::StringLiteral(value) => Ok(value.clone()),
            other => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("unsupported runtime string input {other:?}"),
            }),
        }
    }

    fn run_local_exec(
        &mut self,
        function_name: &str,
        argv: &[String],
    ) -> Result<EvalValue, RuntimeEvalError> {
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("local exec failed: {error}"),
            })?;
        Ok(EvalValue::Process {
            exit_code: output.status.code().unwrap_or(1) as u64,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
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

    fn expect_string<'b>(
        &self,
        function_name: &str,
        value: &'b EvalValue,
    ) -> Result<&'b str, RuntimeEvalError> {
        match value {
            EvalValue::String(value) => Ok(value),
            other => Err(RuntimeEvalError::TypeMismatch {
                function: function_name.to_owned(),
                expected: "string",
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
            Self::Host => "host",
            Self::Process { .. } => "process",
            Self::ProcessStdout(_) => "process-stdout",
            Self::Failure(_) => "failure",
            Self::Pending(_) => "pending",
        }
    }
}

fn produced_slot(op: &KernelOp) -> SlotId {
    match op {
        KernelOp::AssignHostLocal { dst, .. }
        | KernelOp::AssignSandboxTreeRoot { dst, .. }
        | KernelOp::AssignConstInt { dst, .. }
        | KernelOp::AssignConstString { dst, .. }
        | KernelOp::AssignAlias { dst, .. }
        | KernelOp::AssignLazyPoison { dst, .. }
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

fn find_host_tool(name: &str) -> Option<String> {
    use std::path::{Path, PathBuf};

    fn candidate_if_file(path: PathBuf) -> Option<String> {
        path.is_file().then(|| path.to_string_lossy().into_owned())
    }

    if Path::new(name).components().count() > 1 {
        return candidate_if_file(PathBuf::from(name));
    }

    let path_var = std::env::var_os("PATH")?;

    #[cfg(windows)]
    let exts = std::env::var_os("PATHEXT")
        .map(|value| {
            std::env::split_paths(&value)
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![".EXE".to_owned(), ".BAT".to_owned(), ".CMD".to_owned()]);

    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if let Some(path) = candidate_if_file(candidate) {
            return Some(path);
        }
        #[cfg(windows)]
        for ext in &exts {
            if let Some(path) = candidate_if_file(dir.join(format!("{name}{ext}"))) {
                return Some(path);
            }
        }
    }

    None
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

    const SOURCE_NON_STRICT_STRING_HELPERS: &str = r#"
fn measure(name: String) -> U64 (
  name.len()
)

fn prefix(name: String) -> String (
  name.slice(0..2)
)

fn build_len(name: String) -> U64 (
  measure(name)
)

fn build_slice(name: String) -> String (
  prefix(name)
)
"#;

    const SOURCE_DISCOVER_WHICH: &str = r#"
fn probe() #discover -> String (
  discover(executor_class("default"), (host) => host.which("sh"))
)
"#;

    const SOURCE_EXEC_STDOUT: &str = r#"
fn probe() #discover -> String (
  let sh = discover(executor_class("default"), (host) => host.which("sh"));
  exec [sh, "-c", "printf hello"] where {
    observer: (proc, _vfs) => proc.stdout.collect()
  }
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

    #[test]
    fn evaluates_non_strict_string_len_via_local_call() {
        let value = evaluate_source_text_function(
            SOURCE_NON_STRICT_STRING_HELPERS,
            "build_len",
            &[RuntimeValue::String("serde".to_owned())],
        )
        .expect("build_len should evaluate");

        assert_eq!(value, RuntimeValue::U64(5));
    }

    #[test]
    fn evaluates_non_strict_string_slice_via_local_call() {
        let value = evaluate_source_text_function(
            SOURCE_NON_STRICT_STRING_HELPERS,
            "build_slice",
            &[RuntimeValue::String("serde".to_owned())],
        )
        .expect("build_slice should evaluate");

        assert_eq!(value, RuntimeValue::String("se".to_owned()));
    }

    #[test]
    fn evaluates_local_discover_host_which() {
        let value = evaluate_source_text_function(SOURCE_DISCOVER_WHICH, "probe", &[])
            .expect("probe should evaluate");

        match value {
            RuntimeValue::String(path) => {
                assert!(path.starts_with('/'), "expected absolute path, got {path}");
                assert!(path.ends_with("sh"), "expected shell path, got {path}");
            }
            other => panic!("expected string path, got {other:?}"),
        }
    }

    #[test]
    fn evaluates_local_exec_stdout_observer() {
        let value = evaluate_source_text_function(SOURCE_EXEC_STDOUT, "probe", &[])
            .expect("probe should evaluate");

        assert_eq!(value, RuntimeValue::String("hello".to_owned()));
    }
}
