//! Legacy analysis pipeline — moved from vx-analysis.
//!
//! Used only by the strict (Kajit) JIT compilation path.
//! Will be replaced when StrictLowerer is rewritten to work on VIR.

mod model;
pub use model::{
    AnalysisError, AnalysisResult, AnalyzedBody, BodyId, CaptureSlot, ClosureEnvLayout, ExprId,
    ExtractedNode, ExtractedNodeReason, FieldKey, FrameLayout, KernelEntrypoint, KernelId,
    KernelParam, KernelRegion, LayoutId, Observation, RuntimeBoundaryKind, RuntimeNode,
    RuntimeNodeId, SharedKeyedNode, SourceExpr, SourceLocalCallEdge, SourceLocalCalleeOrigin,
    UseSiteId,
};

mod bridge;
pub use bridge::{
    BridgeBindingProduct, BridgeBody, BridgeBoundaryArgBinding, BridgeBoundaryArgClosure,
    BridgeCallSite, BridgeCallSiteKind, BridgeExpr, BridgeExprKind, BridgeLocalBinding,
    BridgeLocalBindingId, BridgeLocalBindingKind, BridgeLocalBoundaryInit, BridgeLocalClosureInit,
    BridgeLocalUse, BridgeParam, BridgeProduct, BridgeProjection, BridgeResolvedRef,
    SingleSourceBridge,
};

mod boundary;

mod bridge_to_analysis;
pub use bridge_to_analysis::build_minimal_analysis;

pub mod lower;
pub mod lower_strict;

mod exec_slice;
pub use exec_slice::{
    DiscoverBoundaryPayload, ExecBoundaryPayload, ExecSliceError, ExecutableBody,
    ExecutableClosure, ExecutableProgram, ExecutableRuntimeNode, KernelKind, KernelOp, Operand,
    ResultSource, RuntimeBoundaryInputSpec, RuntimeBoundaryPayload, RuntimeField, RuntimeMemberOp,
    SharedKeyedBoundaryPayload, SlotId, TreeFilePath, lower_executable_program,
    lower_executable_program_for_entry_body,
};
