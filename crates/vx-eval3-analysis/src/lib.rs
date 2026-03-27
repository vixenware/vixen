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

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests;
