use vox::Tx;
use vx_jit::{
    StrictFragmentClassification, StrictFragmentValue, classify_source_text_function,
    compile_strict_fragment, execute_compiled_copying_strict_fragment_strings,
    interpret_source_text_function, lower_source_text_hir, lower_source_text_ir,
    lower_source_text_strict_fragment,
};
use vx_services::{
    EvalFunctionRequest, EvalFunctionResult, EvalProgressEvent, EvalStrictFunctionFailure,
    EvalStrictFunctionRequest, EvalStrictFunctionResult, EvalStrictFunctionSuccess, Orchestrator,
    RuntimeValue, StoreClient, StrictEvalBackend,
};
use vx_vfsd::VfsdService;

use crate::runtime_eval::{RuntimeEvalContext, evaluate_source_text_function};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrchestratorWiring {
    pub store_addr: Option<String>,
    pub runner_addrs: Vec<String>,
    pub vfsd_addr: Option<String>,
}

#[derive(Clone, Default)]
pub struct OrchestratorService {
    wiring: OrchestratorWiring,
    store: Option<StoreClient>,
    vfsd: Option<VfsdService>,
}

impl OrchestratorService {
    pub fn new(wiring: OrchestratorWiring) -> Self {
        Self {
            wiring,
            store: None,
            vfsd: None,
        }
    }

    pub fn with_runtime(
        wiring: OrchestratorWiring,
        store: Option<StoreClient>,
        vfsd: Option<VfsdService>,
    ) -> Self {
        Self {
            wiring,
            store,
            vfsd,
        }
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
                    if error.describes_non_strict_shape() {
                        let function_name = request.function_name.clone();
                        let reason = match classify_source_text_function(
                            &request.source_text,
                            &function_name,
                        ) {
                            Ok(StrictFragmentClassification::NotStrict { reason }) => reason,
                            Ok(StrictFragmentClassification::StrictFragment) | Err(_) => {
                                error.to_string()
                            }
                        };
                        return EvalStrictFunctionResult::Err(failure(
                            request,
                            format!(
                                "function `{}` is not a strict fragment: {reason}",
                                function_name
                            ),
                        ));
                    }
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

    async fn eval_function_impl(&self, request: EvalFunctionRequest) -> EvalFunctionResult {
        self.eval_function_with_progress_impl(request, None).await
    }

    async fn eval_function_with_progress_impl(
        &self,
        request: EvalFunctionRequest,
        progress: Option<Tx<EvalProgressEvent>>,
    ) -> EvalFunctionResult {
        emit_phase(
            progress.as_ref(),
            format!("classifying `{}`", request.function_name),
        )
        .await;
        emit_phase(
            progress.as_ref(),
            format!("evaluating `{}`", request.function_name),
        )
        .await;
        match evaluate_source_text_function(
            &RuntimeEvalContext {
                store: self.store.clone(),
                vfsd: self.vfsd.clone(),
            },
            &request.source_text,
            &request.function_name,
            &request.args,
            request.backend,
            progress,
        )
        .await
        {
            Ok(value) => EvalFunctionResult::Ok(EvalStrictFunctionSuccess {
                value,
                strict_fragment_dump: None,
                hir: None,
                ir: None,
            }),
            Err(error) => EvalFunctionResult::Err(EvalStrictFunctionFailure {
                message: error.to_string(),
                strict_fragment_dump: None,
                hir: None,
                ir: None,
            }),
        }
    }
}

impl Orchestrator for OrchestratorService {
    async fn eval_function(&self, request: EvalFunctionRequest) -> EvalFunctionResult {
        self.eval_function_impl(request).await
    }

    async fn eval_function_with_progress(
        &self,
        request: EvalFunctionRequest,
        progress: Tx<EvalProgressEvent>,
    ) -> EvalFunctionResult {
        // The handler owns the sender for the lifetime of the request. Dropping it
        // closes the progress stream after the terminal result has been sent.
        self.eval_function_with_progress_impl(request, Some(progress))
            .await
    }

    async fn eval_strict_function(
        &self,
        request: EvalStrictFunctionRequest,
    ) -> EvalStrictFunctionResult {
        self.eval_strict_function_impl(request).await
    }
}

async fn emit_phase(progress: Option<&Tx<EvalProgressEvent>>, message: String) {
    if let Some(progress) = progress {
        let _ = progress.send(format!("phase: {message}\n")).await;
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
        RuntimeValue::Path(_) => {
            return Err("strict fragments do not accept Path arguments yet".to_owned());
        }
        RuntimeValue::StringArray(_) => {
            return Err("strict fragments do not accept Array[String] arguments yet".to_owned());
        }
        RuntimeValue::StringMap(_) => {
            return Err(
                "strict fragments do not accept Map[String, String] arguments yet".to_owned(),
            );
        }
        RuntimeValue::Tree(_) => {
            return Err("strict fragments do not accept Tree arguments yet".to_owned());
        }
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
