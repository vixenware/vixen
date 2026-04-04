//! Orchestrator service protocol definitions.
//!
//! This is the top-level evaluation/scheduling service surface. It is distinct
//! from the tiny `Daemon` host-introspection service.

use std::collections::BTreeMap;

use crate::NodeHash;
use facet::Facet;
use vox::Tx;

/// Orchestrator protocol version.
pub const ORCHESTRATOR_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum StrictEvalBackend {
    Interpreter,
    Kajit,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum RuntimeValue {
    U64(u64),
    Bool(bool),
    String(String),
    Path(String),
    StringArray(Vec<String>),
    StringMap(BTreeMap<String, String>),
    Tree(NodeHash),
}

#[derive(Debug, Clone, Facet)]
pub struct EvalStrictFunctionRequest {
    pub source_text: String,
    pub function_name: String,
    pub args: Vec<RuntimeValue>,
    pub backend: StrictEvalBackend,
    pub include_strict_fragment_dump: bool,
    pub include_hir: bool,
    pub include_ir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct EvalStrictFunctionSuccess {
    pub value: RuntimeValue,
    pub strict_fragment_dump: Option<String>,
    pub hir: Option<String>,
    pub ir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct EvalStrictFunctionFailure {
    pub message: String,
    pub strict_fragment_dump: Option<String>,
    pub hir: Option<String>,
    pub ir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum EvalStrictFunctionResult {
    Ok(EvalStrictFunctionSuccess),
    Err(EvalStrictFunctionFailure),
}

pub type EvalFunctionRequest = EvalStrictFunctionRequest;
pub type EvalFunctionSuccess = EvalStrictFunctionSuccess;
pub type EvalFunctionFailure = EvalStrictFunctionFailure;
pub type EvalFunctionResult = EvalStrictFunctionResult;
pub type EvalProgressEvent = String;

#[vox::service]
pub trait Orchestrator {
    async fn eval_function(&self, request: EvalFunctionRequest) -> EvalFunctionResult;

    async fn eval_function_with_progress(
        &self,
        request: EvalFunctionRequest,
        progress: Tx<EvalProgressEvent>,
    ) -> EvalFunctionResult;

    async fn eval_strict_function(
        &self,
        request: EvalStrictFunctionRequest,
    ) -> EvalStrictFunctionResult;
}
