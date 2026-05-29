use vox::Tx;
use vx_services::{
    EvalFunctionRequest, EvalFunctionResult, EvalProgressEvent, EvalStrictFunctionFailure,
    EvalStrictFunctionRequest, EvalStrictFunctionResult, EvalStrictFunctionSuccess, Orchestrator,
    StoreClient,
};
use vx_vfsd::VfsdService;

use crate::runtime_eval::{
    EvalOutput, EvalRequest, RuntimeEvalContext, RuntimeEvalError, evaluate_source_text_function,
    runtime_error_to_diagnostics,
};
use vx_services::RuntimeValue;

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

    /// Evaluate through the real runtime path, returning the *raw* result so
    /// callers (the test runner) can distinguish a poison (`FailureResult` /
    /// `PendingResult`) from a compile error — a distinction the
    /// `EvalStrictFunctionResult` wrapper collapses into one `Err`.
    pub async fn eval_raw(
        &self,
        source: &str,
        function_name: &str,
        args: &[RuntimeValue],
    ) -> Result<EvalOutput, RuntimeEvalError> {
        self.eval_raw_observed(
            source,
            function_name,
            args,
            &mut vx_ir::compile::NoopObserver,
        )
        .await
    }

    /// Like [`eval_raw`], but reports compile phases to `observer`.
    pub async fn eval_raw_observed(
        &self,
        source: &str,
        function_name: &str,
        args: &[RuntimeValue],
        observer: &mut (dyn vx_ir::compile::CompileObserver + Send),
    ) -> Result<EvalOutput, RuntimeEvalError> {
        evaluate_source_text_function(
            &RuntimeEvalContext {
                store: self.store.clone(),
                vfsd: self.vfsd.clone(),
            },
            EvalRequest {
                source,
                function_name,
                args,
            },
            None,
            observer,
        )
        .await
    }

    async fn eval_strict_function_impl(
        &self,
        request: EvalStrictFunctionRequest,
    ) -> EvalStrictFunctionResult {
        let eval_request = EvalRequest {
            source: &request.source_text,
            function_name: &request.function_name,
            args: &request.args,
        };
        match evaluate_source_text_function(
            &RuntimeEvalContext {
                store: self.store.clone(),
                vfsd: self.vfsd.clone(),
            },
            eval_request,
            None,
            &mut vx_ir::compile::NoopObserver,
        )
        .await
        {
            Ok(output) => EvalStrictFunctionResult::Ok(EvalStrictFunctionSuccess {
                value: output.value,
            }),
            Err(error) => EvalStrictFunctionResult::Err(EvalStrictFunctionFailure {
                diagnostics: runtime_error_to_diagnostics(&error, &request.source_text, None),
            }),
        }
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
        let eval_request = EvalRequest {
            source: &request.source_text,
            function_name: &request.function_name,
            args: &request.args,
        };
        match evaluate_source_text_function(
            &RuntimeEvalContext {
                store: self.store.clone(),
                vfsd: self.vfsd.clone(),
            },
            eval_request,
            progress,
            &mut vx_ir::compile::NoopObserver,
        )
        .await
        {
            Ok(output) => EvalFunctionResult::Ok(EvalStrictFunctionSuccess {
                value: output.value,
            }),
            Err(error) => EvalFunctionResult::Err(EvalStrictFunctionFailure {
                diagnostics: runtime_error_to_diagnostics(&error, &request.source_text, None),
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
