//! Demand-driven dataflow evaluator for the Vixen language.
//!
//! This crate implements Vixen's evaluation model: a dependency graph of
//! computation nodes that resolve on demand. Nothing blocks — when a node
//! needs a value that isn't ready, it parks itself and resumes when the
//! dependency resolves.

mod capability;
pub use capability::CapabilitySet;

mod exec;
pub use exec::{
    AcquireHandler, AcquireRequest, DiscoverHandler, DiscoverRequest, ExecArgValue, ExecHandler,
    ExecMount, ExecRequest, ImportHandler, ImportRequest, RenderHandler, RenderRequest,
    SharedAcquireHandler, SharedDiscoverHandler, SharedExecHandler, SharedImportHandler,
    SharedRenderHandler, SharedStabilizeHandler, SharedStoreHydrationHandler, StabilizeHandler,
    StabilizeRequest, StoreHydrationHandler, StoreHydrationRequest,
};

mod instr;
pub use instr::{BinOp, Expr, FnDef, Module, SourceProvenance, SourceSpan, Stmt, UnaryOp};

mod lower;
pub use lower::{lower_source_file, lower_source_file_with_source_id};

mod node;
pub use node::{NodeId, NodeKey};

mod poison;
pub use poison::{
    DemandChain, PoisonError, PoisonKind, TransportDemandChain, TransportDemandEntry,
    TransportPoisonError,
};

mod scheduler;

mod transport;
pub use transport::{
    ShippedFunction, TransportBinOp, TransportBlob, TransportBlock, TransportBuiltinValue,
    TransportExpr, TransportFnDef, TransportModule, TransportParam, TransportStmt, TransportStruct,
    TransportTree, TransportUnaryOp, TransportValue,
};

mod value;
pub use value::{
    BlobValue, DiscoveredValue, ExecPathValue, ExecutorBackendKind, ExecutorSelector,
    HostObservation, HostValue, MapValue, PathValue, StructValue, TreeValue, Value,
};

mod worker;
pub use worker::{Engine, verify_host_obligations};

#[cfg(test)]
mod tests;
