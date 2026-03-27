//! Fresh analysis-output skeleton for the eval restart.
//!
//! This crate intentionally models analysis decisions only.
//! It does not model lowering or backend concerns.

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
    BridgeBindingProduct, BridgeBody, BridgeCallSite, BridgeCallSiteKind, BridgeExpr,
    BridgeExprKind, BridgeLocalBinding, BridgeLocalBindingId, BridgeLocalBindingKind,
    BridgeLocalClosureInit, BridgeLocalUse, BridgeParam, BridgeProduct, BridgeProjection,
    BridgeResolvedRef, SingleSourceBridge,
};

mod boundary;

mod bridge_to_analysis;
pub use bridge_to_analysis::build_minimal_analysis;

mod graph;
pub use graph::{
    AnalysisGraph, GraphEdge, GraphEdgeKind, GraphNode, GraphNodeKind, graph_from_file,
    graph_from_source_text, project_analysis_graph,
};

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bridge_tests;

#[cfg(test)]
mod bridge_to_analysis_tests;

#[cfg(test)]
mod graph_tests;
