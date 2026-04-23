use vx_jit::{FragmentArg, FragmentResult};
use vx_rt::RtValue;

/// A scalar value that both the interpreter and Cranelift backends can produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarResult {
    U64(u64),
    Bool(bool),
    String(String),
}

#[derive(Debug)]
pub enum EvalError {
    Lowering(String),
    Interpreter(String),
    Cranelift(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::Lowering(e) => write!(f, "lowering: {e}"),
            EvalError::Interpreter(e) => write!(f, "interpreter: {e}"),
            EvalError::Cranelift(e) => write!(f, "cranelift: {e}"),
        }
    }
}

/// Evaluate `source` at `function_name` with `args` using the interpreter.
///
/// Returns `None` if the program uses features the interpreter doesn't support
/// (e.g. runtime boundaries), rather than an error.
pub fn eval_interpreter(
    source: &str,
    function_name: &str,
    args: &[ScalarResult],
) -> Result<Option<ScalarResult>, EvalError> {
    let rt_args: Vec<RtValue> = args.iter().map(scalar_to_rt).collect();
    match vx_rt::eval_source(source, function_name, &rt_args) {
        Ok(val) => rt_to_scalar(val).map(Some).map_err(EvalError::Interpreter),
        Err(vx_rt::RtError::Unsupported(_)) => Ok(None),
        Err(e) => Err(EvalError::Interpreter(e.to_string())),
    }
}

/// Evaluate `source` at `function_name` with `args` using the Cranelift backend.
///
/// Returns `None` if the function cannot be strict-lowered (i.e. it is not
/// strict-compatible — uses lazy features, runtime boundaries, etc.).
pub fn eval_cranelift(
    source: &str,
    function_name: &str,
    args: &[ScalarResult],
) -> Result<Option<ScalarResult>, EvalError> {
    let frag_args: Vec<FragmentArg> = args.iter().map(scalar_to_frag).collect();

    let output = vx_vir::compile::compile_source_with_types(source, function_name)
        .map_err(|e| EvalError::Lowering(e.to_string()))?;

    let entry = output.program.entry;
    let body = output
        .program
        .bodies
        .get(&entry)
        .ok_or_else(|| EvalError::Lowering("missing entry body".to_owned()))?;

    let mut func = match vx_jit::lower_to_strict::try_strict_lower(
        body,
        &output.slot_types,
        output.original_body_count,
    ) {
        Some(f) => f,
        None => return Ok(None),
    };
    func.name = function_name.to_owned();

    let compiled = vx_jit::compile(&func).map_err(|e| EvalError::Cranelift(e.to_string()))?;

    let result = compiled
        .call(&frag_args)
        .map_err(|e| EvalError::Cranelift(e.to_string()))?;

    fragment_to_scalar(result)
        .map(Some)
        .map_err(EvalError::Cranelift)
}

fn scalar_to_rt(v: &ScalarResult) -> RtValue {
    match v {
        ScalarResult::U64(n) => RtValue::U64(*n),
        ScalarResult::Bool(b) => RtValue::Bool(*b),
        ScalarResult::String(s) => RtValue::String(s.clone()),
    }
}

fn scalar_to_frag(v: &ScalarResult) -> FragmentArg {
    match v {
        ScalarResult::U64(n) => FragmentArg::U64(*n),
        ScalarResult::Bool(b) => FragmentArg::Bool(*b),
        ScalarResult::String(s) => FragmentArg::String(s.clone()),
    }
}

fn rt_to_scalar(val: RtValue) -> Result<ScalarResult, String> {
    match val {
        RtValue::U64(n) => Ok(ScalarResult::U64(n)),
        RtValue::Bool(b) => Ok(ScalarResult::Bool(b)),
        RtValue::String(s) => Ok(ScalarResult::String(s)),
        other => Err(format!("interpreter returned non-scalar: {other:?}")),
    }
}

fn fragment_to_scalar(val: FragmentResult) -> Result<ScalarResult, String> {
    Ok(match val {
        FragmentResult::U64(n) => ScalarResult::U64(n),
        FragmentResult::Bool(b) => ScalarResult::Bool(b),
        FragmentResult::String(s) => ScalarResult::String(s),
    })
}
