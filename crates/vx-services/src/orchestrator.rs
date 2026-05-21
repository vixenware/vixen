//! Orchestrator service protocol definitions.
//!
//! This is the top-level evaluation/scheduling service surface. It is distinct
//! from the tiny `Daemon` host-introspection service.

use std::collections::BTreeMap;

use crate::NodeHash;
use facet::Facet;
use thiserror::Error;
use vox::Tx;

/// Orchestrator protocol version.
pub const ORCHESTRATOR_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum StrictEvalBackend {
    Interpreter,
    Cranelift,
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
    pub include_clif_dump: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct EvalStrictFunctionSuccess {
    pub value: RuntimeValue,
    pub strict_fragment_dump: Option<String>,
    pub clif_dump: Option<String>,
    pub jit_report: Option<JitReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct EvalStrictFunctionFailure {
    pub message: String,
    pub strict_fragment_dump: Option<String>,
    pub clif_dump: Option<String>,
    pub jit_report: Option<JitReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct JitReport {
    pub entries: Vec<JitReportEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct JitReportEntry {
    pub body_id: u32,
    pub outcome: JitReportOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum JitReportOutcome {
    Compiled { name: String },
    Skipped { reason: JitSkipReason },
    Failed { reason: JitFailureReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Facet, Error)]
#[repr(C)]
pub enum JitSkipReason {
    #[error("synthesized body")]
    SynthesizedBody,
    #[error("non-strict parameter types: {params:?}")]
    NonStrictParameterTypes { params: Vec<JitUnsupportedParam> },
    #[error("runtime arguments are not representable in the JIT ABI")]
    RuntimeArgumentsNotRepresentable,
    #[error("unsupported operation: {op}")]
    UnsupportedOp { op: JitUnsupportedOp },
    #[error("not strict-lowerable")]
    NotStrictLowerable,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct JitUnsupportedParam {
    pub slot: u32,
    pub ty: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet, Error)]
#[repr(C)]
pub enum JitUnsupportedOp {
    #[error("runtime boundary")]
    RuntimeBoundary,
    #[error("tree construction")]
    BuildTree,
    #[error("array construction")]
    MakeArray,
    #[error("blob construction")]
    MakeBlob,
    #[error("thunk allocation")]
    AllocThunk,
    #[error("closure allocation")]
    AllocClosure,
    #[error("closure application")]
    ApplyClosure,
    #[error("struct construction")]
    MakeStruct,
    #[error("named field projection")]
    ProjectNamedField,
    #[error("failure construction")]
    AssignFailure,
    #[error("body call")]
    CallBody,
    #[error("member call")]
    CallMember,
    #[error("runtime field projection")]
    ProjectField,
    #[error("sandbox tree root")]
    AssignSandboxTreeRoot,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet, Error)]
#[repr(C)]
pub enum JitFailureReason {
    #[error("compile failed: {detail}")]
    Compile { detail: String },
    #[error("execute failed: {detail}")]
    Execute { detail: String },
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
