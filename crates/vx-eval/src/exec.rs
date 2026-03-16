//! Structured executor requests for the `exec` builtin.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::instr;
use crate::poison::DemandChain;
use crate::poison::PoisonError;
use crate::transport::{ShippedFunction, TransportModule};
use crate::value::{ExecutorSelector, HostObservation, PathValue, Value};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
    pub create_dirs: Vec<PathValue>,
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
pub struct ImportRequest {
    pub path: String,
    pub from_source_id: String,
    pub demand_chain: DemandChain,
}

pub type ImportHandler =
    dyn Fn(ImportRequest) -> Result<instr::Module, PoisonError> + Send + Sync + 'static;

pub type SharedImportHandler = Arc<ImportHandler>;

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

/// Trait for external runtime operations that the evaluator delegates to.
///
/// All methods are async. Implementations can `.await` directly on their
/// async infrastructure (e.g. `vxd` calls store/runner clients; tests
/// return ready values).
pub trait BuildRuntime: Send + Sync {
    fn store_hydration(
        &self,
        request: StoreHydrationRequest,
    ) -> BoxFuture<'_, Result<Value, PoisonError>>;
    fn acquire(&self, request: AcquireRequest) -> BoxFuture<'_, Result<Value, PoisonError>>;
    fn import_module(
        &self,
        request: ImportRequest,
    ) -> BoxFuture<'_, Result<instr::Module, PoisonError>>;
    fn render(&self, request: RenderRequest) -> BoxFuture<'_, Result<Value, PoisonError>>;
    fn stabilize(&self, request: StabilizeRequest) -> BoxFuture<'_, Result<Value, PoisonError>>;
    fn exec(&self, request: ExecRequest) -> BoxFuture<'_, Result<Value, PoisonError>>;
    fn discover(&self, request: DiscoverRequest) -> BoxFuture<'_, Result<Value, PoisonError>>;

    /// Whether this runtime handles exec requests (true) or the engine should
    /// fall back to local execution (false).
    fn has_exec(&self) -> bool {
        false
    }

    /// Whether this runtime handles discover requests (true) or the engine
    /// should fall back to local probe invocation (false).
    fn has_discover(&self) -> bool {
        false
    }

    /// Downcast support for `ClosureRuntime` builder methods.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// A `BuildRuntime` backed by optional closures — used by tests and the
/// `with_xxx_handler()` builder methods on `Engine`.
#[derive(Default)]
pub struct ClosureRuntime {
    pub store_hydration: Option<SharedStoreHydrationHandler>,
    pub acquire: Option<SharedAcquireHandler>,
    pub import: Option<SharedImportHandler>,
    pub render: Option<SharedRenderHandler>,
    pub stabilize: Option<SharedStabilizeHandler>,
    pub exec: Option<SharedExecHandler>,
    pub discover: Option<SharedDiscoverHandler>,
}

impl ClosureRuntime {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BuildRuntime for ClosureRuntime {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn store_hydration(
        &self,
        request: StoreHydrationRequest,
    ) -> BoxFuture<'_, Result<Value, PoisonError>> {
        let result = (self
            .store_hydration
            .as_ref()
            .expect("store_hydration handler not configured"))(request);
        Box::pin(std::future::ready(result))
    }

    fn acquire(&self, request: AcquireRequest) -> BoxFuture<'_, Result<Value, PoisonError>> {
        let result = (self
            .acquire
            .as_ref()
            .expect("acquire handler not configured"))(request);
        Box::pin(std::future::ready(result))
    }

    fn import_module(
        &self,
        request: ImportRequest,
    ) -> BoxFuture<'_, Result<instr::Module, PoisonError>> {
        let result = (self.import.as_ref().expect("import handler not configured"))(request);
        Box::pin(std::future::ready(result))
    }

    fn render(&self, request: RenderRequest) -> BoxFuture<'_, Result<Value, PoisonError>> {
        let result = (self.render.as_ref().expect("render handler not configured"))(request);
        Box::pin(std::future::ready(result))
    }

    fn stabilize(&self, request: StabilizeRequest) -> BoxFuture<'_, Result<Value, PoisonError>> {
        let result = (self
            .stabilize
            .as_ref()
            .expect("stabilize handler not configured"))(request);
        Box::pin(std::future::ready(result))
    }

    fn exec(&self, request: ExecRequest) -> BoxFuture<'_, Result<Value, PoisonError>> {
        let result = (self.exec.as_ref().expect("exec handler not configured"))(request);
        Box::pin(std::future::ready(result))
    }

    fn discover(&self, request: DiscoverRequest) -> BoxFuture<'_, Result<Value, PoisonError>> {
        let result = (self
            .discover
            .as_ref()
            .expect("discover handler not configured"))(request);
        Box::pin(std::future::ready(result))
    }

    fn has_exec(&self) -> bool {
        self.exec.is_some()
    }

    fn has_discover(&self) -> bool {
        self.discover.is_some()
    }
}
