//! Structured executor requests for the `exec` builtin.

use std::sync::Arc;

use crate::poison::DemandChain;
use crate::poison::PoisonError;
use crate::transport::{ShippedFunction, TransportModule};
use crate::value::{ExecutorSelector, HostObservation, PathValue, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecArgValue {
    Literal(String),
    Path(PathValue),
}

impl ExecArgValue {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Literal(value) => value,
            Self::Path(path) => path.as_str(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecMount {
    pub path: PathValue,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub args: Vec<ExecArgValue>,
    pub env: Vec<(String, ExecArgValue)>,
    pub mounts: Vec<ExecMount>,
    pub writable_roots: Vec<PathValue>,
    pub executor: ExecutorSelector,
    pub demand_chain: DemandChain,
    pub module: TransportModule,
    pub observer: ShippedFunction,
    pub verification_obligations: Vec<HostObservation>,
}

pub type ExecHandler = dyn Fn(ExecRequest) -> Result<Value, PoisonError> + Send + Sync + 'static;

pub type SharedExecHandler = Arc<ExecHandler>;

#[derive(Debug, Clone)]
pub struct DiscoverRequest {
    pub executor: ExecutorSelector,
    pub demand_chain: DemandChain,
    pub module: TransportModule,
    pub probe: ShippedFunction,
}

pub type DiscoverHandler =
    dyn Fn(DiscoverRequest) -> Result<Value, PoisonError> + Send + Sync + 'static;

pub type SharedDiscoverHandler = Arc<DiscoverHandler>;

#[derive(Debug, Clone)]
pub struct AcquireRequest {
    pub url: String,
    pub digest: String,
    pub extract: Option<String>,
    pub demand_chain: DemandChain,
}

pub type AcquireHandler =
    dyn Fn(AcquireRequest) -> Result<Value, PoisonError> + Send + Sync + 'static;

pub type SharedAcquireHandler = Arc<AcquireHandler>;

#[derive(Debug, Clone)]
pub struct StabilizeRequest {
    pub witness: Value,
    pub module: TransportModule,
    pub resolve: ShippedFunction,
    pub demand_chain: DemandChain,
}

pub type StabilizeHandler =
    dyn Fn(StabilizeRequest) -> Result<Value, PoisonError> + Send + Sync + 'static;

pub type SharedStabilizeHandler = Arc<StabilizeHandler>;

#[derive(Debug, Clone)]
pub struct StoreHydrationRequest {
    pub value: Value,
    pub demand_chain: DemandChain,
}

pub type StoreHydrationHandler =
    dyn Fn(StoreHydrationRequest) -> Result<Value, PoisonError> + Send + Sync + 'static;

pub type SharedStoreHydrationHandler = Arc<StoreHydrationHandler>;

#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub template: String,
    pub bindings: Value,
    pub demand_chain: DemandChain,
}

pub type RenderHandler =
    dyn Fn(RenderRequest) -> Result<Value, PoisonError> + Send + Sync + 'static;

pub type SharedRenderHandler = Arc<RenderHandler>;
