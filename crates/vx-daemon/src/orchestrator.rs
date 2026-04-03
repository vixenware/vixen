use vx_jit::{
    KernelType, KernelValue, compile_source_text_function, interpret_source_text_function,
    lower_source_text_hir, lower_source_text_ir, lower_source_text_kernel,
};
use vx_services::{
    EvalStrictFunctionFailure, EvalStrictFunctionRequest, EvalStrictFunctionResult,
    EvalStrictFunctionSuccess, Orchestrator, StrictEvalBackend, StrictValue,
};

#[derive(Clone, Default)]
pub struct OrchestratorService;

impl OrchestratorService {
    pub fn new() -> Self {
        Self
    }

    async fn eval_strict_function_impl(
        &self,
        request: EvalStrictFunctionRequest,
    ) -> EvalStrictFunctionResult {
        let kernel = match lower_source_text_kernel(&request.source_text, &request.function_name) {
            Ok(kernel) => kernel,
            Err(error) => {
                return EvalStrictFunctionResult::Err(failure(request, error.to_string()));
            }
        };

        let args = match request
            .args
            .iter()
            .cloned()
            .map(kernel_value_from_rpc)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => args,
            Err(message) => return EvalStrictFunctionResult::Err(failure(request, message)),
        };

        let kernel_dump = request.include_kernel_dump.then(|| format!("{kernel:#?}"));
        let hir = if request.include_hir {
            match lower_source_text_hir(&request.source_text, &request.function_name) {
                Ok(hir) => Some(hir.to_string()),
                Err(error) => {
                    return EvalStrictFunctionResult::Err(failure_with_dump(
                        kernel_dump,
                        format!("failed to lower HIR: {error}"),
                    ));
                }
            }
        } else {
            None
        };
        let ir = if request.include_ir {
            match lower_source_text_ir(&request.source_text, &request.function_name) {
                Ok(ir) => Some(ir.to_string()),
                Err(error) => {
                    return EvalStrictFunctionResult::Err(EvalStrictFunctionFailure {
                        message: format!("failed to lower IR: {error}"),
                        kernel_dump,
                        hir,
                        ir: None,
                    });
                }
            }
        } else {
            None
        };

        let value = match request.backend {
            StrictEvalBackend::Interpreter => match interpret_source_text_function(
                &request.source_text,
                &request.function_name,
                &args,
            ) {
                Ok(value) => value,
                Err(error) => {
                    return EvalStrictFunctionResult::Err(EvalStrictFunctionFailure {
                        message: format!("interpreter failed: {error}"),
                        kernel_dump,
                        hir,
                        ir,
                    });
                }
            },
            StrictEvalBackend::Kajit => match execute_with_kajit(&kernel, &request, &args) {
                Ok(value) => value,
                Err(message) => {
                    return EvalStrictFunctionResult::Err(EvalStrictFunctionFailure {
                        message,
                        kernel_dump,
                        hir,
                        ir,
                    });
                }
            },
        };

        EvalStrictFunctionResult::Ok(EvalStrictFunctionSuccess {
            value: rpc_value_from_kernel(value),
            kernel_dump,
            hir,
            ir,
        })
    }
}

impl Orchestrator for OrchestratorService {
    async fn eval_strict_function(
        &self,
        request: EvalStrictFunctionRequest,
    ) -> EvalStrictFunctionResult {
        self.eval_strict_function_impl(request).await
    }
}

fn failure(_request: EvalStrictFunctionRequest, message: String) -> EvalStrictFunctionFailure {
    failure_with_dump(None, message)
}

fn failure_with_dump(kernel_dump: Option<String>, message: String) -> EvalStrictFunctionFailure {
    EvalStrictFunctionFailure {
        message,
        kernel_dump,
        hir: None,
        ir: None,
    }
}

fn kernel_value_from_rpc(value: StrictValue) -> Result<KernelValue, String> {
    Ok(match value {
        StrictValue::U64(value) => KernelValue::U64(value),
        StrictValue::Bool(value) => KernelValue::Bool(value),
        StrictValue::String(value) => KernelValue::String(value),
    })
}

fn rpc_value_from_kernel(value: KernelValue) -> StrictValue {
    match value {
        KernelValue::U64(value) => StrictValue::U64(value),
        KernelValue::Bool(value) => StrictValue::Bool(value),
        KernelValue::String(value) => StrictValue::String(value),
    }
}

