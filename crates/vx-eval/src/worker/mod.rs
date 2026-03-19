//! Demand-driven evaluation.
//!
//! Expressions build lazy value graphs. Forcing only happens at the
//! external boundary or inside a node whose operation truly needs a
//! resolved dependency.

mod builtins;
mod eval;
mod exec_local;
mod identity;
mod tree_ops;

use identity::*;
pub use identity::{verify_host_obligations, verify_host_obligations_with_search_path};

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::fmt::Write;
use std::fs;
use std::mem;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use facet_value::value;
use tempfile::TempDir;
use tokio::sync::Semaphore;

use crate::CapabilitySet;
use crate::events::{ErasedEventSink, ErasedSpan};
use crate::exec::{
    AcquireRequest, BoxFuture, BuildRuntime, ClosureRuntime, DiscoverRequest, ExecArgValue,
    ExecMount, ExecMountMode, ExecRequest, ImportRequest, RenderRequest, SharedAcquireHandler,
    SharedDiscoverHandler, SharedExecHandler, SharedImportHandler, SharedRenderHandler,
    SharedStabilizeHandler, SharedStoreHydrationHandler, StabilizeRequest, StoreHydrationRequest,
};
use crate::instr::{self, BinOp, UnaryOp};
use crate::node::{
    Node, NodeId, NodeJob, NodeKey, NodeState, Outcome, TemplateKind, TemplatePartValue, Waiter,
};
use crate::poison::{DemandChain, DemandEntry, PoisonError, PoisonKind};
use crate::scheduler::Scheduler;
use crate::transport::{ShippedFunction, TransportModule, TransportValue};
use crate::value::{
    BlobValue, BuiltinFunction, BuiltinMethod, BuiltinNamespace, BuiltinValue, DiscoveredValue,
    ExecPathValue, ExecutorSelector, FunctionValue, HostObservation, HostValue, MapValue,
    PartialValue, PathValue, StructValue, TreeValue, Value,
};

tokio::task_local! {
    pub(crate) static DEMAND_STACK: RefCell<Vec<NodeId>>;
}

const DEFAULT_MAX_DEMAND_DEPTH: usize = 1_024;
const IMPLICIT_EXEC_MOUNT_PREFIX: &str = ".vx/implicit";

enum BuildResult {
    Value(Value),
    Returned(Value),
    Poisoned(PoisonError),
}

pub(crate) struct InspectedValue {
    pub value: Value,
    pub pending: BTreeSet<NodeId>,
    pub errors: Vec<PoisonError>,
}

type InspectedList = (Vec<Value>, BTreeSet<NodeId>, Vec<PoisonError>);

impl InspectedValue {
    fn resolved(value: Value) -> Self {
        Self {
            value,
            pending: BTreeSet::new(),
            errors: Vec::new(),
        }
    }
}

pub(crate) enum SchedulerNodeState {
    Pending,
    Evaluating,
    Resolved(Value),
    Poisoned(PoisonError),
}

#[derive(Clone)]
struct ParsedExecRequest {
    args: Vec<ExecArgValue>,
    env: Vec<(String, ExecArgValue)>,
    mounts: Vec<ExecMount>,
    writable_roots: Vec<PathValue>,
    create_dirs: Vec<PathValue>,
    executor: ExecutorSelector,
    observer: Value,
    verification_obligations: Vec<HostObservation>,
}

type ModuleId = Arc<str>;

struct ModuleState {
    module: instr::Module,
    functions: HashMap<String, Vec<Arc<instr::FnDef>>>,
    function_identities: HashMap<usize, Arc<str>>,
    scope_state: Mutex<ModuleScopeState>,
}

enum ModuleScopeState {
    Uninitialized,
    Evaluating,
    Ready(Arc<HashMap<String, Value>>),
    Poisoned(PoisonError),
}

/// The evaluation engine.
pub struct Engine {
    root_module_id: ModuleId,
    modules: DashMap<ModuleId, Arc<ModuleState>>,
    nodes: DashMap<NodeId, Arc<Node>>,
    call_memo: DashMap<NodeKey, Arc<Node>>,
    max_demand_depth: usize,
    runtime: Option<Arc<dyn BuildRuntime>>,
    self_ref: std::sync::OnceLock<std::sync::Weak<Engine>>,
    host_search_path: Option<Vec<std::path::PathBuf>>,
    exec_semaphore: Option<Arc<Semaphore>>,
    event_sink: Arc<dyn ErasedEventSink>,
    capabilities: CapabilitySet,
}

impl Engine {
    fn build_module_state(module: instr::Module) -> ModuleState {
        let mut functions = HashMap::new();
        for func in &module.functions {
            functions
                .entry(func.name.clone())
                .or_insert_with(Vec::new)
                .push(func.clone());
        }
        let function_identities =
            compute_function_identities(&module.functions, module.source_id.as_ref());
        ModuleState {
            module,
            functions,
            function_identities,
            scope_state: Mutex::new(ModuleScopeState::Uninitialized),
        }
    }

    /// Create a new engine from a lowered module.
    pub fn from_module(module: &instr::Module) -> Arc<Self> {
        let root_module_id = Arc::clone(&module.source_id);
        let modules = DashMap::new();
        modules.insert(
            Arc::clone(&root_module_id),
            Arc::new(Self::build_module_state(module.clone())),
        );

        let exec_semaphore = env::var("VX_EXEC_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .map(|n| Arc::new(Semaphore::new(n)));

        let engine = Arc::new(Self {
            root_module_id,
            modules,
            nodes: DashMap::new(),
            call_memo: DashMap::new(),
            max_demand_depth: DEFAULT_MAX_DEMAND_DEPTH,
            runtime: None,
            self_ref: std::sync::OnceLock::new(),
            host_search_path: None,
            exec_semaphore,
            event_sink: Arc::new(()),
            capabilities: CapabilitySet::NONE,
        });
        engine
            .self_ref
            .set(Arc::downgrade(&engine))
            .expect("self_ref already set");
        engine
    }

    /// Set a custom event sink for recording engine events.
    pub fn with_event_sink(self: Arc<Self>, sink: Arc<dyn ErasedEventSink>) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        this.event_sink = sink;
        Self::rewrap(this)
    }

    fn rewrap(mut this: Self) -> Arc<Self> {
        this.self_ref = std::sync::OnceLock::new();
        let arc = Arc::new(this);
        arc.self_ref
            .set(Arc::downgrade(&arc))
            .expect("self_ref already set");
        arc
    }

    fn arc_self(&self) -> Arc<Self> {
        self.self_ref
            .get()
            .expect("engine self_ref not set")
            .upgrade()
            .expect("engine Arc dropped while still in use")
    }

