//! Fresh analysis-output skeleton for the eval restart.
//!
//! This crate intentionally models analysis decisions only.
//! It does not model lowering or backend concerns.

mod model;
pub use model::{
    AnalysisError, AnalysisResult, AnalyzedBody, BodyId, CaptureSlot, ClosureEnvLayout, ExprId,
    ExtractedNode, ExtractedNodeReason, FieldKey, FrameLayout, KernelEntrypoint, KernelId,
    KernelParam, KernelRegion, LayoutId, Observation, RuntimeBoundaryKind, RuntimeNode,
    RuntimeNodeId, SharedKeyedNode, SourceExpr, UseSiteId,
};

mod bridge;
pub use bridge::{
    BridgeBindingProduct, BridgeBody, BridgeCallSite, BridgeCallSiteKind, BridgeExpr,
    BridgeExprKind, BridgeLocalBinding, BridgeLocalBindingId, BridgeLocalBindingKind,
    BridgeLocalUse, BridgeParam, BridgeProduct, BridgeProjection, BridgeResolvedRef,
    SingleSourceBridge,
};

mod boundary;

mod bridge_to_analysis;
pub use bridge_to_analysis::build_minimal_analysis;

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bridge_tests;

#[cfg(test)]
mod bridge_to_analysis_tests;