fn execute_with_kajit(
    kernel: &vx_jit::KernelFunction,
    request: &EvalStrictFunctionRequest,
    args: &[KernelValue],
) -> Result<KernelValue, String> {
    let compiled = compile_source_text_function(&request.source_text, &request.function_name)
        .map_err(|error| format!("Kajit compile failed: {error}"))?;

    match (kernel.return_type, kernel.params.as_slice()) {
        (KernelType::U64, []) => {
            let f: unsafe extern "C" fn() -> u64 = unsafe { core::mem::transmute(compiled.as_ptr()) };
            Ok(KernelValue::U64(unsafe { f() }))
        }
        (KernelType::U64, [a]) if a.ty == KernelType::U64 => {
            let [KernelValue::U64(a0)] = args else {
                return Err("Kajit backend expected one U64 argument".to_owned());
            };
            let f: unsafe extern "C" fn(u64) -> u64 =
                unsafe { core::mem::transmute(compiled.as_ptr()) };
            Ok(KernelValue::U64(unsafe { f(*a0) }))
        }
        (KernelType::U64, [a]) if a.ty == KernelType::String => {
            let [KernelValue::String(a0)] = args else {
                return Err("Kajit backend expected one String argument".to_owned());
            };
            let f: unsafe extern "C" fn(u64, u64) -> u64 =
                unsafe { core::mem::transmute(compiled.as_ptr()) };
            Ok(KernelValue::U64(unsafe { f(a0.as_ptr() as u64, a0.len() as u64) }))
        }
        (KernelType::U64, [a, b]) if a.ty == KernelType::U64 && b.ty == KernelType::U64 => {
            let [KernelValue::U64(a0), KernelValue::U64(a1)] = args else {
                return Err("Kajit backend expected two U64 arguments".to_owned());
            };
            let f: unsafe extern "C" fn(u64, u64) -> u64 =
                unsafe { core::mem::transmute(compiled.as_ptr()) };
            Ok(KernelValue::U64(unsafe { f(*a0, *a1) }))
        }
        (KernelType::String, []) => Err(
            "Kajit backend string returns are not yet exposed safely over RPC because ownership is unresolved"
                .to_owned(),
        ),
        (KernelType::String, [a]) if a.ty == KernelType::U64 => Err(
            "Kajit backend string returns are not yet exposed safely over RPC because ownership is unresolved"
                .to_owned(),
        ),
        (KernelType::String, [a]) if a.ty == KernelType::String => Err(
            "Kajit backend string returns are not yet exposed safely over RPC because ownership is unresolved"
                .to_owned(),
        ),
        (KernelType::String, [a, b])
            if a.ty == KernelType::String && b.ty == KernelType::String =>
        {
            Err(
                "Kajit backend string returns are not yet exposed safely over RPC because ownership is unresolved"
                    .to_owned(),
            )
        }
        _ => Err(format!(
            "Kajit backend does not yet support RPC execution for signature {:?} -> {:?}",
            kernel.params.iter().map(|param| param.ty).collect::<Vec<_>>(),
            kernel.return_type
        )),
    }
}

#[cfg(test)]
mod tests {
    use vx_services::{
        EvalStrictFunctionRequest, EvalStrictFunctionResult, Orchestrator, StrictEvalBackend,
        StrictValue,
    };

    use super::OrchestratorService;

    const SOURCE_ADD_U64: &str = r#"
fn add(lhs: U64, rhs: U64) -> U64 (
  lhs + rhs
)
"#;

    const SOURCE_STR_LEN: &str = r#"
fn str_len(name: String) -> U64 (
  name.len()
)
"#;

    const SOURCE_ONE: &str = r#"
fn one() -> String (
  "1"
)
"#;

    #[tokio::test]
    async fn evals_u64_with_kajit() {
        let service = OrchestratorService::new();
        let result = service
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: SOURCE_ADD_U64.to_owned(),
                function_name: "add".to_owned(),
                args: vec![StrictValue::U64(30), StrictValue::U64(12)],
                backend: StrictEvalBackend::Kajit,
                include_kernel_dump: false,
                include_hir: false,
                include_ir: false,
            })
            .await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, StrictValue::U64(42));
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn evals_string_with_interpreter() {
        let service = OrchestratorService::new();
        let result = service
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: SOURCE_ONE.to_owned(),
                function_name: "one".to_owned(),
                args: vec![],
                backend: StrictEvalBackend::Interpreter,
                include_kernel_dump: false,
                include_hir: false,
                include_ir: false,
            })
            .await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, StrictValue::String("1".to_owned()));
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lowers_hir_and_ir_when_requested() {
        let service = OrchestratorService::new();
        let result = service
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: SOURCE_STR_LEN.to_owned(),
                function_name: "str_len".to_owned(),
                args: vec![StrictValue::String("serde".to_owned())],
                backend: StrictEvalBackend::Interpreter,
                include_kernel_dump: true,
                include_hir: true,
                include_ir: true,
            })
            .await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, StrictValue::U64(5));
                assert!(
                    success
                        .kernel_dump
                        .as_deref()
                        .unwrap_or_default()
                        .contains("KernelFunction")
                );
                assert!(
                    success
                        .hir
                        .as_deref()
                        .unwrap_or_default()
                        .contains("function")
                );
                assert!(success.ir.as_deref().unwrap_or_default().contains("lambda"));
            }
            other => panic!("expected success, got {other:?}"),
        }
    }
}