    // r[impl eval.demand-depth]
    pub fn with_max_demand_depth(self: Arc<Self>, max_demand_depth: usize) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        this.max_demand_depth = max_demand_depth;
        Self::rewrap(this)
    }

    pub fn with_host_search_path(self: Arc<Self>, path: Vec<std::path::PathBuf>) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        this.host_search_path = Some(path);
        Self::rewrap(this)
    }

    pub fn with_runtime(self: Arc<Self>, runtime: Arc<dyn BuildRuntime>) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        this.runtime = Some(runtime);
        Self::rewrap(this)
    }

    pub fn with_capabilities(self: Arc<Self>, capabilities: CapabilitySet) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        this.capabilities = capabilities;
        Self::rewrap(this)
    }

    fn ensure_closure_runtime(this: &mut Self) -> &mut ClosureRuntime {
        if this.runtime.is_none() {
            this.runtime = Some(Arc::new(ClosureRuntime::default()));
        }
        let arc = this.runtime.as_mut().unwrap();
        Arc::get_mut(arc)
            .expect("cannot add closure handlers after runtime Arc has been shared")
            .as_any_mut()
            .downcast_mut::<ClosureRuntime>()
            .expect("cannot add closure handlers to a non-ClosureRuntime")
    }

    pub fn with_acquire_handler(self: Arc<Self>, handler: SharedAcquireHandler) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        Self::ensure_closure_runtime(&mut this).acquire = Some(handler);
        Self::rewrap(this)
    }

    pub fn with_render_handler(self: Arc<Self>, handler: SharedRenderHandler) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        Self::ensure_closure_runtime(&mut this).render = Some(handler);
        Self::rewrap(this)
    }

    pub fn with_stabilize_handler(self: Arc<Self>, handler: SharedStabilizeHandler) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        Self::ensure_closure_runtime(&mut this).stabilize = Some(handler);
        Self::rewrap(this)
    }

    pub fn with_exec_handler(self: Arc<Self>, handler: SharedExecHandler) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        Self::ensure_closure_runtime(&mut this).exec = Some(handler);
        Self::rewrap(this)
    }

    pub fn with_discover_handler(self: Arc<Self>, handler: SharedDiscoverHandler) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        Self::ensure_closure_runtime(&mut this).discover = Some(handler);
        Self::rewrap(this)
    }

    pub fn with_import_handler(self: Arc<Self>, handler: SharedImportHandler) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        Self::ensure_closure_runtime(&mut this).import = Some(handler);
        Self::rewrap(this)
    }

    pub fn with_store_hydration_handler(
        self: Arc<Self>,
        handler: SharedStoreHydrationHandler,
    ) -> Arc<Self> {
        let mut this =
            Arc::try_unwrap(self).unwrap_or_else(|_| panic!("builder called after Arc shared"));
        Self::ensure_closure_runtime(&mut this).store_hydration = Some(handler);
        Self::rewrap(this)
    }

    // r[impl eval.forcing.boundary]
    /// Evaluate a top-level function by name at the external forcing boundary.
    pub async fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let function = self
                    .resolve_module_function(&self.root_module_id, name)
                    .await?;
                let provenance = function.def.provenance.clone();
                let value = self.build_call(Value::Function(function), args, provenance);
                self.resolve_boundary_value(value).await
            })
            .await
    }

    pub fn transport_module(&self) -> TransportModule {
        self.transport_module_for(self.root_module_id.as_ref())
    }

    pub fn transport_module_for(&self, root_module_id: &str) -> TransportModule {
        let root_module_id: ModuleId = Arc::<str>::from(root_module_id);
        let root = self
            .module_state(&root_module_id)
            .expect("transport root module must exist");
        let mut loaded_modules = self
            .modules
            .iter()
            .filter_map(|entry| {
                if entry.key().as_ref() == root_module_id.as_ref() {
                    None
                } else {
                    Some(TransportModule::from(&entry.value().module))
                }
            })
            .collect::<Vec<_>>();
        loaded_modules.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        let mut module = TransportModule::from(&root.module);
        module.loaded_modules = loaded_modules;
        module
    }

    pub async fn module_namespace(&self) -> Result<Value, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                self.module_namespace_for(&self.root_module_id).await
            })
            .await
    }

    async fn module_namespace_for(&self, module_id: &ModuleId) -> Result<Value, PoisonError> {
        let bindings = self.eval_module_scope(module_id).await?;
        let mut entries = bindings
            .iter()
            .map(|(name, value)| (Value::String(Arc::from(name.as_str())), value.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|(left, _), (right, _)| {
            let Value::String(left) = left else {
                unreachable!("module exports are string-keyed")
            };
            let Value::String(right) = right else {
                unreachable!("module exports are string-keyed")
            };
            left.cmp(right)
        });
        Ok(Value::Map(MapValue {
            entries: Arc::from(entries),
        }))
    }

    fn module_state(&self, module_id: &ModuleId) -> Result<Arc<ModuleState>, PoisonError> {
        self.modules
            .get(module_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| self.internal_error(&format!("missing module {}", module_id)))
    }

    pub async fn ship_value(&self, value: Value) -> Result<TransportValue, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let value = self.resolve_boundary_value(value).await?;
                TransportValue::try_from(&value).map_err(|message| self.type_error(&message))
            })
            .await
    }

    pub async fn ship_function(&self, value: Value) -> Result<ShippedFunction, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let value = self.resolve_boundary_value(value).await?;
                let Value::Function(function) = value else {
                    return Err(self.type_error("shipped callbacks must be functions"));
                };
                ShippedFunction::try_from(&function).map_err(|message| self.type_error(&message))
            })
            .await
    }

    pub async fn call_shipped(
        &self,
        function: &ShippedFunction,
        args: Vec<Value>,
    ) -> Result<Value, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let function = function
                    .to_runtime()
                    .map_err(|message| self.type_error(&message))?;
                let value = self.build_call(Value::Function(function), args, None);
                self.resolve_boundary_value(value).await
            })
            .await
    }

    pub async fn invoke_shipped(
        module: &TransportModule,
        function: &ShippedFunction,
        args: Vec<Value>,
    ) -> Result<Value, PoisonError> {
        let engine = Self::from_transport_module(module);
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let function = function
                    .to_runtime()
                    .map_err(|message| engine.type_error(&message))?;
                let value = engine.build_call(Value::Function(function), args, None);
                engine.resolve_boundary_value(value).await
            })
            .await
    }

    pub async fn invoke_shipped_discover(
        module: &TransportModule,
        function: &ShippedFunction,
    ) -> Result<Value, PoisonError> {
        let engine = Self::from_transport_module(module);
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let function = function
                    .to_runtime()
                    .map_err(|message| engine.type_error(&message))?;
                engine
                    .invoke_discover_probe(Value::Function(function))
                    .await
            })
            .await
    }

    fn from_transport_module(module: &TransportModule) -> Arc<Self> {
        let root: instr::Module = module.root_only().into();
        let engine = Self::from_module(&root);
        for imported in &module.loaded_modules {
            let imported_module: instr::Module = imported.root_only().into();
            let module_id = Arc::clone(&imported_module.source_id);
            engine.modules.insert(
                module_id,
                Arc::new(Self::build_module_state(imported_module)),
            );
        }
        engine
    }

    async fn resolve_boundary_value(&self, value: Value) -> Result<Value, PoisonError> {
        Scheduler::new(self.arc_self()).resolve_value(value).await
    }

    #[cfg(test)]
    pub(crate) async fn call_unforced(
        &self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let function = self
                    .resolve_module_function(&self.root_module_id, name)
                    .await?;
                Ok(self.build_call(Value::Function(function), args, None))
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn force_shallow(&self, value: Value) -> Result<Value, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                self.force_value_shallow(value).await
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn force_fully(&self, value: Value) -> Result<Value, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                self.force_value(value).await
            })
            .await
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn poison_value(&self, message: &str) -> Value {
        assert!(
            self.capabilities.has(CapabilitySet::TEST_ONLY),
            "poison_value requires test-only capabilities"
        );
        let error = PoisonError::new(PoisonKind::UserFail, message, DemandChain::new());
        self.pending_value(NodeJob::Poison(error), format!("poison({message})"), None)
    }

    #[cfg(test)]
    pub(crate) fn memoized_call_count(&self) -> usize {
        self.call_memo.len()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn sleep_value(&self, millis: u64, value: Value) -> Value {
        assert!(
            self.capabilities.has(CapabilitySet::TEST_ONLY),
            "sleep_value requires test-only capabilities"
        );
        self.pending_value(
            NodeJob::Sleep { millis, value },
            format!("sleep({millis}ms)"),
            None,
        )
    }

    #[cfg(test)]
    pub(crate) async fn function_key(&self, name: &str) -> Result<String, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                self.resolve_module_function(&self.root_module_id, name)
                    .await?
                    .key()
                    .ok_or_else(|| self.internal_error("top-level function key should be resolved"))
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn function_identity_string(&self, name: &str) -> Result<String, PoisonError> {
        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                Ok(self
                    .resolve_module_function(&self.root_module_id, name)
                    .await?
                    .identity())
            })
            .await
    }

    // r[impl eval.entry]
    pub async fn call_entry(
        &self,
        name: &str,
        workspace: impl AsRef<str>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    ) -> Result<Value, PoisonError> {
        let workspace = Value::Path(PathValue::new(workspace.as_ref()));
        let args = Value::Array(Arc::from(
            args.into_iter()
                .map(|arg| Value::String(Arc::from(arg)))
                .collect::<Vec<_>>(),
        ));
        let env = Value::Map(MapValue {
            entries: Arc::from(
                env.into_iter()
                    .map(|(key, value)| {
                        (
                            Value::String(Arc::from(key)),
                            Value::String(Arc::from(value)),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        });

        DEMAND_STACK
            .scope(RefCell::new(Vec::new()), async {
                let value = self.call(name, vec![workspace, args, env]).await?;
                match value {
                    Value::Tree(_) => Ok(value),
                    Value::Discovered(discovered)
                        if matches!(discovered.value.as_ref(), Value::Tree(_)) =>
                    {
                        Ok(Value::Discovered(discovered))
                    }
                    other => Err(self.type_error(&format!(
                        "build entry must return Tree, got {}",
                        other.type_name()
                    ))),
                }
            })
            .await
    }

    async fn invoke_discover_probe(&self, probe: Value) -> Result<Value, PoisonError> {
        match probe {
            Value::Function(_) | Value::Builtin(_) => {}
            other => {
                return Err(self.type_error(&format!(
                    "discover probe must be callable, got {}",
                    other.type_name()
                )));
            }
        }

        self.resolve_boundary_value(self.build_call(probe, vec![Value::Host(HostValue)], None))
            .await
    }

    fn add_obligations(
        obligations: &mut BTreeMap<String, HostObservation>,
        items: impl IntoIterator<Item = HostObservation>,
    ) {
        for observation in items {
            obligations.insert(observation.key(), observation);
        }
    }

    fn attach_obligations(
        &self,
        value: Value,
        items: impl IntoIterator<Item = HostObservation>,
    ) -> Value {
        let mut obligations = BTreeMap::new();
        let mut preserve_discovered = false;
        let inner = match value {
            Value::Discovered(discovered) => {
                preserve_discovered = true;
                Self::add_obligations(&mut obligations, discovered.obligations.iter().cloned());
                *discovered.value
            }
            other => other,
        };
        Self::add_obligations(&mut obligations, items);
        if obligations.is_empty() {
            if preserve_discovered {
                Value::Discovered(DiscoveredValue {
                    value: Box::new(inner),
                    obligations: Arc::from([]),
                })
            } else {
                inner
            }
        } else {
            Value::Discovered(DiscoveredValue {
                value: Box::new(inner),
                obligations: Arc::from(obligations.into_values().collect::<Vec<_>>()),
            })
        }
    }

    fn reattach_discovered(
        &self,
        value: Value,
        obligations: impl IntoIterator<Item = HostObservation>,
    ) -> Value {
        self.attach_obligations(
            Value::Discovered(DiscoveredValue {
                value: Box::new(value),
                obligations: Arc::from([]),
            }),
            obligations,
        )
    }

    fn finish_obligated_result(
        &self,
        result: Result<Value, PoisonError>,
        items: impl IntoIterator<Item = HostObservation>,
    ) -> Result<Value, PoisonError> {
        let items = items.into_iter().collect::<Vec<_>>();
        match result {
            Ok(value) => Ok(self.attach_obligations(value, items.iter().cloned())),
            Err(error) => Err(error.with_observations(items)),
        }
    }

    fn finish_obligated_build_result(
        &self,
        result: BuildResult,
        items: impl IntoIterator<Item = HostObservation>,
    ) -> BuildResult {
        let items = items.into_iter().collect::<Vec<_>>();
        match result {
            BuildResult::Value(value) => {
                BuildResult::Value(self.attach_obligations(value, items.iter().cloned()))
            }
            BuildResult::Returned(value) => {
                BuildResult::Returned(self.attach_obligations(value, items.iter().cloned()))
            }
            BuildResult::Poisoned(error) => BuildResult::Poisoned(error.with_observations(items)),
        }
    }

    fn fallible_none_from_error(&self, error: &PoisonError) -> Value {
        self.attach_obligations(option_none(), error.observations().iter().cloned())
    }

    fn split_obligations(&self, value: Value) -> (Value, Vec<HostObservation>) {
        match value {
            Value::Discovered(discovered) => (*discovered.value, discovered.obligations.to_vec()),
            other => (other, Vec::new()),
        }
    }

    fn split_obligations_ref<'a>(&self, value: &'a Value) -> (&'a Value, &'a [HostObservation]) {
        match value {
            Value::Discovered(discovered) => {
                (discovered.value.as_ref(), discovered.obligations.as_ref())
            }
            other => (other, &[]),
        }
    }

    async fn hydrate_store_value(&self, value: Value, context: &str) -> Result<Value, PoisonError> {
        match value {
            Value::Blob(BlobValue::Store { .. }) | Value::Tree(TreeValue::Store(_)) => {
                let Some(runtime) = &self.runtime else {
                    return Err(self.internal_error(&format!(
                        "{context} requires an external store hydration handler"
                    )));
                };
                let hydrated = runtime
                    .store_hydration(StoreHydrationRequest {
                        value,
                        demand_chain: self.current_demand_chain(None),
                    })
                    .await?;
                match hydrated {
                    Value::Blob(BlobValue::Store { .. }) | Value::Tree(TreeValue::Store(_)) => {
                        Err(self.internal_error(&format!(
                            "{context} store hydration did not resolve the requested value"
                        )))
                    }
                    other => Ok(other),
                }
            }
            other => Ok(other),
        }
    }

    async fn hydrate_store_blob(
        &self,
        blob: &BlobValue,
        context: &str,
    ) -> Result<BlobValue, PoisonError> {
        let hydrated = self
            .hydrate_store_value(Value::Blob(blob.clone()), context)
            .await?;
        let Value::Blob(blob) = hydrated else {
            return Err(self.internal_error(&format!(
                "{context} store hydration returned {} instead of Blob",
                hydrated.type_name()
            )));
        };
        Ok(blob)
    }

    async fn hydrate_store_tree(
        &self,
        tree: &TreeValue,
        context: &str,
    ) -> Result<TreeValue, PoisonError> {
        let hydrated = self
            .hydrate_store_value(Value::Tree(tree.clone()), context)
            .await?;
        let Value::Tree(tree) = hydrated else {
            return Err(self.internal_error(&format!(
                "{context} store hydration returned {} instead of Tree",
                hydrated.type_name()
            )));
        };
        Ok(tree)
    }

    fn collect_value_obligations(
        &self,
        value: &Value,
        obligations: &mut BTreeMap<String, HostObservation>,
    ) {
        match value {
            Value::Discovered(discovered) => {
                Self::add_obligations(obligations, discovered.obligations.iter().cloned());
                self.collect_value_obligations(discovered.value.as_ref(), obligations);
            }
            Value::ExecPath(ExecPathValue::InTree { tree, .. }) => {
                self.collect_tree_obligations(tree, obligations);
            }
            Value::ExecPath(ExecPathValue::InBlob { .. }) => {}
            Value::Tree(tree) => {
                self.collect_tree_obligations(tree, obligations);
            }
            Value::Tuple(values) | Value::Array(values) => {
                for value in values.iter() {
                    self.collect_value_obligations(value, obligations);
                }
            }
            Value::Map(map) => {
                for (key, value) in map.entries.iter() {
                    self.collect_value_obligations(key, obligations);
                    self.collect_value_obligations(value, obligations);
                }
            }
            Value::Struct(struct_value) => {
                for (_, value) in struct_value.fields.iter() {
                    self.collect_value_obligations(value, obligations);
                }
            }
            Value::Function(function) => {
                for (_, capture) in function.captures.iter() {
                    self.collect_value_obligations(capture, obligations);
                }
            }
            Value::Partial(partial) => {
                self.collect_value_obligations(&partial.function, obligations);
                for value in partial.args.iter() {
                    self.collect_value_obligations(value, obligations);
                }
                for (_, value) in partial.kwargs.iter() {
                    self.collect_value_obligations(value, obligations);
                }
                if let Some(value) = &partial.kwargs_expr {
                    self.collect_value_obligations(value, obligations);
                }
            }
            Value::Builtin(BuiltinValue::Method(method)) => match method {
                BuiltinMethod::StringStartsWith { receiver }
                | BuiltinMethod::StringContains { receiver }
                | BuiltinMethod::StringSplit { receiver }
                | BuiltinMethod::StringLines { receiver }
                | BuiltinMethod::StringTrim { receiver }
                | BuiltinMethod::StringToLower { receiver }
                | BuiltinMethod::ArrayMap { receiver }
                | BuiltinMethod::ArrayToMap { receiver }
                | BuiltinMethod::ArrayFlatten { receiver }
                | BuiltinMethod::ArrayMerge { receiver }
                | BuiltinMethod::MapMapValues { receiver }
                | BuiltinMethod::MapHas { receiver }
                | BuiltinMethod::MapGet { receiver }
                | BuiltinMethod::BlobToString { receiver }
                | BuiltinMethod::BlobExecutable { receiver }
                | BuiltinMethod::BlobUtf8 { receiver }
                | BuiltinMethod::BlobUtf8Lossy { receiver }
                | BuiltinMethod::BytesUtf8 { receiver }
                | BuiltinMethod::BytesUtf8Lossy { receiver }
                | BuiltinMethod::BytesBlob { receiver }
                | BuiltinMethod::StreamCollect { receiver }
                | BuiltinMethod::TreeFile { receiver }
                | BuiltinMethod::TreeMerge { receiver }
                | BuiltinMethod::TreeSubtree { receiver }
                | BuiltinMethod::OptionUnwrapOr { receiver }
                | BuiltinMethod::FallibleCall { receiver }
                | BuiltinMethod::HostOs { receiver }
                | BuiltinMethod::HostArch { receiver }
                | BuiltinMethod::HostRead { receiver }
                | BuiltinMethod::HostHash { receiver }
                | BuiltinMethod::HostWhich { receiver } => {
                    self.collect_value_obligations(receiver.as_ref(), obligations);
                }
                BuiltinMethod::BlobFromString
                | BuiltinMethod::BytesFromString
                | BuiltinMethod::TreeOf
                | BuiltinMethod::TreeEmpty
                | BuiltinMethod::TreeFromPath => {}
            },
            _ => {}
        }
    }

    fn collect_tree_obligations(
        &self,
        tree: &TreeValue,
        obligations: &mut BTreeMap<String, HostObservation>,
    ) {
        if let TreeValue::Inline { entries, .. } = tree {
            for (_, value) in entries.iter() {
                self.collect_value_obligations(value, obligations);
            }
        }
    }

    fn global_function_value(&self, module_id: &ModuleId, def: Arc<instr::FnDef>) -> FunctionValue {
        FunctionValue {
            module_id: Arc::clone(module_id),
            code_identity: self.function_identity(module_id, &def),
            def,
            captures: Arc::from([]),
        }
    }

    fn local_function_value(
        &self,
        module_id: &ModuleId,
        def: Arc<instr::FnDef>,
        captures: Arc<[(Arc<str>, Value)]>,
    ) -> FunctionValue {
        FunctionValue {
            module_id: Arc::clone(module_id),
            code_identity: self.function_identity(module_id, &def),
            def,
            captures,
        }
    }

    fn closure_function_value(
        &self,
        module_id: &ModuleId,
        def: Arc<instr::FnDef>,
        code_identity: Arc<str>,
        captures: Arc<[(Arc<str>, Value)]>,
    ) -> FunctionValue {
        FunctionValue {
            module_id: Arc::clone(module_id),
            code_identity,
            def,
            captures,
        }
    }

    fn struct_constructor_value(&self, decl: &instr::StructDecl) -> Value {
        Value::Builtin(BuiltinValue::Function(BuiltinFunction::StructConstructor {
            type_name: Arc::from(decl.name.as_str()),
            fields: decl.fields.clone(),
        }))
    }

    fn function_identity(&self, module_id: &ModuleId, def: &Arc<instr::FnDef>) -> Arc<str> {
        let module = self
            .module_state(module_id)
            .expect("function identity requires loaded module");
        module
            .function_identities
            .get(&fn_ptr(def))
            .cloned()
            .unwrap_or_else(|| Arc::from(format!("{module_id}::{}", stable_fn_repr(def))))
    }

    // r[impl eval.pending]
    // r[impl eval.memoization.pending]
    fn build_call(
        &self,
        function: Value,
        args: Vec<Value>,
        provenance: Option<instr::SourceProvenance>,
    ) -> Value {
        if let Value::Function(function_value) = &function
            && function_value.key().is_some()
            && args.iter().all(|value| value.memo_key().is_some())
        {
            return Value::Pending(self.memoized_invoke_node(function_value.clone(), args));
        }

        self.pending_value(
            NodeJob::Call {
                function,
                args: Arc::from(args),
                kwargs: Arc::from([]),
                kwargs_expr: None,
            },
            "call(..)".into(),
            provenance,
        )
    }

    fn build_call_with_kwargs(
        &self,
        function: Value,
        args: Vec<Value>,
        kwargs: Vec<(Arc<str>, Value)>,
        kwargs_expr: Option<Value>,
        provenance: Option<instr::SourceProvenance>,
    ) -> Value {
        if kwargs.is_empty() && kwargs_expr.is_none() {
            return self.build_call(function, args, provenance);
        }

        self.pending_value(
            NodeJob::Call {
                function,
                args: Arc::from(args),
                kwargs: Arc::from(kwargs),
                kwargs_expr,
            },
            "call(..)".into(),
            provenance,
        )
    }

    fn memoized_invoke_node(&self, function: FunctionValue, args: Vec<Value>) -> NodeId {
        let key = NodeKey {
            identity: format!(
                "call:{}({})",
                function.key().unwrap_or_else(|| "<unresolved>".into()),
                args.iter()
                    .map(|value| value.memo_key().unwrap_or_else(|| "<unknown>".into()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        };

        match self.call_memo.entry(key) {
            Entry::Occupied(entry) => {
                self.event_sink.instant("memo.hit");
                entry.get().id
            }
            Entry::Vacant(entry) => {
                self.event_sink.instant("memo.miss");
                let node = Arc::new(Node::new(
                    NodeJob::Invoke {
                        function,
                        args: Arc::from(args),
                    },
                    "invoke(..)".into(),
                    None,
                ));
                let node_id = node.id;
                self.nodes.insert(node_id, node.clone());
                entry.insert(node);
                node_id
            }
        }
    }

    fn pending_value(
        &self,
        job: NodeJob,
        description: String,
        provenance: Option<instr::SourceProvenance>,
    ) -> Value {
        let node = Arc::new(Node::new(job, description, provenance));
        let node_id = node.id;
        self.nodes.insert(node_id, node);
        Value::Pending(node_id)
    }

    pub(crate) fn max_demand_depth_limit(&self) -> usize {
        self.max_demand_depth
    }

    pub(crate) fn node_state(&self, node_id: NodeId) -> Result<SchedulerNodeState, PoisonError> {
        let node = self.node_ref(node_id)?;
        let state = node.state.lock();
        match &*state {
            NodeState::Pending => Ok(SchedulerNodeState::Pending),
            NodeState::Evaluating { .. } => Ok(SchedulerNodeState::Evaluating),
            NodeState::Resolved(value) => Ok(SchedulerNodeState::Resolved(value.clone())),
            NodeState::Poisoned(error) => Ok(SchedulerNodeState::Poisoned(error.clone())),
        }
    }

    pub(crate) async fn wait_for_node_completion(
        &self,
        node_id: NodeId,
    ) -> Result<(), PoisonError> {
        let node = self.node_ref(node_id)?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let waiter = Waiter { sender };

        let immediate = {
            let mut state = node.state.lock();
            match &mut *state {
                NodeState::Pending => return Ok(()),
                NodeState::Evaluating { waiters } => {
                    waiters.push(waiter);
                    None
                }
                NodeState::Resolved(value) => Some(Outcome::Value(value.clone())),
                NodeState::Poisoned(error) => Some(Outcome::Poison(error.clone())),
            }
        };

        match immediate {
            Some(Outcome::Value(_)) | Some(Outcome::Poison(_)) => Ok(()),
            None => match receiver.await {
                Ok((_node_id, Outcome::Value(_))) | Ok((_node_id, Outcome::Poison(_))) => Ok(()),
                Err(_) => Err(self.internal_error("all scheduler waiters dropped")),
            },
        }
    }

    pub(crate) async fn node_deps(&self, node_id: NodeId) -> Result<Vec<NodeId>, PoisonError> {
        let node = self.node_ref(node_id)?;
        let deps = match &node.job {
            NodeJob::Binary { op, left, right } => {
                let mut deps = if matches!(op, BinOp::Concat) {
                    self.collect_pending_shallow(left.clone()).await?
                } else {
                    self.collect_pending_deep(left.clone()).await?
                };
                deps.extend(if matches!(op, BinOp::Concat) {
                    self.collect_pending_shallow(right.clone()).await?
                } else {
                    self.collect_pending_deep(right.clone()).await?
                });
                deps
            }
            NodeJob::Unary { operand, .. } => self.collect_pending_deep(operand.clone()).await?,
            NodeJob::Template { parts, .. } => {
                let mut deps = Vec::new();
                for part in parts.iter() {
                    if let TemplatePartValue::Expr(value) = part {
                        deps.extend(self.collect_pending_deep(value.clone()).await?);
                    }
                }
                deps
            }
            NodeJob::If { cond, .. } => self.collect_pending_deep(cond.clone()).await?,
            NodeJob::Fallible { value } => self.collect_pending_shallow(value.clone()).await?,
            NodeJob::Demand { demands, .. } => {
                let mut deps = Vec::new();
                for value in demands.iter() {
                    deps.extend(self.collect_pending_deep(value.clone()).await?);
                }
                deps
            }
            NodeJob::Fail { message } => self.collect_pending_deep(message.clone()).await?,
            NodeJob::Call {
                function,
                args,
                kwargs,
                kwargs_expr,
            } => {
                let mut deps = self.collect_callable_pending(function.clone()).await?;
                if deps.is_empty() {
                    deps.extend(
                        self.builtin_call_pending_deps(
                            function.clone(),
                            args.as_ref(),
                            kwargs.as_ref(),
                            kwargs_expr.as_ref(),
                        )
                        .await?,
                    );
                } else if let Some(kwargs_expr) = kwargs_expr {
                    deps.extend(self.collect_pending_shallow(kwargs_expr.clone()).await?);
                }
                deps
            }
            NodeJob::Invoke { .. } => Vec::new(),
            NodeJob::Field { receiver, .. } => {
                self.collect_pending_shallow(receiver.clone()).await?
            }
            NodeJob::Sleep { .. } | NodeJob::Poison(_) => Vec::new(),
        };
        Ok(Self::dedup_node_ids(deps))
    }

    async fn collect_pending_deep(&self, value: Value) -> Result<Vec<NodeId>, PoisonError> {
        Ok(self
            .inspect_value(value)
            .await?
            .pending
            .into_iter()
            .collect())
    }

    async fn collect_pending_shallow(&self, value: Value) -> Result<Vec<NodeId>, PoisonError> {
        self.collect_pending_shallow_inner(value, &mut Vec::new())
            .await
    }

    fn collect_pending_shallow_inner<'a>(
        &'a self,
        value: Value,
        visiting: &'a mut Vec<NodeId>,
    ) -> BoxFuture<'a, Result<Vec<NodeId>, PoisonError>> {
        Box::pin(async move {
            match value {
                Value::Pending(node_id) => match self.node_state(node_id)? {
                    SchedulerNodeState::Pending | SchedulerNodeState::Evaluating => {
                        Ok(vec![node_id])
                    }
                    SchedulerNodeState::Resolved(value) => {
                        if visiting.contains(&node_id) {
                            return Err(self.circular_dependency_error(visiting, node_id));
                        }
                        visiting.push(node_id);
                        let result = self.collect_pending_shallow_inner(value, visiting).await;
                        let popped = visiting.pop();
                        debug_assert_eq!(popped, Some(node_id));
                        result
                    }
                    SchedulerNodeState::Poisoned(_) => Ok(Vec::new()),
                },
                Value::Discovered(discovered) => {
                    self.collect_pending_shallow_inner(*discovered.value, visiting)
                        .await
                }
                Value::Builtin(_) => {
                    let mut deps = Vec::new();
                    value.collect_pending(&mut deps);
                    Ok(Self::dedup_node_ids(deps))
                }
                _ => Ok(Vec::new()),
            }
        })
    }

    async fn collect_callable_pending(&self, value: Value) -> Result<Vec<NodeId>, PoisonError> {
        self.collect_callable_pending_inner(value, &mut Vec::new())
            .await
    }

    fn collect_callable_pending_inner<'a>(
        &'a self,
        value: Value,
        visiting: &'a mut Vec<NodeId>,
    ) -> BoxFuture<'a, Result<Vec<NodeId>, PoisonError>> {
        Box::pin(async move {
            match value {
                Value::Pending(node_id) => match self.node_state(node_id)? {
                    SchedulerNodeState::Pending | SchedulerNodeState::Evaluating => {
                        Ok(vec![node_id])
                    }
                    SchedulerNodeState::Resolved(value) => {
                        if visiting.contains(&node_id) {
                            return Err(self.circular_dependency_error(visiting, node_id));
                        }
                        visiting.push(node_id);
                        let result = self.collect_callable_pending_inner(value, visiting).await;
                        let popped = visiting.pop();
                        debug_assert_eq!(popped, Some(node_id));
                        result
                    }
                    SchedulerNodeState::Poisoned(_) => Ok(Vec::new()),
                },
                Value::Discovered(discovered) => {
                    self.collect_callable_pending_inner(*discovered.value, visiting)
                        .await
                }
                Value::Partial(partial) => {
                    self.collect_callable_pending_inner(partial.function.clone(), visiting)
                        .await
                }
                _ => Ok(Vec::new()),
            }
        })
    }

    fn builtin_call_pending_deps<'a>(
        &'a self,
        function: Value,
        args: &'a [Value],
        kwargs: &'a [(Arc<str>, Value)],
        kwargs_expr: Option<&'a Value>,
    ) -> BoxFuture<'a, Result<Vec<NodeId>, PoisonError>> {
        Box::pin(async move {
            let function = self
                .materialize_value_shallow(function, "forcing callable surface")
                .await?;
            let (function, _) = self.split_obligations(function);
            match function {
                Value::Partial(partial) => {
                    let mut deps = self
                        .builtin_call_pending_deps(partial.function.clone(), &[], &[], None)
                        .await?;
                    for value in partial.args.iter().chain(args.iter()) {
                        deps.extend(self.collect_pending_deep(value.clone()).await?);
                    }
                    for (_, value) in partial.kwargs.iter().chain(kwargs.iter()) {
                        deps.extend(self.collect_pending_deep(value.clone()).await?);
                    }
                    if let Some(kwargs_expr) = &partial.kwargs_expr {
                        deps.extend(self.collect_pending_deep((**kwargs_expr).clone()).await?);
                    }
                    if let Some(kwargs_expr) = kwargs_expr {
                        deps.extend(self.collect_pending_deep(kwargs_expr.clone()).await?);
                    }
                    Ok(Self::dedup_node_ids(deps))
                }
                Value::Builtin(builtin) => match builtin {
                    BuiltinValue::Namespace(BuiltinNamespace::Blob)
                    | BuiltinValue::Namespace(BuiltinNamespace::Bytes) => Ok(Self::dedup_node_ids(
                        self.collect_pending_deep(args[0].clone()).await?,
                    )),
                    BuiltinValue::Namespace(BuiltinNamespace::Tree) => {
                        let mut deps = self.collect_pending_shallow(args[0].clone()).await?;
                        if !deps.is_empty() {
                            return Ok(Self::dedup_node_ids(deps));
                        }

                        let value = self
                            .materialize_value_shallow(args[0].clone(), "forcing Tree input")
                            .await?;
                        let (value, _) = self.split_obligations(value);
                        match value {
                            Value::Map(map) => {
                                for (key, _) in map.entries.iter() {
                                    deps.extend(self.collect_pending_deep(key.clone()).await?);
                                }
                            }
                            Value::Array(items) => {
                                for item in items.iter() {
                                    deps.extend(self.collect_pending_deep(item.clone()).await?);
                                }
                            }
                            Value::Path(_) | Value::String(_) => {}
                            _ => {}
                        }
                        Ok(Self::dedup_node_ids(deps))
                    }
                    BuiltinValue::Function(BuiltinFunction::StructConstructor { .. }) => Ok(
                        Self::dedup_node_ids(self.collect_pending_shallow(args[0].clone()).await?),
                    ),
                    BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver }) => {
                        let receiver = self
                            .materialize_value_shallow(*receiver, "forcing array.flatten receiver")
                            .await?;
                        let (receiver, _) = self.split_obligations(receiver);
                        let Value::Array(items) = receiver else {
                            return Ok(Vec::new());
                        };

                        let mut deps = Vec::new();
                        for item in items.iter() {
                            deps.extend(self.collect_pending_shallow(item.clone()).await?);
                        }
                        Ok(Self::dedup_node_ids(deps))
                    }
                    BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver }) => {
                        let receiver = self
                            .materialize_value_shallow(*receiver, "forcing array.merge receiver")
                            .await?;
                        let (receiver, _) = self.split_obligations(receiver);
                        let Value::Array(items) = receiver else {
                            return Ok(Vec::new());
                        };

                        let mut deps = Vec::new();
                        for item in items.iter() {
                            deps.extend(self.collect_pending_deep(item.clone()).await?);
                        }
                        Ok(Self::dedup_node_ids(deps))
                    }
                    BuiltinValue::Method(BuiltinMethod::StringStartsWith { .. })
                    | BuiltinValue::Method(BuiltinMethod::StringContains { .. })
                    | BuiltinValue::Method(BuiltinMethod::StringSplit { .. })
                    | BuiltinValue::Method(BuiltinMethod::BlobFromString)
                    | BuiltinValue::Method(BuiltinMethod::BytesFromString)
                    | BuiltinValue::Method(BuiltinMethod::TreeFromPath)
                    | BuiltinValue::Method(BuiltinMethod::HostRead { .. })
                    | BuiltinValue::Method(BuiltinMethod::HostHash { .. })
                    | BuiltinValue::Method(BuiltinMethod::HostWhich { .. }) => Ok(
                        Self::dedup_node_ids(self.collect_pending_deep(args[0].clone()).await?),
                    ),
                    BuiltinValue::Method(BuiltinMethod::TreeOf) => {
                        let mut deps = self.collect_pending_shallow(args[0].clone()).await?;
                        if !deps.is_empty() {
                            return Ok(Self::dedup_node_ids(deps));
                        }

                        let value = self
                            .materialize_value_shallow(args[0].clone(), "forcing Tree.of input")
                            .await?;
                        let (value, _) = self.split_obligations(value);
                        let Value::Map(map) = value else {
                            return Ok(Vec::new());
                        };
                        for (key, _) in map.entries.iter() {
                            deps.extend(self.collect_pending_deep(key.clone()).await?);
                        }
                        Ok(Self::dedup_node_ids(deps))
                    }
                    BuiltinValue::Method(BuiltinMethod::TreeEmpty)
                    | BuiltinValue::Method(BuiltinMethod::StringLines { .. })
                    | BuiltinValue::Method(BuiltinMethod::StringTrim { .. })
                    | BuiltinValue::Method(BuiltinMethod::StringToLower { .. })
                    | BuiltinValue::Method(BuiltinMethod::BlobToString { .. })
                    | BuiltinValue::Method(BuiltinMethod::BlobExecutable { .. })
                    | BuiltinValue::Method(BuiltinMethod::BlobUtf8 { .. })
                    | BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { .. })
                    | BuiltinValue::Method(BuiltinMethod::BytesUtf8 { .. })
                    | BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { .. })
                    | BuiltinValue::Method(BuiltinMethod::BytesBlob { .. })
                    | BuiltinValue::Method(BuiltinMethod::StreamCollect { .. }) => Ok(Vec::new()),
                    BuiltinValue::Method(BuiltinMethod::TreeFile { .. })
                    | BuiltinValue::Method(BuiltinMethod::TreeSubtree { .. }) => Ok(
                        Self::dedup_node_ids(self.collect_pending_deep(args[0].clone()).await?),
                    ),
                    BuiltinValue::Method(BuiltinMethod::TreeMerge { .. }) => Ok(
                        Self::dedup_node_ids(self.collect_pending_shallow(args[0].clone()).await?),
                    ),
                    BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver }) => {
                        Ok(Self::dedup_node_ids(
                            self.collect_pending_shallow(receiver.as_ref().clone())
                                .await?,
                        ))
                    }
                    BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver }) => {
                        let receiver = self
                            .materialize_value_shallow(
                                *receiver,
                                "forcing option.unwrap_or receiver",
                            )
                            .await?;
                        let (receiver, _) = self.split_obligations(receiver);
                        let Value::Struct(option) = receiver else {
                            return Ok(Vec::new());
                        };
                        match option.type_name.as_ref() {
                            "Some" => Ok(Vec::new()),
                            "None" => Ok(Self::dedup_node_ids(
                                self.collect_pending_deep(args[0].clone()).await?,
                            )),
                            _ => Ok(Vec::new()),
                        }
                    }
                    BuiltinValue::Function(BuiltinFunction::Acquire)
                    | BuiltinValue::Function(BuiltinFunction::Stabilize)
                    | BuiltinValue::Function(BuiltinFunction::Render)
                    | BuiltinValue::Function(BuiltinFunction::Exec)
                    | BuiltinValue::Function(BuiltinFunction::Discover)
                    | BuiltinValue::Function(BuiltinFunction::Force)
                    | BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                        let mut deps = Vec::new();
                        for value in args {
                            deps.extend(self.collect_pending_deep(value.clone()).await?);
                        }
                        for (_, value) in kwargs {
                            deps.extend(self.collect_pending_deep(value.clone()).await?);
                        }
                        if let Some(kwargs_expr) = kwargs_expr {
                            deps.extend(self.collect_pending_deep(kwargs_expr.clone()).await?);
                        }
                        Ok(Self::dedup_node_ids(deps))
                    }
                    _ => Ok(Vec::new()),
                },
                _ => Ok(Vec::new()),
            }
        })
    }

    fn dedup_node_ids(ids: Vec<NodeId>) -> Vec<NodeId> {
        ids.into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn builtin_namespace_name(namespace: &BuiltinNamespace) -> &'static str {
        match namespace {
            BuiltinNamespace::Blob => "Blob",
            BuiltinNamespace::Bytes => "Bytes",
            BuiltinNamespace::Tree => "Tree",
        }
    }

    fn builtin_function_name(function: &BuiltinFunction) -> String {
        match function {
            BuiltinFunction::Acquire => "acquire".into(),
            BuiltinFunction::Stabilize => "stabilize".into(),
            BuiltinFunction::Render => "render".into(),
            BuiltinFunction::Exec => "exec".into(),
            BuiltinFunction::Discover => "discover".into(),
            BuiltinFunction::Force => "force".into(),
            BuiltinFunction::ExecutorClass => "Executor.class".into(),
            BuiltinFunction::StructConstructor { type_name, .. } => {
                format!("{type_name}.new")
            }
        }
    }

    fn builtin_method_name(method: &BuiltinMethod) -> &'static str {
        match method {
            BuiltinMethod::StringStartsWith { .. } => "String.starts_with",
            BuiltinMethod::StringContains { .. } => "String.contains",
            BuiltinMethod::StringSplit { .. } => "String.split",
            BuiltinMethod::StringLines { .. } => "String.lines",
            BuiltinMethod::StringTrim { .. } => "String.trim",
            BuiltinMethod::StringToLower { .. } => "String.to_lower",
            BuiltinMethod::BlobFromString => "Blob.from_string",
            BuiltinMethod::BytesFromString => "Bytes.from_string",
            BuiltinMethod::TreeOf => "Tree.of",
            BuiltinMethod::TreeEmpty => "Tree.empty",
            BuiltinMethod::TreeFromPath => "Tree.from_path",
            BuiltinMethod::ArrayMap { .. } => "Array.map",
            BuiltinMethod::ArrayToMap { .. } => "Array.to_map",
            BuiltinMethod::ArrayFlatten { .. } => "Array.flatten",
            BuiltinMethod::ArrayMerge { .. } => "Array.merge",
            BuiltinMethod::MapMapValues { .. } => "Map.map_values",
            BuiltinMethod::MapHas { .. } => "Map.has",
            BuiltinMethod::MapGet { .. } => "Map.get",
            BuiltinMethod::BlobToString { .. } => "Blob.to_string",
            BuiltinMethod::BlobExecutable { .. } => "Blob.executable",
            BuiltinMethod::BlobUtf8 { .. } => "Blob.utf8",
            BuiltinMethod::BlobUtf8Lossy { .. } => "Blob.utf8_lossy",
            BuiltinMethod::BytesUtf8 { .. } => "Bytes.utf8",
            BuiltinMethod::BytesUtf8Lossy { .. } => "Bytes.utf8_lossy",
            BuiltinMethod::BytesBlob { .. } => "Bytes.blob",
            BuiltinMethod::StreamCollect { .. } => "Stream.collect",
            BuiltinMethod::TreeFile { .. } => "Tree.file",
            BuiltinMethod::TreeMerge { .. } => "Tree.merge",
            BuiltinMethod::TreeSubtree { .. } => "Tree.subtree",
            BuiltinMethod::OptionUnwrapOr { .. } => "Option.unwrap_or",
            BuiltinMethod::FallibleCall { .. } => "Fallible.call",
            BuiltinMethod::HostOs { .. } => "Host.os",
            BuiltinMethod::HostArch { .. } => "Host.arch",
            BuiltinMethod::HostRead { .. } => "Host.read",
            BuiltinMethod::HostHash { .. } => "Host.hash",
            BuiltinMethod::HostWhich { .. } => "Host.which",
        }
    }

    fn value_trace_target(value: &Value) -> (String, String) {
        match value {
            Value::Function(function) => ("function".into(), function.def.name.clone()),
            Value::Builtin(BuiltinValue::Namespace(namespace)) => (
                "builtin_namespace".into(),
                Self::builtin_namespace_name(namespace).into(),
            ),
            Value::Builtin(BuiltinValue::Function(function)) => (
                "builtin_function".into(),
                Self::builtin_function_name(function),
            ),
            Value::Builtin(BuiltinValue::Method(method)) => (
                "builtin_method".into(),
                Self::builtin_method_name(method).into(),
            ),
            Value::Partial(partial) => {
                let (_, target_name) = Self::value_trace_target(&partial.function);
                ("partial".into(), target_name)
            }
            other => ("value".into(), other.type_name().into()),
        }
    }

    fn source_trace_detail(provenance: Option<&instr::SourceProvenance>) -> facet_value::Value {
        let Some(provenance) = provenance else {
            return value!({});
        };
        value!({
            "source_id": ((provenance.source_id.to_string())),
            "label": ((provenance.label.clone())),
            "line": ((provenance.span.start_line)),
            "column": ((provenance.span.start_column)),
            "end_line": ((provenance.span.end_line)),
            "end_column": ((provenance.span.end_column))
        })
    }

    fn job_trace_detail(node: &Node) -> facet_value::Value {
        match &node.job {
            NodeJob::Binary { op, .. } => value!({
                "operation": ((format!("{op:?}"))),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Unary { op, .. } => value!({
                "operation": ((format!("{op:?}"))),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Template { kind, parts } => value!({
                "template_kind": ((match kind {
                    TemplateKind::String => "string",
                    TemplateKind::Path => "path",
                })),
                "part_count": ((parts.len() as u64)),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::If { .. } => value!({
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Fallible { value } => value!({
                "value_kind": ((value.type_name())),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Demand { demands, .. } => value!({
                "demand_count": ((demands.len() as u64)),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Call {
                function,
                args,
                kwargs,
                kwargs_expr,
            } => {
                let (target_kind, target_name) = Self::value_trace_target(function);
                value!({
                    "target_kind": ((target_kind)),
                    "target_name": ((target_name)),
                    "target_source": ((match function {
                        Value::Function(function) => Self::source_trace_detail(function.def.provenance.as_ref()),
                        _ => value!({}),
                    })),
                    "arg_count": ((args.len() as u64)),
                    "kwarg_count": ((kwargs.len() as u64)),
                    "has_kwargs_expr": ((kwargs_expr.is_some())),
                    "pending_dep_count": ((node.job.pending_deps().len() as u64))
                })
            }
            NodeJob::Invoke { function, args } => value!({
                "target_kind": "function",
                "target_name": ((function.def.name.clone())),
                "target_source": ((Self::source_trace_detail(function.def.provenance.as_ref()))),
                "arg_count": ((args.len() as u64)),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Field {
                field, receiver, ..
            } => value!({
                "field": ((field.to_string())),
                "receiver_kind": ((receiver.type_name())),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Fail { message } => value!({
                "message_kind": ((message.type_name())),
                "pending_dep_count": ((node.job.pending_deps().len() as u64))
            }),
            NodeJob::Sleep { millis, value } => value!({
                "millis": ((*millis)),
                "value_kind": ((value.type_name()))
            }),
            NodeJob::Poison(_) => value!({}),
        }
    }

    fn node_display_name(node: &Node) -> String {
        match &node.job {
            NodeJob::Call { function, .. } => {
                let (_, target_name) = Self::value_trace_target(function);
                format!("{target_name}(..)")
            }
            NodeJob::Invoke { function, .. } => format!("{}(..)", function.def.name),
            NodeJob::Binary { op, .. } => format!("{op:?}"),
            NodeJob::Unary { op, .. } => format!("{op:?}"),
            NodeJob::Template { kind, .. } => match kind {
                TemplateKind::String => "string template".to_string(),
                TemplateKind::Path => "path template".to_string(),
            },
            NodeJob::If { .. } => "if".to_string(),
            NodeJob::Fallible { .. } => "fallible(..)".to_string(),
            NodeJob::Demand { demands, .. } => format!("demand[{}]", demands.len()),
            NodeJob::Field { field, .. } => format!(".{field}"),
            NodeJob::Fail { .. } => "fail(..)".to_string(),
            NodeJob::Sleep { millis, .. } => format!("sleep({millis}ms)"),
            NodeJob::Poison(_) => "poison".to_string(),
        }
    }

    fn node_trace_args_with_display(node: &Node, display_name: String) -> facet_value::Value {
        let provenance_source = node
            .provenance
            .as_ref()
            .map(|provenance| provenance.source_id.to_string())
            .unwrap_or_default();
        let provenance_label = node
            .provenance
            .as_ref()
            .map(|provenance| provenance.label.clone())
            .unwrap_or_default();
        let (source_line, source_column, source_end_line, source_end_column) = node
            .provenance
            .as_ref()
            .map(|provenance| {
                (
                    provenance.span.start_line,
                    provenance.span.start_column,
                    provenance.span.end_line,
                    provenance.span.end_column,
                )
            })
            .unwrap_or_default();
        let job_kind = match &node.job {
            NodeJob::Binary { .. } => "binary",
            NodeJob::Unary { .. } => "unary",
            NodeJob::Template { .. } => "template",
            NodeJob::If { .. } => "if",
            NodeJob::Fallible { .. } => "fallible",
            NodeJob::Demand { .. } => "demand",
            NodeJob::Call { .. } => "call",
            NodeJob::Invoke { .. } => "invoke",
            NodeJob::Field { .. } => "field",
            NodeJob::Fail { .. } => "fail",
            NodeJob::Sleep { .. } => "sleep",
            NodeJob::Poison(_) => "poison",
        };
        let detail = Self::job_trace_detail(node);

        value!({
            "display_name": ((display_name)),
            "node_id": ((node.id.0)),
            "description": ((node.description.clone())),
            "job_kind": ((job_kind)),
            "source_id": ((provenance_source)),
            "source_label": ((provenance_label)),
            "source_line": ((source_line)),
            "source_column": ((source_column)),
            "source_end_line": ((source_end_line)),
            "source_end_column": ((source_end_column)),
            "detail": ((detail))
        })
    }

    fn begin_node_phase_span(&self, phase: &'static str, node: &Node) -> ErasedSpan {
        let display_name = match phase {
            "node.inputs" => format!("inputs {}", Self::node_display_name(node)),
            "node.wait" => format!("wait {}", Self::node_display_name(node)),
            _ => Self::node_display_name(node),
        };
        if self.event_sink.supports_args() {
            self.event_sink.begin_span_with_args(
                phase,
                Self::node_trace_args_with_display(node, display_name),
            )
        } else {
            self.event_sink.begin_span(phase)
        }
    }

    pub async fn execute_pending_node_with_chain(
        &self,
        node_id: NodeId,
        chain: &[NodeId],
    ) -> Result<Value, PoisonError> {
        let node = self.node_ref(node_id)?;
        let (sender, receiver) = tokio::sync::oneshot::channel();

        match node.try_start_or_wait(Waiter { sender }) {
            Ok(()) => {
                let pending_deps = if node.job.should_preforce_inputs() {
                    node.job.pending_deps()
                } else {
                    Vec::new()
                };
                if !pending_deps.is_empty() {
                    let _span = self.begin_node_phase_span("node.inputs", &node);
                    let pending_values = pending_deps
                        .into_iter()
                        .map(Value::Pending)
                        .collect::<Vec<_>>();
                    if let Err(error) = self
                        .with_prefixed_demand_chain(chain, node_id, self.force_many(pending_values))
                        .await
                    {
                        node.poison(error.clone());
                        return Err(error);
                    }
                }
                let _span = self.begin_node_phase_span("node.execute", &node);
                let result = self
                    .with_prefixed_demand_chain(chain, node_id, self.execute_node(&node))
                    .await;
                match result {
                    Ok(value) => {
                        node.resolve(value.clone());
                        Ok(value)
                    }
                    Err(error) => {
                        node.poison(error.clone());
                        Err(error)
                    }
                }
            }
            Err(Some(Outcome::Value(value))) => {
                self.event_sink.instant("node.cache_hit");
                Ok(value)
            }
            Err(Some(Outcome::Poison(error))) => {
                self.event_sink.instant("node.cache_hit");
                Err(error)
            }
            Err(None) => {
                let _span = self.begin_node_phase_span("node.wait", &node);
                match receiver.await {
                    Ok((_node_id, Outcome::Value(value))) => Ok(value),
                    Ok((_node_id, Outcome::Poison(error))) => Err(error),
                    Err(_) => Err(self.internal_error("all scheduler waiters dropped")),
                }
            }
        }
    }

    async fn with_prefixed_demand_chain<T>(
        &self,
        chain: &[NodeId],
        node_id: NodeId,
        f: impl std::future::Future<Output = Result<T, PoisonError>>,
    ) -> Result<T, PoisonError> {
        DEMAND_STACK.with(|stack| stack.borrow_mut().extend(chain.iter().copied()));
        let result = self.with_demand(node_id, f).await;
        DEMAND_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            for expected in chain.iter().rev() {
                let popped = stack.pop();
                debug_assert_eq!(popped, Some(*expected));
            }
        });
        result
    }

    pub(crate) fn demand_chain_for_stack(
        &self,
        stack: &[NodeId],
        next: Option<NodeId>,
    ) -> DemandChain {
        let mut entries = Vec::new();
        for node_id in stack.iter().copied().chain(next) {
            if let Some(node) = self.nodes.get(&node_id) {
                entries.push(DemandEntry {
                    node_id,
                    description: node.description.clone(),
                    provenance: node.provenance.clone(),
                });
            }
        }
        DemandChain { entries }
    }

    pub(crate) fn circular_dependency_error(
        &self,
        stack: &[NodeId],
        node_id: NodeId,
    ) -> PoisonError {
        PoisonError::new(
            PoisonKind::CircularDependency,
            format!("circular dependency while demanding node {}", node_id.0),
            self.demand_chain_for_stack(stack, Some(node_id)),
        )
    }

    pub(crate) fn demand_depth_exceeded_error(
        &self,
        stack: &[NodeId],
        node_id: NodeId,
    ) -> PoisonError {
        PoisonError::new(
            PoisonKind::DemandDepthExceeded,
            format!(
                "demand chain exceeded configured depth limit of {}",
                self.max_demand_depth
            ),
            self.demand_chain_for_stack(stack, Some(node_id)),
        )
    }

    pub(crate) fn stalled_error(&self, stack: &[NodeId], pending: &[NodeId]) -> PoisonError {
        let list = pending
            .iter()
            .map(|node_id| node_id.0.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let details = pending
            .iter()
            .map(|node_id| self.format_stalled_node(*node_id, 3))
            .collect::<Vec<_>>()
            .join("; ");
        PoisonError::new(
            PoisonKind::Internal,
            format!("scheduler stalled while resolving pending nodes [{list}] ({details})"),
            self.demand_chain_for_stack(stack, None),
        )
    }

    fn format_stalled_node(&self, node_id: NodeId, depth: usize) -> String {
        let Some(node) = self.nodes.get(&node_id) else {
            return format!("{}:<missing>", node_id.0);
        };

        let state = node.state.lock();
        let state = match &*state {
            NodeState::Pending => "Pending".to_string(),
            NodeState::Evaluating { .. } => "Evaluating".to_string(),
            NodeState::Resolved(value) => format!("Resolved({})", value.type_name()),
            NodeState::Poisoned(error) => format!("Poisoned({})", error.message),
        };
        let deps = node
            .job
            .pending_deps()
            .into_iter()
            .map(|dep| {
                if depth == 0 {
                    dep.0.to_string()
                } else {
                    self.format_stalled_node(dep, depth - 1)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            "{}:{} state={} deps=[{}]",
            node_id.0, node.description, state, deps
        )
    }

    pub(crate) fn aggregate_poison_errors_for_stack(
        &self,
        context: &str,
        stack: &[NodeId],
        errors: Vec<PoisonError>,
    ) -> PoisonError {
        PoisonError::aggregate(context, self.demand_chain_for_stack(stack, None), errors)
    }

    fn node_ref(&self, node_id: NodeId) -> Result<Arc<Node>, PoisonError> {
        self.nodes
            .get(&node_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| self.internal_error(&format!("missing node {}", node_id.0)))
    }

    fn check_arity(&self, name: &str, args: &[Value], expected: usize) -> Result<(), PoisonError> {
        if args.len() != expected {
            return Err(self.type_error(&format!(
                "{name} expects {expected} argument(s), got {}",
                args.len()
            )));
        }
        Ok(())
    }

    fn require_no_kwargs(
        &self,
        name: &str,
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<(), PoisonError> {
        if kwargs.is_empty() {
            return Ok(());
        }
        let names = kwargs
            .iter()
            .map(|(name, _)| name.as_ref())
            .collect::<Vec<_>>()
            .join(", ");
        Err(self.type_error(&format!(
            "{name} does not accept keyword arguments ({names})"
        )))
    }

    fn stringify_scalar(&self, value: &Value) -> Result<String, PoisonError> {
        match value {
            Value::String(text) => Ok(text.to_string()),
            Value::Path(path) => Ok(path.as_str().to_string()),
            Value::Bool(_) | Value::U64(_) | Value::I64(_) | Value::F64(_) | Value::Unit => {
                Ok(value.to_string())
            }
            other => {
                Err(self.type_error(&format!("cannot turn {} into Blob text", other.type_name())))
            }
        }
    }

    fn expect_relative_path(&self, value: &Value, context: &str) -> Result<PathValue, PoisonError> {
        let path = self.expect_path_value(value, context)?;
        if path.as_str().starts_with('/') {
            return Err(self.type_error(&format!("{context} must be relative")));
        }
        Ok(path)
    }

    fn expect_path_value(&self, value: &Value, context: &str) -> Result<PathValue, PoisonError> {
        let (value, _) = self.split_obligations_ref(value);
        match value {
            Value::Path(path) => Ok(path.clone()),
            Value::String(path) => Ok(PathValue::new(path.as_ref())),
            other => Err(self.type_error(&format!(
                "{context} must be Path or String, got {}",
                other.type_name()
            ))),
        }
    }

    fn unresolved_name(&self, name: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::UnresolvedName,
            format!("unresolved name: {name}"),
            self.current_demand_chain(None),
        )
    }

    fn unresolved_function(&self, name: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::UnresolvedName,
            format!("no function named '{name}'"),
            self.current_demand_chain(None),
        )
    }

    fn type_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::TypeError,
            message,
            self.current_demand_chain(None),
        )
    }

    fn type_error_at(
        &self,
        message: &str,
        provenance: Option<instr::SourceProvenance>,
    ) -> PoisonError {
        let mut chain = self.current_demand_chain(None);
        if let Some(provenance) = provenance {
            chain.push(
                NodeId(u64::MAX),
                format!("{} (defined here)", provenance.label),
                Some(provenance),
            );
        }
        PoisonError::new(PoisonKind::TypeError, message, chain)
    }

    fn arithmetic_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::ArithmeticError,
            message,
            self.current_demand_chain(None),
        )
    }

    pub(crate) fn internal_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::Internal,
            message,
            self.current_demand_chain(None),
        )
    }

    fn aggregate_poison_errors(&self, context: &str, errors: Vec<PoisonError>) -> PoisonError {
        PoisonError::aggregate(context, self.current_demand_chain(None), errors)
    }

    fn current_demand_chain(&self, next: Option<NodeId>) -> DemandChain {
        let mut entries = Vec::new();
        DEMAND_STACK.with(|stack| {
            for node_id in stack.borrow().iter().copied().chain(next) {
                if let Some(node) = self.nodes.get(&node_id) {
                    entries.push(DemandEntry {
                        node_id,
                        description: node.description.clone(),
                        provenance: node.provenance.clone(),
                    });
                }
            }
        });
        DemandChain { entries }
    }

    fn current_demand_stack_ids(&self) -> Vec<NodeId> {
        DEMAND_STACK.with(|stack| stack.borrow().iter().copied().collect())
    }

    async fn force_pending_shallow(&self, node_id: NodeId) -> Result<Value, PoisonError> {
        Scheduler::with_initial_stack(self.arc_self(), self.current_demand_stack_ids())
            .resolve_node_shallow(node_id)
            .await
    }

    // r[impl eval.demand-depth]
    async fn with_demand<T>(
        &self,
        node_id: NodeId,
        f: impl std::future::Future<Output = Result<T, PoisonError>>,
    ) -> Result<T, PoisonError> {
        if DEMAND_STACK.with(|stack| stack.borrow().contains(&node_id)) {
            return Err(PoisonError::new(
                PoisonKind::CircularDependency,
                format!("circular dependency while demanding node {}", node_id.0),
                self.current_demand_chain(Some(node_id)),
            ));
        }

        let next_depth = DEMAND_STACK.with(|stack| stack.borrow().len() + 1);
        if next_depth > self.max_demand_depth {
            return Err(PoisonError::new(
                PoisonKind::DemandDepthExceeded,
                format!(
                    "demand chain exceeded configured depth limit of {}",
                    self.max_demand_depth
                ),
                self.current_demand_chain(Some(node_id)),
            ));
        }

        DEMAND_STACK.with(|stack| stack.borrow_mut().push(node_id));
        let result = f.await;
        DEMAND_STACK.with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some(node_id));
        });
        result
    }
}

impl BuildResult {
    fn from_result(result: Result<Value, PoisonError>) -> Self {
        match result {
            Ok(value) => Self::Value(value),
            Err(error) => Self::Poisoned(error),
        }
    }

    fn into_value(self) -> Result<Value, PoisonError> {
        match self {
            BuildResult::Value(value) | BuildResult::Returned(value) => Ok(value),
            BuildResult::Poisoned(error) => Err(error),
        }
    }
}
