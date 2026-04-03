use vx_jit::{
    StrictFragmentValue, compile_strict_fragment, execute_compiled_copying_strict_fragment_strings,
    interpret_source_text_function, lower_source_text_hir, lower_source_text_ir,
    lower_source_text_strict_fragment,
};
use vx_services::{
    EvalStrictFunctionFailure, EvalStrictFunctionRequest, EvalStrictFunctionResult,
    EvalStrictFunctionSuccess, Orchestrator, RuntimeValue, StrictEvalBackend,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrchestratorWiring {
    pub store_addr: Option<String>,
    pub runner_addrs: Vec<String>,
    pub vfsd_addr: Option<String>,
}

#[derive(Clone, Default)]
pub struct OrchestratorService {
    wiring: OrchestratorWiring,
}

impl OrchestratorService {
    pub fn new(wiring: OrchestratorWiring) -> Self {
        Self { wiring }
    }

    pub fn wiring(&self) -> &OrchestratorWiring {
        &self.wiring
    }

    async fn eval_strict_function_impl(
        &self,
        request: EvalStrictFunctionRequest,
    ) -> EvalStrictFunctionResult {
        let strict_fragment =
            match lower_source_text_strict_fragment(&request.source_text, &request.function_name) {
                Ok(strict_fragment) => strict_fragment,
                Err(error) => {
                    return EvalStrictFunctionResult::Err(failure(request, error.to_string()));
                }
            };

        let args = match request
            .args
            .iter()
            .cloned()
            .map(strict_fragment_value_from_runtime)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => args,
            Err(message) => return EvalStrictFunctionResult::Err(failure(request, message)),
        };

        let strict_fragment_dump = request
            .include_strict_fragment_dump
            .then(|| format!("{strict_fragment:#?}"));
        let hir = if request.include_hir {
            match lower_source_text_hir(&request.source_text, &request.function_name) {
                Ok(hir) => Some(hir.to_string()),
                Err(error) => {
                    return EvalStrictFunctionResult::Err(failure_with_dump(
                        strict_fragment_dump,
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
                        strict_fragment_dump,
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
                        strict_fragment_dump,
                        hir,
                        ir,
                    });
                }
            },
            StrictEvalBackend::Kajit => match execute_with_kajit(&strict_fragment, &args) {
                Ok(value) => value,
                Err(message) => {
                    return EvalStrictFunctionResult::Err(EvalStrictFunctionFailure {
                        message,
                        strict_fragment_dump,
                        hir,
                        ir,
                    });
                }
            },
        };

        EvalStrictFunctionResult::Ok(EvalStrictFunctionSuccess {
            value: runtime_value_from_strict_fragment(value),
            strict_fragment_dump,
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

fn failure_with_dump(
    strict_fragment_dump: Option<String>,
    message: String,
) -> EvalStrictFunctionFailure {
    EvalStrictFunctionFailure {
        message,
        strict_fragment_dump,
        hir: None,
        ir: None,
    }
}

fn strict_fragment_value_from_runtime(value: RuntimeValue) -> Result<StrictFragmentValue, String> {
    Ok(match value {
        RuntimeValue::U64(value) => StrictFragmentValue::U64(value),
        RuntimeValue::Bool(value) => StrictFragmentValue::Bool(value),
        RuntimeValue::String(value) => StrictFragmentValue::String(value),
    })
}

fn runtime_value_from_strict_fragment(value: StrictFragmentValue) -> RuntimeValue {
    match value {
        StrictFragmentValue::U64(value) => RuntimeValue::U64(value),
        StrictFragmentValue::Bool(value) => RuntimeValue::Bool(value),
        StrictFragmentValue::String(value) => RuntimeValue::String(value),
    }
}

fn execute_with_kajit(
    strict_fragment: &vx_jit::StrictFragmentFunction,
    args: &[StrictFragmentValue],
) -> Result<StrictFragmentValue, String> {
    let compiled = compile_strict_fragment(strict_fragment)
        .map_err(|error| format!("Kajit compile failed: {error}"))?;

    unsafe { execute_compiled_copying_strict_fragment_strings(strict_fragment, &compiled, args) }
        .map_err(|error| format!("Kajit execution failed: {error}"))
}

#[cfg(test)]
mod tests {
    use vx_services::{
        EvalStrictFunctionRequest, EvalStrictFunctionResult, Orchestrator, RuntimeValue,
        StrictEvalBackend,
    };

    use super::{OrchestratorService, OrchestratorWiring};

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
        let service = OrchestratorService::new(OrchestratorWiring::default());
        let result = service
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: SOURCE_ADD_U64.to_owned(),
                function_name: "add".to_owned(),
                args: vec![RuntimeValue::U64(30), RuntimeValue::U64(12)],
                backend: StrictEvalBackend::Kajit,
                include_strict_fragment_dump: false,
                include_hir: false,
                include_ir: false,
            })
            .await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, RuntimeValue::U64(42));
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn evals_string_with_interpreter() {
        let service = OrchestratorService::new(OrchestratorWiring::default());
        let result = service
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: SOURCE_ONE.to_owned(),
                function_name: "one".to_owned(),
                args: vec![],
                backend: StrictEvalBackend::Interpreter,
                include_strict_fragment_dump: false,
                include_hir: false,
                include_ir: false,
            })
            .await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, RuntimeValue::String("1".to_owned()));
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn evals_string_with_kajit() {
        let service = OrchestratorService::new(OrchestratorWiring::default());
        let result = service
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: SOURCE_ONE.to_owned(),
                function_name: "one".to_owned(),
                args: vec![],
                backend: StrictEvalBackend::Kajit,
                include_strict_fragment_dump: false,
                include_hir: false,
                include_ir: false,
            })
            .await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, RuntimeValue::String("1".to_owned()));
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lowers_hir_and_ir_when_requested() {
        let service = OrchestratorService::new(OrchestratorWiring::default());
        let result = service
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: SOURCE_STR_LEN.to_owned(),
                function_name: "str_len".to_owned(),
                args: vec![RuntimeValue::String("serde".to_owned())],
                backend: StrictEvalBackend::Interpreter,
                include_strict_fragment_dump: true,
                include_hir: true,
                include_ir: true,
            })
            .await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, RuntimeValue::U64(5));
                assert!(
                    success
                        .strict_fragment_dump
                        .as_deref()
                        .unwrap_or_default()
                        .contains("StrictFragmentFunction")
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

    #[test]
    fn retains_resolved_wiring() {
        let wiring = OrchestratorWiring {
            store_addr: Some("tcp://store-a:5000".to_owned()),
            runner_addrs: vec![
                "tcp://runner-a:5000".to_owned(),
                "tcp://runner-b:5000".to_owned(),
            ],
            vfsd_addr: Some("local:///tmp/vx-vfsd.vox".to_owned()),
        };
        let service = OrchestratorService::new(wiring.clone());
        assert_eq!(service.wiring(), &wiring);
    }
}
