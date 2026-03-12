//! Demand-driven evaluation.
//!
//! Expressions build lazy value graphs. Forcing only happens at the
//! external boundary or inside a node whose operation truly needs a
//! resolved dependency.

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
use std::sync::Arc;
use std::thread;
#[cfg(test)]
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tempfile::TempDir;

use crate::exec::{
    DiscoverRequest, ExecArgValue, ExecMount, ExecRequest, SharedDiscoverHandler, SharedExecHandler,
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
    ExecPathValue, FunctionValue, HostObservation, HostValue, MapValue, PartialValue, PathValue,
    StructValue, TreeValue, Value,
};

thread_local! {
    static DEMAND_STACK: RefCell<Vec<NodeId>> = const { RefCell::new(Vec::new()) };
    static DISCOVERY_OBSERVATIONS: RefCell<Vec<Vec<HostObservation>>> = const { RefCell::new(Vec::new()) };
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
    Resolved(Value),
    Poisoned(PoisonError),
}

#[derive(Clone)]
struct ParsedExecRequest {
    args: Vec<ExecArgValue>,
    env: Vec<(String, ExecArgValue)>,
    mounts: Vec<(PathValue, Value)>,
    writable_roots: Vec<PathValue>,
    observer: Value,
    verification_obligations: Vec<HostObservation>,
}

/// The evaluation engine.
pub struct Engine {
    module: instr::Module,
    nodes: DashMap<NodeId, Arc<Node>>,
    call_memo: DashMap<NodeKey, Arc<Node>>,
    functions: HashMap<String, Vec<Arc<instr::FnDef>>>,
    function_identities: HashMap<usize, Arc<str>>,
    max_demand_depth: usize,
    exec_handler: Option<SharedExecHandler>,
    discover_handler: Option<SharedDiscoverHandler>,
}

impl Engine {
    /// Create a new engine from a lowered module.
    pub fn from_module(module: &instr::Module) -> Self {
        let mut functions = HashMap::new();
        for func in &module.functions {
            functions
                .entry(func.name.clone())
                .or_insert_with(Vec::new)
                .push(func.clone());
        }
        let function_identities = compute_function_identities(&module.functions);

        Self {
            module: module.clone(),
            nodes: DashMap::new(),
            call_memo: DashMap::new(),
            functions,
            function_identities,
            max_demand_depth: DEFAULT_MAX_DEMAND_DEPTH,
            exec_handler: None,
            discover_handler: None,
        }
    }

    // r[impl eval.demand-depth]
    pub fn with_max_demand_depth(mut self, max_demand_depth: usize) -> Self {
        self.max_demand_depth = max_demand_depth;
        self
    }

    pub fn with_exec_handler(mut self, exec_handler: SharedExecHandler) -> Self {
        self.exec_handler = Some(exec_handler);
        self
    }

    pub fn with_discover_handler(mut self, discover_handler: SharedDiscoverHandler) -> Self {
        self.discover_handler = Some(discover_handler);
        self
    }

    // r[impl eval.forcing.boundary]
    /// Evaluate a top-level function by name at the external forcing boundary.
    pub fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        let function = self.resolve_module_function(name)?;
        let value = self.build_call(Value::Function(function), args, None);
        self.resolve_boundary_value(value)
    }

    pub fn transport_module(&self) -> TransportModule {
        TransportModule::from(&self.module)
    }

    pub fn ship_value(&self, value: Value) -> Result<TransportValue, PoisonError> {
        let value = self.resolve_boundary_value(value)?;
        TransportValue::try_from(&value).map_err(|message| self.type_error(&message))
    }

    pub fn ship_function(&self, value: Value) -> Result<ShippedFunction, PoisonError> {
        let value = self.resolve_boundary_value(value)?;
        let Value::Function(function) = value else {
            return Err(self.type_error("shipped callbacks must be functions"));
        };
        ShippedFunction::try_from(&function).map_err(|message| self.type_error(&message))
    }

    pub fn invoke_shipped(
        module: &TransportModule,
        function: &ShippedFunction,
        args: Vec<Value>,
    ) -> Result<Value, PoisonError> {
        let module: instr::Module = module.clone().into();
        let engine = Self::from_module(&module);
        let function = function
            .to_runtime()
            .map_err(|message| engine.type_error(&message))?;
        let value = engine.execute_call(
            Value::Function(function),
            Arc::from(args),
            Arc::from([]),
            None,
        )?;
        engine.resolve_boundary_value(value)
    }

    pub fn invoke_shipped_discover(
        module: &TransportModule,
        function: &ShippedFunction,
    ) -> Result<Value, PoisonError> {
        let module: instr::Module = module.clone().into();
        let engine = Self::from_module(&module);
        let function = function
            .to_runtime()
            .map_err(|message| engine.type_error(&message))?;
        engine.invoke_discover_probe(Value::Function(function))
    }

    fn resolve_boundary_value(&self, value: Value) -> Result<Value, PoisonError> {
        Scheduler::new(self).resolve_value(value)
    }

    #[cfg(test)]
    pub(crate) fn call_unforced(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        let function = self.resolve_module_function(name)?;
        Ok(self.build_call(Value::Function(function), args, None))
    }

    #[cfg(test)]
    pub(crate) fn force_shallow(&self, value: Value) -> Result<Value, PoisonError> {
        self.force_value_shallow(value)
    }

    #[cfg(test)]
    pub(crate) fn force_fully(&self, value: Value) -> Result<Value, PoisonError> {
        self.force_value(value)
    }

    #[cfg(test)]
    pub(crate) fn poison_value(&self, message: &str) -> Value {
        let error = PoisonError::new(PoisonKind::UserFail, message, DemandChain::new());
        self.pending_value(NodeJob::Poison(error), format!("poison({message})"), None)
    }

    #[cfg(test)]
    pub(crate) fn memoized_call_count(&self) -> usize {
        self.call_memo.len()
    }

    #[cfg(test)]
    pub(crate) fn sleep_value(&self, millis: u64, value: Value) -> Value {
        self.pending_value(
            NodeJob::Sleep { millis, value },
            format!("sleep({millis}ms)"),
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn function_key(&self, name: &str) -> Result<String, PoisonError> {
        self.resolve_module_function(name)?
            .key()
            .ok_or_else(|| self.internal_error("top-level function key should be resolved"))
    }

    #[cfg(test)]
    pub(crate) fn function_identity_string(&self, name: &str) -> Result<String, PoisonError> {
        Ok(self.resolve_module_function(name)?.identity())
    }

    // r[impl eval.entry]
    pub fn call_entry(
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

        let value = self.call(name, vec![workspace, args, env])?;
        match value {
            Value::Tree(_) => Ok(value),
            other => Err(self.type_error(&format!(
                "build entry must return Tree, got {}",
                other.type_name()
            ))),
        }
    }

    fn begin_discovery_recording(&self) {
        DISCOVERY_OBSERVATIONS.with(|stack| stack.borrow_mut().push(Vec::new()));
    }

    fn finish_discovery_recording(&self) -> Vec<HostObservation> {
        DISCOVERY_OBSERVATIONS.with(|stack| stack.borrow_mut().pop().unwrap_or_default())
    }

    fn record_discovery_observation(&self, observation: HostObservation) {
        DISCOVERY_OBSERVATIONS.with(|stack| {
            if let Some(current) = stack.borrow_mut().last_mut() {
                current.push(observation);
            }
        });
    }

    fn discovery_recording_active(&self) -> bool {
        DISCOVERY_OBSERVATIONS.with(|stack| !stack.borrow().is_empty())
    }

    fn invoke_discover_probe(&self, probe: Value) -> Result<Value, PoisonError> {
        let probe = self.force_value(probe)?;
        match probe {
            Value::Function(_) | Value::Builtin(_) => {}
            other => {
                return Err(self.type_error(&format!(
                    "discover probe must be callable, got {}",
                    other.type_name()
                )));
            }
        }

        self.begin_discovery_recording();
        let result = (|| {
            let value = self.execute_call(
                probe,
                Arc::from([Value::Host(HostValue)]),
                Arc::from([]),
                None,
            )?;
            self.force_value(value)
        })();
        let observations = self.finish_discovery_recording();
        result.map(|value| self.attach_obligations(value, observations))
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
        let inner = match value {
            Value::Discovered(discovered) => {
                Self::add_obligations(&mut obligations, discovered.obligations.iter().cloned());
                *discovered.value
            }
            other => other,
        };
        Self::add_obligations(&mut obligations, items);
        if obligations.is_empty() {
            inner
        } else {
            Value::Discovered(DiscoveredValue {
                value: Box::new(inner),
                obligations: Arc::from(obligations.into_values().collect::<Vec<_>>()),
            })
        }
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
                BuiltinMethod::ArrayMap { receiver }
                | BuiltinMethod::ArrayToMap { receiver }
                | BuiltinMethod::ArrayFlatten { receiver }
                | BuiltinMethod::ArrayMerge { receiver }
                | BuiltinMethod::MapMapValues { receiver }
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

    fn global_function_value(&self, def: Arc<instr::FnDef>) -> FunctionValue {
        FunctionValue {
            code_identity: self.function_identity(&def),
            def,
            captures: Arc::from([]),
        }
    }

    fn local_function_value(
        &self,
        def: Arc<instr::FnDef>,
        captures: Arc<[(Arc<str>, Value)]>,
    ) -> FunctionValue {
        FunctionValue {
            code_identity: self.function_identity(&def),
            def,
            captures,
        }
    }

    fn closure_function_value(
        &self,
        def: Arc<instr::FnDef>,
        code_identity: Arc<str>,
        captures: Arc<[(Arc<str>, Value)]>,
    ) -> FunctionValue {
        FunctionValue {
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

    fn function_identity(&self, def: &Arc<instr::FnDef>) -> Arc<str> {
        self.function_identities
            .get(&fn_ptr(def))
            .cloned()
            .unwrap_or_else(|| Arc::from(stable_fn_repr(def)))
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
            && args.iter().all(|value| value.key().is_some())
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
                    .map(|value| value.key().unwrap_or_else(|| "<pending>".into()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        };

        match self.call_memo.entry(key) {
            Entry::Occupied(entry) => entry.get().id,
            Entry::Vacant(entry) => {
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

    pub(crate) fn scheduler_max_demand_depth(&self) -> usize {
        self.max_demand_depth
    }

    pub(crate) fn scheduler_node_state(
        &self,
        node_id: NodeId,
    ) -> Result<SchedulerNodeState, PoisonError> {
        let node = self.scheduler_node(node_id)?;
        let state = node.state.lock();
        match &*state {
            NodeState::Pending | NodeState::Evaluating { .. } => Ok(SchedulerNodeState::Pending),
            NodeState::Resolved(value) => Ok(SchedulerNodeState::Resolved(value.clone())),
            NodeState::Poisoned(error) => Ok(SchedulerNodeState::Poisoned(error.clone())),
        }
    }

    pub(crate) fn scheduler_node_deps(&self, node_id: NodeId) -> Result<Vec<NodeId>, PoisonError> {
        let node = self.scheduler_node(node_id)?;
        let deps = match &node.job {
            NodeJob::Binary { left, right, .. } => {
                let mut deps = self.scheduler_pending_deep(left.clone())?;
                deps.extend(self.scheduler_pending_deep(right.clone())?);
                deps
            }
            NodeJob::Unary { operand, .. } => self.scheduler_pending_deep(operand.clone())?,
            NodeJob::Template { parts, .. } => {
                let mut deps = Vec::new();
                for part in parts.iter() {
                    if let TemplatePartValue::Expr(value) = part {
                        deps.extend(self.scheduler_pending_deep(value.clone())?);
                    }
                }
                deps
            }
            NodeJob::If { cond, .. } => self.scheduler_pending_deep(cond.clone())?,
            NodeJob::Fallible { value } => self.scheduler_pending_shallow(value.clone())?,
            NodeJob::Demand { demands, .. } => {
                let mut deps = Vec::new();
                for value in demands.iter() {
                    deps.extend(self.scheduler_pending_deep(value.clone())?);
                }
                deps
            }
            NodeJob::Fail { message } => self.scheduler_pending_deep(message.clone())?,
            NodeJob::Call {
                function,
                args,
                kwargs,
                kwargs_expr,
            } => {
                let mut deps = self.scheduler_callable_pending(function.clone())?;
                if deps.is_empty() {
                    deps.extend(self.scheduler_builtin_call_deps(
                        function.clone(),
                        args.as_ref(),
                        kwargs.as_ref(),
                        kwargs_expr.as_ref(),
                    )?);
                } else if let Some(kwargs_expr) = kwargs_expr {
                    deps.extend(self.scheduler_pending_shallow(kwargs_expr.clone())?);
                }
                deps
            }
            NodeJob::Invoke { .. } => Vec::new(),
            NodeJob::Field { receiver, .. } => self.scheduler_pending_shallow(receiver.clone())?,
            #[cfg(test)]
            NodeJob::Sleep { .. } | NodeJob::Poison(_) => Vec::new(),
        };
        Ok(Self::dedup_node_ids(deps))
    }

    fn scheduler_pending_deep(&self, value: Value) -> Result<Vec<NodeId>, PoisonError> {
        Ok(self
            .inspect_scheduled_value(value)?
            .pending
            .into_iter()
            .collect())
    }

    fn scheduler_pending_shallow(&self, value: Value) -> Result<Vec<NodeId>, PoisonError> {
        self.scheduler_pending_shallow_inner(value, &mut Vec::new())
    }

    fn scheduler_pending_shallow_inner(
        &self,
        value: Value,
        visiting: &mut Vec<NodeId>,
    ) -> Result<Vec<NodeId>, PoisonError> {
        match value {
            Value::Pending(node_id) => match self.scheduler_node_state(node_id)? {
                SchedulerNodeState::Pending => Ok(vec![node_id]),
                SchedulerNodeState::Resolved(value) => {
                    if visiting.contains(&node_id) {
                        return Err(self.scheduler_circular_dependency(visiting, node_id));
                    }
                    visiting.push(node_id);
                    let result = self.scheduler_pending_shallow_inner(value, visiting);
                    let popped = visiting.pop();
                    debug_assert_eq!(popped, Some(node_id));
                    result
                }
                SchedulerNodeState::Poisoned(_) => Ok(Vec::new()),
            },
            Value::Discovered(discovered) => {
                self.scheduler_pending_shallow_inner(*discovered.value, visiting)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn scheduler_callable_pending(&self, value: Value) -> Result<Vec<NodeId>, PoisonError> {
        self.scheduler_callable_pending_inner(value, &mut Vec::new())
    }

    fn scheduler_callable_pending_inner(
        &self,
        value: Value,
        visiting: &mut Vec<NodeId>,
    ) -> Result<Vec<NodeId>, PoisonError> {
        match value {
            Value::Pending(node_id) => match self.scheduler_node_state(node_id)? {
                SchedulerNodeState::Pending => Ok(vec![node_id]),
                SchedulerNodeState::Resolved(value) => {
                    if visiting.contains(&node_id) {
                        return Err(self.scheduler_circular_dependency(visiting, node_id));
                    }
                    visiting.push(node_id);
                    let result = self.scheduler_callable_pending_inner(value, visiting);
                    let popped = visiting.pop();
                    debug_assert_eq!(popped, Some(node_id));
                    result
                }
                SchedulerNodeState::Poisoned(_) => Ok(Vec::new()),
            },
            Value::Discovered(discovered) => {
                self.scheduler_callable_pending_inner(*discovered.value, visiting)
            }
            Value::Partial(partial) => {
                self.scheduler_callable_pending_inner(partial.function.clone(), visiting)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn scheduler_builtin_call_deps(
        &self,
        function: Value,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<&Value>,
    ) -> Result<Vec<NodeId>, PoisonError> {
        let function =
            self.materialize_scheduled_value_shallow(function, "forcing callable surface")?;
        let (function, _) = self.split_obligations(function);
        match function {
            Value::Partial(partial) => {
                let mut deps =
                    self.scheduler_builtin_call_deps(partial.function.clone(), &[], &[], None)?;
                for value in partial.args.iter().chain(args.iter()) {
                    deps.extend(self.scheduler_pending_deep(value.clone())?);
                }
                for (_, value) in partial.kwargs.iter().chain(kwargs.iter()) {
                    deps.extend(self.scheduler_pending_deep(value.clone())?);
                }
                if let Some(kwargs_expr) = &partial.kwargs_expr {
                    deps.extend(self.scheduler_pending_deep((**kwargs_expr).clone())?);
                }
                if let Some(kwargs_expr) = kwargs_expr {
                    deps.extend(self.scheduler_pending_deep(kwargs_expr.clone())?);
                }
                Ok(Self::dedup_node_ids(deps))
            }
            Value::Builtin(builtin) => match builtin {
                BuiltinValue::Namespace(BuiltinNamespace::Blob)
                | BuiltinValue::Namespace(BuiltinNamespace::Bytes) => Ok(Self::dedup_node_ids(
                    self.scheduler_pending_deep(args[0].clone())?,
                )),
                BuiltinValue::Namespace(BuiltinNamespace::Tree) => {
                    let mut deps = self.scheduler_pending_shallow(args[0].clone())?;
                    if !deps.is_empty() {
                        return Ok(Self::dedup_node_ids(deps));
                    }

                    let value = self.materialize_scheduled_value_shallow(
                        args[0].clone(),
                        "forcing Tree input",
                    )?;
                    let (value, _) = self.split_obligations(value);
                    match value {
                        Value::Map(map) => {
                            for (key, _) in map.entries.iter() {
                                deps.extend(self.scheduler_pending_deep(key.clone())?);
                            }
                        }
                        Value::Array(items) => {
                            for item in items.iter() {
                                deps.extend(self.scheduler_pending_deep(item.clone())?);
                            }
                        }
                        Value::Path(_) | Value::String(_) => {}
                        _ => {}
                    }
                    Ok(Self::dedup_node_ids(deps))
                }
                BuiltinValue::Function(BuiltinFunction::StructConstructor { .. }) => Ok(
                    Self::dedup_node_ids(self.scheduler_pending_shallow(args[0].clone())?),
                ),
                BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver }) => {
                    let receiver = self.materialize_scheduled_value_shallow(
                        *receiver,
                        "forcing array.flatten receiver",
                    )?;
                    let (receiver, _) = self.split_obligations(receiver);
                    let Value::Array(items) = receiver else {
                        return Ok(Vec::new());
                    };

                    let mut deps = Vec::new();
                    for item in items.iter() {
                        deps.extend(self.scheduler_pending_shallow(item.clone())?);
                    }
                    Ok(Self::dedup_node_ids(deps))
                }
                BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver }) => {
                    let receiver = self.materialize_scheduled_value_shallow(
                        *receiver,
                        "forcing array.merge receiver",
                    )?;
                    let (receiver, _) = self.split_obligations(receiver);
                    let Value::Array(items) = receiver else {
                        return Ok(Vec::new());
                    };

                    let mut deps = Vec::new();
                    for item in items.iter() {
                        deps.extend(self.scheduler_pending_deep(item.clone())?);
                    }
                    Ok(Self::dedup_node_ids(deps))
                }
                BuiltinValue::Method(BuiltinMethod::BlobFromString)
                | BuiltinValue::Method(BuiltinMethod::BytesFromString)
                | BuiltinValue::Method(BuiltinMethod::TreeFromPath)
                | BuiltinValue::Method(BuiltinMethod::HostRead { .. })
                | BuiltinValue::Method(BuiltinMethod::HostHash { .. })
                | BuiltinValue::Method(BuiltinMethod::HostWhich { .. }) => Ok(
                    Self::dedup_node_ids(self.scheduler_pending_deep(args[0].clone())?),
                ),
                BuiltinValue::Method(BuiltinMethod::TreeOf) => {
                    let mut deps = self.scheduler_pending_shallow(args[0].clone())?;
                    if !deps.is_empty() {
                        return Ok(Self::dedup_node_ids(deps));
                    }

                    let value = self.materialize_scheduled_value_shallow(
                        args[0].clone(),
                        "forcing Tree.of input",
                    )?;
                    let (value, _) = self.split_obligations(value);
                    let Value::Map(map) = value else {
                        return Ok(Vec::new());
                    };
                    for (key, _) in map.entries.iter() {
                        deps.extend(self.scheduler_pending_deep(key.clone())?);
                    }
                    Ok(Self::dedup_node_ids(deps))
                }
                BuiltinValue::Method(BuiltinMethod::TreeEmpty)
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
                    Self::dedup_node_ids(self.scheduler_pending_deep(args[0].clone())?),
                ),
                BuiltinValue::Method(BuiltinMethod::TreeMerge { .. }) => Ok(Self::dedup_node_ids(
                    self.scheduler_pending_shallow(args[0].clone())?,
                )),
                BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver }) => {
                    Ok(Self::dedup_node_ids(
                        self.scheduler_pending_shallow(receiver.as_ref().clone())?,
                    ))
                }
                BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver }) => {
                    let receiver = self.materialize_scheduled_value_shallow(
                        *receiver,
                        "forcing option.unwrap_or receiver",
                    )?;
                    let (receiver, _) = self.split_obligations(receiver);
                    let Value::Struct(option) = receiver else {
                        return Ok(Vec::new());
                    };
                    match option.type_name.as_ref() {
                        "Some" => Ok(Vec::new()),
                        "None" => Ok(Self::dedup_node_ids(
                            self.scheduler_pending_deep(args[0].clone())?,
                        )),
                        _ => Ok(Vec::new()),
                    }
                }
                BuiltinValue::Function(BuiltinFunction::Exec)
                | BuiltinValue::Function(BuiltinFunction::Discover)
                | BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                    let mut deps = Vec::new();
                    for value in args {
                        deps.extend(self.scheduler_pending_deep(value.clone())?);
                    }
                    for (_, value) in kwargs {
                        deps.extend(self.scheduler_pending_deep(value.clone())?);
                    }
                    if let Some(kwargs_expr) = kwargs_expr {
                        deps.extend(self.scheduler_pending_deep(kwargs_expr.clone())?);
                    }
                    Ok(Self::dedup_node_ids(deps))
                }
                _ => Ok(Vec::new()),
            },
            _ => Ok(Vec::new()),
        }
    }

    fn dedup_node_ids(ids: Vec<NodeId>) -> Vec<NodeId> {
        ids.into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn scheduler_execute_pending_node_with_chain(
        &self,
        node_id: NodeId,
        chain: &[NodeId],
    ) -> Result<Value, PoisonError> {
        let node = self.scheduler_node(node_id)?;
        let (sender, receiver) = crossbeam_channel::bounded(1);

        match node.try_start_or_wait(Waiter { sender }) {
            Ok(()) => {
                let result = self.with_scheduler_demand_chain(chain, node_id, || {
                    self.execute_node_scheduled(&node)
                });
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
            Err(Some(Outcome::Value(value))) => Ok(value),
            Err(Some(Outcome::Poison(error))) => Err(error),
            Err(None) => match receiver.recv() {
                Ok((_node_id, Outcome::Value(value))) => Ok(value),
                Ok((_node_id, Outcome::Poison(error))) => Err(error),
                Err(_) => Err(self.internal_error("all scheduler waiters dropped")),
            },
        }
    }

    fn with_scheduler_demand_chain<T>(
        &self,
        chain: &[NodeId],
        node_id: NodeId,
        f: impl FnOnce() -> Result<T, PoisonError>,
    ) -> Result<T, PoisonError> {
        DEMAND_STACK.with(|stack| stack.borrow_mut().extend(chain.iter().copied()));
        let result = self.with_demand(node_id, f);
        DEMAND_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            for expected in chain.iter().rev() {
                let popped = stack.pop();
                debug_assert_eq!(popped, Some(*expected));
            }
        });
        result
    }

    pub(crate) fn scheduler_demand_chain(
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

    pub(crate) fn scheduler_circular_dependency(
        &self,
        stack: &[NodeId],
        node_id: NodeId,
    ) -> PoisonError {
        PoisonError::new(
            PoisonKind::CircularDependency,
            format!("circular dependency while demanding node {}", node_id.0),
            self.scheduler_demand_chain(stack, Some(node_id)),
        )
    }

    pub(crate) fn scheduler_demand_depth_exceeded(
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
            self.scheduler_demand_chain(stack, Some(node_id)),
        )
    }

    pub(crate) fn scheduler_stalled(&self, stack: &[NodeId], pending: &[NodeId]) -> PoisonError {
        let list = pending
            .iter()
            .map(|node_id| node_id.0.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        PoisonError::new(
            PoisonKind::Internal,
            format!("scheduler stalled while resolving pending nodes [{list}]"),
            self.scheduler_demand_chain(stack, None),
        )
    }

    pub(crate) fn scheduler_aggregate_poison_errors(
        &self,
        context: &str,
        stack: &[NodeId],
        errors: Vec<PoisonError>,
    ) -> PoisonError {
        PoisonError::aggregate(context, self.scheduler_demand_chain(stack, None), errors)
    }

    fn scheduler_node(&self, node_id: NodeId) -> Result<Arc<Node>, PoisonError> {
        self.nodes
            .get(&node_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| self.internal_error(&format!("missing node {}", node_id.0)))
    }

    fn force_value_shallow(&self, value: Value) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => self.force_pending_shallow(node_id),
            Value::Discovered(discovered) => {
                let inner = self.force_value_shallow(*discovered.value)?;
                Ok(self.attach_obligations(inner, discovered.obligations.iter().cloned()))
            }
            other => Ok(other),
        }
    }

    fn force_selector_receiver(&self, value: Value) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => {
                let value = self.force_pending_shallow(node_id)?;
                self.force_selector_receiver(value)
            }
            Value::Discovered(discovered) => {
                let inner = self.force_selector_receiver(*discovered.value)?;
                Ok(self.attach_obligations(inner, discovered.obligations.iter().cloned()))
            }
            other => Ok(other),
        }
    }

    // r[impl eval.forcing.boundary]
    fn force_value(&self, value: Value) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => self.with_demand(node_id, || {
                let value = self.force_node(node_id)?;
                self.force_value(value)
            }),
            Value::Discovered(discovered) => {
                let inner = self.force_value(*discovered.value)?;
                Ok(self.attach_obligations(inner, discovered.obligations.iter().cloned()))
            }
            Value::Tuple(elems) => {
                let forced = self.force_many(elems.iter().cloned().collect())?;
                Ok(Value::Tuple(Arc::from(forced)))
            }
            Value::Array(items) => {
                let forced = self.force_many(items.iter().cloned().collect())?;
                Ok(Value::Array(Arc::from(forced)))
            }
            Value::Map(map) => {
                let keys = map
                    .entries
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                let values = map
                    .entries
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect::<Vec<_>>();
                let forced_keys = self.force_many(keys)?;
                let forced_values = self.force_many(values)?;
                Ok(Value::Map(MapValue {
                    entries: Arc::from(
                        forced_keys
                            .into_iter()
                            .zip(forced_values)
                            .collect::<Vec<_>>(),
                    ),
                }))
            }
            Value::Tree(tree) => match tree {
                TreeValue::Inline(entries) => {
                    let names = entries
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>();
                    let values = entries
                        .iter()
                        .map(|(_, value)| value.clone())
                        .collect::<Vec<_>>();
                    let forced = self.force_many(values)?;
                    self.validate_tree_entries(&forced)?;
                    Ok(Value::Tree(TreeValue::Inline(Arc::from(
                        names.into_iter().zip(forced).collect::<Vec<_>>(),
                    ))))
                }
                TreeValue::FsDir(path) => Ok(Value::Tree(TreeValue::FsDir(path))),
                TreeValue::Store(root) => Ok(Value::Tree(TreeValue::Store(root))),
            },
            Value::Struct(struct_value) => {
                let names = struct_value
                    .fields
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                let values = struct_value
                    .fields
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect::<Vec<_>>();
                let forced = self.force_many(values)?;
                let fields = names.into_iter().zip(forced).collect::<Vec<_>>();
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
            other => Ok(other),
        }
    }

    // r[impl eval.concurrency]
    // r[impl error.propagation]
    fn force_many(&self, values: Vec<Value>) -> Result<Vec<Value>, PoisonError> {
        if values.len() <= 1 {
            let mut forced = Vec::with_capacity(values.len());
            let mut errors = Vec::new();
            for value in values {
                match self.force_value(value) {
                    Ok(value) => forced.push(value),
                    Err(error) => errors.push(error),
                }
            }
            return if errors.is_empty() {
                Ok(forced)
            } else {
                Err(self.aggregate_poison_errors("forcing dependency", errors))
            };
        }

        thread::scope(|scope| {
            let handles = values
                .into_iter()
                .map(|value| scope.spawn(move || self.force_value(value)))
                .collect::<Vec<_>>();

            let mut forced = Vec::with_capacity(handles.len());
            let mut errors = Vec::new();
            for handle in handles {
                match handle.join() {
                    Ok(Ok(value)) => forced.push(value),
                    Ok(Err(error)) => errors.push(error),
                    Err(_) => errors.push(self.internal_error("force worker panicked")),
                }
            }
            if errors.is_empty() {
                Ok(forced)
            } else {
                Err(self.aggregate_poison_errors("forcing dependencies", errors))
            }
        })
    }

    fn force_many_shallow(&self, values: Vec<Value>) -> Result<Vec<Value>, PoisonError> {
        if values.len() <= 1 {
            let mut forced = Vec::with_capacity(values.len());
            let mut errors = Vec::new();
            for value in values {
                match self.force_value_shallow(value) {
                    Ok(value) => forced.push(value),
                    Err(error) => errors.push(error),
                }
            }
            return if errors.is_empty() {
                Ok(forced)
            } else {
                Err(self.aggregate_poison_errors("forcing shallow dependency", errors))
            };
        }

        thread::scope(|scope| {
            let handles = values
                .into_iter()
                .map(|value| scope.spawn(move || self.force_value_shallow(value)))
                .collect::<Vec<_>>();

            let mut forced = Vec::with_capacity(handles.len());
            let mut errors = Vec::new();
            for handle in handles {
                match handle.join() {
                    Ok(Ok(value)) => forced.push(value),
                    Ok(Err(error)) => errors.push(error),
                    Err(_) => errors.push(self.internal_error("force shallow worker panicked")),
                }
            }
            if errors.is_empty() {
                Ok(forced)
            } else {
                Err(self.aggregate_poison_errors("forcing shallow dependencies", errors))
            }
        })
    }

    fn force_node(&self, node_id: NodeId) -> Result<Value, PoisonError> {
        let node = self
            .nodes
            .get(&node_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| self.internal_error(&format!("missing node {}", node_id.0)))?;
        let (sender, receiver) = crossbeam_channel::bounded(1);

        match node.try_start_or_wait(Waiter { sender }) {
            Ok(()) => {
                let result = self.execute_node(&node);
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
            Err(Some(Outcome::Value(value))) => Ok(value),
            Err(Some(Outcome::Poison(error))) => Err(error),
            Err(None) => match receiver.recv() {
                Ok((_node_id, Outcome::Value(value))) => Ok(value),
                Ok((_node_id, Outcome::Poison(error))) => Err(error),
                Err(_) => Err(self.internal_error("all waiters dropped")),
            },
        }
    }

    fn execute_node(&self, node: &Arc<Node>) -> Result<Value, PoisonError> {
        match &node.job {
            NodeJob::Binary { op, left, right } => {
                let mut values = self.force_many(vec![left.clone(), right.clone()])?;
                let right = values.pop().expect("pair has two values");
                let left = values.pop().expect("pair has two values");
                self.compute_binary(*op, &left, &right)
            }
            NodeJob::Unary { op, operand } => {
                let operand = self.force_value(operand.clone())?;
                self.compute_unary(*op, &operand)
            }
            NodeJob::Template { kind, parts } => {
                let values = self.force_template_parts(parts)?;
                match kind {
                    TemplateKind::String => self.interpolate_string_parts(&values),
                    TemplateKind::Path => self.interpolate_path_parts(&values),
                }
            }
            NodeJob::If {
                cond,
                then_branch,
                else_branch,
                bindings,
                scope_identity,
            } => {
                let cond = self.force_value(cond.clone())?;
                match cond {
                    Value::Bool(true) => self
                        .eval_block(then_branch, bindings.as_ref(), scope_identity.as_ref())
                        .into_value(),
                    Value::Bool(false) => self
                        .eval_block(else_branch, bindings.as_ref(), scope_identity.as_ref())
                        .into_value(),
                    other => Err(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            NodeJob::Fallible { value } => match self.force_value_shallow(value.clone()) {
                Ok(value) => Ok(self.build_fallible_value(value)),
                Err(_) => Ok(option_none()),
            },
            NodeJob::Demand { demands, result } => {
                if self.discovery_recording_active() {
                    let _ = self.force_value(Value::Array(Arc::clone(demands)))?;
                } else {
                    let _ = self.resolve_boundary_value(Value::Array(Arc::clone(demands)))?;
                }
                result.clone()
            }
            NodeJob::Call {
                function,
                args,
                kwargs,
                kwargs_expr,
            } => self.execute_call(
                function.clone(),
                args.clone(),
                kwargs.clone(),
                kwargs_expr.clone(),
            ),
            NodeJob::Invoke { function, args } => self.invoke_function(function, args.clone()),
            NodeJob::Field { receiver, field } => {
                let receiver = self.force_selector_receiver(receiver.clone())?;
                self.select_field(&receiver, field)
            }
            NodeJob::Fail { message } => Err(self.user_fail(message.clone())?),
            #[cfg(test)]
            NodeJob::Sleep { millis, value } => {
                thread::sleep(Duration::from_millis(*millis));
                Ok(value.clone())
            }
            #[cfg(test)]
            NodeJob::Poison(error) => Err(error.clone()),
        }
    }

    pub(crate) fn inspect_scheduled_value(
        &self,
        value: Value,
    ) -> Result<InspectedValue, PoisonError> {
        self.inspect_scheduled_value_inner(value, &mut Vec::new())
    }

    fn inspect_scheduled_value_inner(
        &self,
        value: Value,
        visiting: &mut Vec<NodeId>,
    ) -> Result<InspectedValue, PoisonError> {
        match value {
            Value::Pending(node_id) => match self.scheduler_node_state(node_id)? {
                SchedulerNodeState::Pending => {
                    let mut pending = BTreeSet::new();
                    pending.insert(node_id);
                    Ok(InspectedValue {
                        value: Value::Pending(node_id),
                        pending,
                        errors: Vec::new(),
                    })
                }
                SchedulerNodeState::Resolved(value) => {
                    if visiting.contains(&node_id) {
                        return Err(self.scheduler_circular_dependency(visiting, node_id));
                    }
                    if visiting.len() + 1 > self.max_demand_depth {
                        return Err(self.scheduler_demand_depth_exceeded(visiting, node_id));
                    }
                    visiting.push(node_id);
                    let result = self.inspect_scheduled_value_inner(value, visiting);
                    let popped = visiting.pop();
                    debug_assert_eq!(popped, Some(node_id));
                    result
                }
                SchedulerNodeState::Poisoned(error) => Ok(InspectedValue {
                    value: Value::Pending(node_id),
                    pending: BTreeSet::new(),
                    errors: vec![error],
                }),
            },
            Value::Discovered(discovered) => {
                let inspected = self.inspect_scheduled_value_inner(*discovered.value, visiting)?;
                Ok(InspectedValue {
                    value: self.attach_obligations(
                        inspected.value,
                        discovered.obligations.iter().cloned(),
                    ),
                    pending: inspected.pending,
                    errors: inspected.errors,
                })
            }
            Value::Tuple(elems) => {
                let (elems, pending, errors) =
                    self.inspect_scheduled_list(elems.iter().cloned(), visiting)?;
                Ok(InspectedValue {
                    value: Value::Tuple(Arc::from(elems)),
                    pending,
                    errors,
                })
            }
            Value::Array(items) => {
                let (items, pending, errors) =
                    self.inspect_scheduled_list(items.iter().cloned(), visiting)?;
                Ok(InspectedValue {
                    value: Value::Array(Arc::from(items)),
                    pending,
                    errors,
                })
            }
            Value::Map(map) => {
                let mut entries = Vec::with_capacity(map.entries.len());
                let mut pending = BTreeSet::new();
                let mut errors = Vec::new();
                for (key, value) in map.entries.iter() {
                    let key = self.inspect_scheduled_value_inner(key.clone(), visiting)?;
                    let value = self.inspect_scheduled_value_inner(value.clone(), visiting)?;
                    pending.extend(key.pending);
                    pending.extend(value.pending);
                    errors.extend(key.errors);
                    errors.extend(value.errors);
                    entries.push((key.value, value.value));
                }
                Ok(InspectedValue {
                    value: Value::Map(MapValue {
                        entries: Arc::from(entries),
                    }),
                    pending,
                    errors,
                })
            }
            Value::ExecPath(path) => match path {
                ExecPathValue::InTree { tree, suffix } => {
                    let tree = self.inspect_scheduled_tree(tree, visiting)?;
                    Ok(InspectedValue {
                        value: Value::ExecPath(ExecPathValue::InTree {
                            tree: match tree.value {
                                Value::Tree(tree) => tree,
                                other => {
                                    return Err(self.internal_error(&format!(
                                        "expected Tree while inspecting ExecPath, got {}",
                                        other.type_name()
                                    )));
                                }
                            },
                            suffix,
                        }),
                        pending: tree.pending,
                        errors: tree.errors,
                    })
                }
                ExecPathValue::InBlob { blob, leaf } => Ok(InspectedValue::resolved(
                    Value::ExecPath(ExecPathValue::InBlob { blob, leaf }),
                )),
            },
            Value::Tree(tree) => self.inspect_scheduled_tree(tree, visiting),
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                let mut pending = BTreeSet::new();
                let mut errors = Vec::new();
                for (name, value) in struct_value.fields.iter() {
                    let value = self.inspect_scheduled_value_inner(value.clone(), visiting)?;
                    pending.extend(value.pending);
                    errors.extend(value.errors);
                    fields.push((name.clone(), value.value));
                }
                Ok(InspectedValue {
                    value: Value::Struct(StructValue {
                        type_name: struct_value.type_name,
                        fields: Arc::from(fields),
                    }),
                    pending,
                    errors,
                })
            }
            Value::Function(function) => {
                let mut captures = Vec::with_capacity(function.captures.len());
                let mut pending = BTreeSet::new();
                let mut errors = Vec::new();
                for (name, value) in function.captures.iter() {
                    let value = self.inspect_scheduled_value_inner(value.clone(), visiting)?;
                    pending.extend(value.pending);
                    errors.extend(value.errors);
                    captures.push((name.clone(), value.value));
                }
                Ok(InspectedValue {
                    value: Value::Function(FunctionValue {
                        def: function.def,
                        code_identity: function.code_identity,
                        captures: Arc::from(captures),
                    }),
                    pending,
                    errors,
                })
            }
            Value::Partial(partial) => {
                let function =
                    self.inspect_scheduled_value_inner(partial.function.clone(), visiting)?;
                let (args, mut pending, mut errors) =
                    self.inspect_scheduled_list(partial.args.iter().cloned(), visiting)?;
                pending.extend(function.pending);
                errors.extend(function.errors);
                let mut kwargs = Vec::with_capacity(partial.kwargs.len());
                for (name, value) in partial.kwargs.iter() {
                    let value = self.inspect_scheduled_value_inner(value.clone(), visiting)?;
                    pending.extend(value.pending);
                    errors.extend(value.errors);
                    kwargs.push((name.clone(), value.value));
                }
                let kwargs_expr = match &partial.kwargs_expr {
                    Some(kwargs_expr) => {
                        let value =
                            self.inspect_scheduled_value_inner((**kwargs_expr).clone(), visiting)?;
                        pending.extend(value.pending);
                        errors.extend(value.errors);
                        Some(Box::new(value.value))
                    }
                    None => None,
                };
                Ok(InspectedValue {
                    value: Value::Partial(Arc::new(PartialValue {
                        function: function.value,
                        args: Arc::from(args),
                        kwargs: Arc::from(kwargs),
                        kwargs_expr,
                    })),
                    pending,
                    errors,
                })
            }
            Value::Builtin(builtin) => {
                let (builtin, pending, errors) =
                    self.inspect_scheduled_builtin(builtin, visiting)?;
                Ok(InspectedValue {
                    value: Value::Builtin(builtin),
                    pending,
                    errors,
                })
            }
            other => Ok(InspectedValue::resolved(other)),
        }
    }

    fn inspect_scheduled_list(
        &self,
        values: impl IntoIterator<Item = Value>,
        visiting: &mut Vec<NodeId>,
    ) -> Result<(Vec<Value>, BTreeSet<NodeId>, Vec<PoisonError>), PoisonError> {
        let mut out = Vec::new();
        let mut pending = BTreeSet::new();
        let mut errors = Vec::new();
        for value in values {
            let inspected = self.inspect_scheduled_value_inner(value, visiting)?;
            out.push(inspected.value);
            pending.extend(inspected.pending);
            errors.extend(inspected.errors);
        }
        Ok((out, pending, errors))
    }

    fn inspect_scheduled_tree(
        &self,
        tree: TreeValue,
        visiting: &mut Vec<NodeId>,
    ) -> Result<InspectedValue, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => {
                let mut out = Vec::with_capacity(entries.len());
                let mut pending = BTreeSet::new();
                let mut errors = Vec::new();
                for (name, value) in entries.iter() {
                    let value = self.inspect_scheduled_value_inner(value.clone(), visiting)?;
                    pending.extend(value.pending);
                    errors.extend(value.errors);
                    out.push((name.clone(), value.value));
                }
                Ok(InspectedValue {
                    value: Value::Tree(TreeValue::Inline(Arc::from(out))),
                    pending,
                    errors,
                })
            }
            TreeValue::FsDir(path) => Ok(InspectedValue::resolved(Value::Tree(TreeValue::FsDir(
                path,
            )))),
            TreeValue::Store(root) => Ok(InspectedValue::resolved(Value::Tree(TreeValue::Store(
                root,
            )))),
        }
    }

    fn inspect_scheduled_builtin(
        &self,
        builtin: BuiltinValue,
        visiting: &mut Vec<NodeId>,
    ) -> Result<(BuiltinValue, BTreeSet<NodeId>, Vec<PoisonError>), PoisonError> {
        let inspect_receiver =
            |receiver: Box<Value>,
             visiting: &mut Vec<NodeId>|
             -> Result<(Value, BTreeSet<NodeId>, Vec<PoisonError>), PoisonError> {
                let inspected = self.inspect_scheduled_value_inner(*receiver, visiting)?;
                Ok((inspected.value, inspected.pending, inspected.errors))
            };

        match builtin {
            BuiltinValue::Namespace(namespace) => Ok((
                BuiltinValue::Namespace(namespace),
                BTreeSet::new(),
                Vec::new(),
            )),
            BuiltinValue::Function(function) => Ok((
                BuiltinValue::Function(function),
                BTreeSet::new(),
                Vec::new(),
            )),
            BuiltinValue::Method(method) => match method {
                BuiltinMethod::BlobFromString => Ok((
                    BuiltinValue::Method(BuiltinMethod::BlobFromString),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::BytesFromString => Ok((
                    BuiltinValue::Method(BuiltinMethod::BytesFromString),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::TreeOf => Ok((
                    BuiltinValue::Method(BuiltinMethod::TreeOf),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::TreeEmpty => Ok((
                    BuiltinValue::Method(BuiltinMethod::TreeEmpty),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::TreeFromPath => Ok((
                    BuiltinValue::Method(BuiltinMethod::TreeFromPath),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::ArrayMap { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayMap {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::ArrayToMap { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayToMap {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::ArrayFlatten { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayFlatten {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::ArrayMerge { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayMerge {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::MapMapValues { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::MapMapValues {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobToString { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobToString {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobExecutable { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobExecutable {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobUtf8 { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobUtf8 {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobUtf8Lossy { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BytesUtf8 { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BytesUtf8 {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BytesUtf8Lossy { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BytesBlob { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BytesBlob {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::StreamCollect { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StreamCollect {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::TreeFile { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::TreeFile {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::TreeMerge { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::TreeMerge {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::TreeSubtree { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::TreeSubtree {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::OptionUnwrapOr { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::FallibleCall { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::FallibleCall {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostRead { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostRead {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostHash { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostHash {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostWhich { receiver } => {
                    let (value, pending, errors) = inspect_receiver(receiver, visiting)?;
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostWhich {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
            },
        }
    }

    fn materialize_scheduled_value_deep(
        &self,
        value: Value,
        context: &str,
    ) -> Result<Value, PoisonError> {
        let inspected = self.inspect_scheduled_value(value)?;
        if !inspected.errors.is_empty() {
            return Err(self.scheduler_aggregate_poison_errors(context, &[], inspected.errors));
        }
        if !inspected.pending.is_empty() {
            return Err(self.internal_error(&format!(
                "{context} still had unresolved pending values: {}",
                inspected
                    .pending
                    .iter()
                    .map(|node_id| node_id.0.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(inspected.value)
    }

    fn materialize_scheduled_many_deep(
        &self,
        values: Vec<Value>,
        context: &str,
    ) -> Result<Vec<Value>, PoisonError> {
        let mut materialized = Vec::with_capacity(values.len());
        let mut pending = BTreeSet::new();
        let mut errors = Vec::new();
        for value in values {
            let inspected = self.inspect_scheduled_value(value)?;
            materialized.push(inspected.value);
            pending.extend(inspected.pending);
            errors.extend(inspected.errors);
        }
        if !errors.is_empty() {
            return Err(self.scheduler_aggregate_poison_errors(context, &[], errors));
        }
        if !pending.is_empty() {
            return Err(self.internal_error(&format!(
                "{context} still had unresolved pending values: {}",
                pending
                    .iter()
                    .map(|node_id| node_id.0.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(materialized)
    }

    fn materialize_scheduled_value_shallow(
        &self,
        value: Value,
        context: &str,
    ) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => match self.scheduler_node_state(node_id)? {
                SchedulerNodeState::Pending => Err(self.internal_error(&format!(
                    "{context} still had unresolved pending node {}",
                    node_id.0
                ))),
                SchedulerNodeState::Resolved(value) => {
                    self.materialize_scheduled_value_shallow(value, context)
                }
                SchedulerNodeState::Poisoned(error) => Err(error),
            },
            Value::Discovered(discovered) => Ok(self.attach_obligations(
                self.materialize_scheduled_value_shallow(*discovered.value, context)?,
                discovered.obligations.iter().cloned(),
            )),
            other => Ok(other),
        }
    }

    fn materialize_scheduled_template_parts(
        &self,
        parts: &[TemplatePartValue],
    ) -> Result<Vec<TemplatePartValue>, PoisonError> {
        let values = parts
            .iter()
            .filter_map(|part| match part {
                TemplatePartValue::Text(_) => None,
                TemplatePartValue::Expr(value) => Some(value.clone()),
            })
            .collect::<Vec<_>>();
        let forced = self.materialize_scheduled_many_deep(values, "forcing dependencies")?;
        let mut forced_iter = forced.into_iter();
        Ok(parts
            .iter()
            .map(|part| match part {
                TemplatePartValue::Text(text) => TemplatePartValue::Text(text.clone()),
                TemplatePartValue::Expr(_) => TemplatePartValue::Expr(
                    forced_iter.next().expect("materialized value for template"),
                ),
            })
            .collect())
    }

    fn execute_node_scheduled(&self, node: &Arc<Node>) -> Result<Value, PoisonError> {
        match &node.job {
            NodeJob::Binary { op, left, right } => {
                let mut values = self.materialize_scheduled_many_deep(
                    vec![left.clone(), right.clone()],
                    "forcing dependencies",
                )?;
                let right = values.pop().expect("pair has two values");
                let left = values.pop().expect("pair has two values");
                self.compute_binary(*op, &left, &right)
            }
            NodeJob::Unary { op, operand } => {
                let operand =
                    self.materialize_scheduled_value_deep(operand.clone(), "forcing dependency")?;
                self.compute_unary(*op, &operand)
            }
            NodeJob::Template { kind, parts } => {
                let values = self.materialize_scheduled_template_parts(parts)?;
                match kind {
                    TemplateKind::String => self.interpolate_string_parts(&values),
                    TemplateKind::Path => self.interpolate_path_parts(&values),
                }
            }
            NodeJob::If {
                cond,
                then_branch,
                else_branch,
                bindings,
                scope_identity,
            } => {
                let cond =
                    self.materialize_scheduled_value_deep(cond.clone(), "forcing dependency")?;
                match cond {
                    Value::Bool(true) => self
                        .eval_block(then_branch, bindings.as_ref(), scope_identity.as_ref())
                        .into_value(),
                    Value::Bool(false) => self
                        .eval_block(else_branch, bindings.as_ref(), scope_identity.as_ref())
                        .into_value(),
                    other => Err(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            NodeJob::Fallible { value } => {
                match self
                    .materialize_scheduled_value_shallow(value.clone(), "forcing fallible operand")
                {
                    Ok(value) => Ok(self.build_fallible_value(value)),
                    Err(_) => Ok(option_none()),
                }
            }
            NodeJob::Demand { demands, result } => {
                if self.discovery_recording_active() {
                    for value in demands.iter() {
                        let _ = self.force_value(value.clone())?;
                    }
                } else {
                    let _ = self.force_many(demands.iter().cloned().collect())?;
                }
                result.clone()
            }
            NodeJob::Call {
                function,
                args,
                kwargs,
                kwargs_expr,
            } => self.execute_call_scheduled(
                function.clone(),
                args.clone(),
                kwargs.clone(),
                kwargs_expr.clone(),
            ),
            NodeJob::Invoke { function, args } => self.invoke_function(function, args.clone()),
            NodeJob::Field { receiver, field } => {
                let receiver = self.materialize_scheduled_value_shallow(
                    receiver.clone(),
                    "forcing selector receiver",
                )?;
                self.select_field(&receiver, field)
            }
            NodeJob::Fail { message } => {
                let message =
                    self.materialize_scheduled_value_deep(message.clone(), "forcing dependency")?;
                Err(PoisonError::new(
                    PoisonKind::UserFail,
                    message.to_string(),
                    self.current_demand_chain(None),
                ))
            }
            #[cfg(test)]
            NodeJob::Sleep { millis, value } => {
                thread::sleep(Duration::from_millis(*millis));
                Ok(value.clone())
            }
            #[cfg(test)]
            NodeJob::Poison(error) => Err(error.clone()),
        }
    }

    fn execute_call_scheduled(
        &self,
        function: Value,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        let (function, function_obligations) = self.split_obligations(
            self.materialize_scheduled_value_shallow(function, "forcing callable surface")?,
        );
        match function {
            Value::Function(function) => self.finish_execute_call_scheduled(
                function,
                args,
                kwargs,
                kwargs_expr,
                function_obligations,
            ),
            Value::Partial(partial) => {
                let mut merged_args = partial.args.iter().cloned().collect::<Vec<_>>();
                merged_args.extend(args.iter().cloned());
                let mut merged_kwargs = partial.kwargs.iter().cloned().collect::<Vec<_>>();
                merged_kwargs.extend(kwargs.iter().cloned());
                let merged_kwargs_expr = match (partial.kwargs_expr.as_ref(), kwargs_expr) {
                    (Some(left), Some(right)) => {
                        Some(self.merge_keyword_records_scheduled((**left).clone(), right)?)
                    }
                    (Some(value), None) => Some((**value).clone()),
                    (None, value) => value,
                };
                let result = self.execute_call_scheduled(
                    partial.function.clone(),
                    Arc::from(merged_args),
                    Arc::from(merged_kwargs),
                    merged_kwargs_expr,
                )?;
                Ok(self.attach_obligations(result, function_obligations))
            }
            Value::Builtin(builtin) => {
                let result =
                    self.invoke_builtin_scheduled(&builtin, &args, &kwargs, kwargs_expr)?;
                Ok(self.attach_obligations(result, function_obligations))
            }
            other => Err(self.type_error(&format!("cannot call {}", other.type_name()))),
        }
    }

    fn finish_execute_call_scheduled(
        &self,
        function: FunctionValue,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
        kwargs_expr: Option<Value>,
        function_obligations: Vec<HostObservation>,
    ) -> Result<Value, PoisonError> {
        if !kwargs.is_empty() || kwargs_expr.is_some() {
            return Err(self.type_error("keyword arguments are only supported for builtins"));
        }

        if function.key().is_some() && args.iter().all(|value| value.key().is_some()) {
            let node_id = self.memoized_invoke_node(function, args.iter().cloned().collect());
            return Ok(self.attach_obligations(Value::Pending(node_id), function_obligations));
        }

        let result = self.invoke_function(&function, args)?;
        Ok(self.attach_obligations(result, function_obligations))
    }

    // r[impl eval.enter-functions]
    fn execute_call(
        &self,
        function: Value,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        let (function, function_obligations) = self.split_obligations(self.force_value(function)?);
        match function {
            Value::Function(function) => {
                self.finish_execute_call(function, args, kwargs, kwargs_expr, function_obligations)
            }
            Value::Partial(partial) => {
                let mut merged_args = partial.args.iter().cloned().collect::<Vec<_>>();
                merged_args.extend(args.iter().cloned());
                let mut merged_kwargs = partial.kwargs.iter().cloned().collect::<Vec<_>>();
                merged_kwargs.extend(kwargs.iter().cloned());
                let merged_kwargs_expr = match (partial.kwargs_expr.as_ref(), kwargs_expr) {
                    (Some(left), Some(right)) => {
                        Some(self.merge_keyword_records((**left).clone(), right)?)
                    }
                    (Some(value), None) => Some((**value).clone()),
                    (None, value) => value,
                };
                let result = self.execute_call(
                    partial.function.clone(),
                    Arc::from(merged_args),
                    Arc::from(merged_kwargs),
                    merged_kwargs_expr,
                )?;
                Ok(self.attach_obligations(result, function_obligations))
            }
            Value::Builtin(builtin) => {
                let result = self.invoke_builtin(&builtin, &args, &kwargs, kwargs_expr)?;
                Ok(self.attach_obligations(result, function_obligations))
            }
            other => Err(self.type_error(&format!("cannot call {}", other.type_name()))),
        }
    }

    fn finish_execute_call(
        &self,
        function: FunctionValue,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
        kwargs_expr: Option<Value>,
        function_obligations: Vec<HostObservation>,
    ) -> Result<Value, PoisonError> {
        if !kwargs.is_empty() || kwargs_expr.is_some() {
            return Err(self.type_error("keyword arguments are only supported for builtins"));
        }

        if function.key().is_some() && args.iter().all(|value| value.key().is_some()) {
            let result = self.force_pending_shallow(
                self.memoized_invoke_node(function, args.iter().cloned().collect()),
            )?;
            return Ok(self.attach_obligations(result, function_obligations));
        }

        let result = self.invoke_function(&function, args)?;
        Ok(self.attach_obligations(result, function_obligations))
    }

    // r[impl eval.enter-functions]
    fn invoke_function(
        &self,
        function: &FunctionValue,
        args: Arc<[Value]>,
    ) -> Result<Value, PoisonError> {
        let mut bindings = HashMap::new();

        for (name, value) in function.captures.iter() {
            bindings.insert(name.to_string(), value.clone());
        }

        for (param, arg) in function.def.params.iter().zip(args.iter()) {
            bindings.insert(param.name.clone(), arg.clone());
        }

        for (name, overloads) in &self.functions {
            if !bindings.contains_key(name) {
                bindings.insert(
                    name.clone(),
                    Value::Function(self.global_function_value(overloads[0].clone())),
                );
            }
        }

        self.eval_block(
            &function.def.body,
            &bindings,
            function.code_identity.as_ref(),
        )
        .into_value()
    }

    fn resolve_module_function(&self, name: &str) -> Result<FunctionValue, PoisonError> {
        let mut bindings = self.eval_module_scope()?;
        match bindings.remove(name) {
            Some(Value::Function(function)) => Ok(function),
            Some(other) => Err(self.type_error(&format!(
                "top-level name '{name}' is {}, not a function",
                other.type_name()
            ))),
            None => Err(self.unresolved_function(name)),
        }
    }

    fn eval_module_scope(&self) -> Result<HashMap<String, Value>, PoisonError> {
        let mut bindings = HashMap::new();
        let mut demanded_values = Vec::new();

        for (name, overloads) in &self.functions {
            bindings.insert(
                name.clone(),
                Value::Function(self.global_function_value(overloads[0].clone())),
            );
        }

        for stmt in &self.module.top_level.stmts {
            match stmt {
                instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                    let value = self.eval_expr(value, &bindings, "<module>").into_value()?;
                    bindings.insert(name.clone(), value);
                }
                instr::Stmt::LetPattern { pattern, value } => {
                    let value = self.eval_expr(value, &bindings, "<module>").into_value()?;
                    self.bind_pattern_into(pattern, value, &mut bindings)?;
                }
                instr::Stmt::Return(_) => {
                    return Err(self.internal_error("return is not allowed at file scope"));
                }
                instr::Stmt::FnDecl(def) => {
                    bindings.insert(
                        def.name.clone(),
                        Value::Function(self.local_function_value(
                            def.clone(),
                            capture_referenced_bindings(&bindings, &def.params, &def.body),
                        )),
                    );
                }
                instr::Stmt::StructDecl(decl) => {
                    bindings.insert(decl.name.clone(), self.struct_constructor_value(decl));
                }
                instr::Stmt::Expr(expr) => {
                    let value = self.eval_expr(expr, &bindings, "<module>").into_value()?;
                    demanded_values.push(value);
                }
            }
        }

        self.resolve_demanded_values(mem::take(&mut demanded_values))?;
        Ok(bindings)
    }

    fn eval_block(
        &self,
        block: &instr::Block,
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        let mut local_bindings = bindings.clone();
        let mut demanded_values = Vec::new();

        for (index, stmt) in block.stmts.iter().enumerate() {
            let is_last = index + 1 == block.stmts.len();
            match stmt {
                instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                    match self.eval_expr(value, &local_bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            local_bindings.insert(name.clone(), value);
                        }
                        other => {
                            return self.finish_demanded_result(other, &mut demanded_values);
                        }
                    }
                }
                instr::Stmt::LetPattern { pattern, value } => {
                    match self.eval_expr(value, &local_bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            if let Err(error) =
                                self.bind_pattern_into(pattern, value, &mut local_bindings)
                            {
                                return self.finish_demanded_result(
                                    BuildResult::Poisoned(error),
                                    &mut demanded_values,
                                );
                            }
                        }
                        other => {
                            return self.finish_demanded_result(other, &mut demanded_values);
                        }
                    }
                }
                instr::Stmt::Return(expr) => {
                    match self.eval_expr(expr, &local_bindings, current_code_identity) {
                        BuildResult::Value(value) | BuildResult::Returned(value) => {
                            return self.finish_demanded_result(
                                BuildResult::Returned(value),
                                &mut demanded_values,
                            );
                        }
                        BuildResult::Poisoned(error) => {
                            return self.finish_demanded_result(
                                BuildResult::Poisoned(error),
                                &mut demanded_values,
                            );
                        }
                    }
                }
                instr::Stmt::FnDecl(def) => {
                    local_bindings.insert(
                        def.name.clone(),
                        Value::Function(self.local_function_value(
                            def.clone(),
                            capture_referenced_bindings(&local_bindings, &def.params, &def.body),
                        )),
                    );
                }
                instr::Stmt::StructDecl(decl) => {
                    local_bindings.insert(decl.name.clone(), self.struct_constructor_value(decl));
                }
                instr::Stmt::Expr(expr) => {
                    match self.eval_expr(expr, &local_bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            if is_last {
                                return self.finish_demanded_result(
                                    BuildResult::Value(value),
                                    &mut demanded_values,
                                );
                            }
                            if self.discovery_recording_active() {
                                if let Err(error) = self.force_value(value) {
                                    return self.finish_demanded_result(
                                        BuildResult::Poisoned(error),
                                        &mut demanded_values,
                                    );
                                }
                            } else {
                                demanded_values.push(value);
                            }
                        }
                        other => {
                            return self.finish_demanded_result(other, &mut demanded_values);
                        }
                    }
                }
            }
        }

        self.finish_demanded_result(BuildResult::Value(Value::Unit), &mut demanded_values)
    }

    fn finish_demanded_result(
        &self,
        result: BuildResult,
        demanded_values: &mut Vec<Value>,
    ) -> BuildResult {
        let demanded_values = mem::take(demanded_values);
        match result {
            BuildResult::Value(value) => {
                BuildResult::Value(self.wrap_demanded_value(demanded_values, Ok(value)))
            }
            BuildResult::Returned(value) => {
                BuildResult::Returned(self.wrap_demanded_value(demanded_values, Ok(value)))
            }
            BuildResult::Poisoned(error) => {
                if let Err(demand_error) = self.resolve_demanded_values(demanded_values) {
                    return BuildResult::Poisoned(demand_error);
                }
                BuildResult::Poisoned(error)
            }
        }
    }

    fn wrap_demanded_value(
        &self,
        demanded_values: Vec<Value>,
        result: Result<Value, PoisonError>,
    ) -> Value {
        if demanded_values.is_empty() {
            return result.expect("demand-free result must be a value");
        }

        self.pending_value(
            NodeJob::Demand {
                demands: Arc::from(demanded_values),
                result,
            },
            "demand(..)".into(),
            None,
        )
    }

    fn resolve_demanded_values(&self, demanded_values: Vec<Value>) -> Result<(), PoisonError> {
        if demanded_values.is_empty() {
            return Ok(());
        }

        let _ = self.resolve_boundary_value(Value::Array(Arc::from(demanded_values)))?;
        Ok(())
    }

    fn build_fallible_value(&self, value: Value) -> Value {
        let (value, obligations) = self.split_obligations(value);
        match value {
            Value::Function(_) | Value::Builtin(_) | Value::Partial(_) => {
                let value = self.attach_obligations(value, obligations);
                self.pending_value(
                    NodeJob::Fallible {
                        value: self.build_call(value, vec![], None),
                    },
                    "fallible(..)".into(),
                    None,
                )
            }
            Value::Pending(_) => self.pending_value(
                NodeJob::Fallible {
                    value: self.attach_obligations(value, obligations),
                },
                "fallible(..)".into(),
                None,
            ),
            other => option_some(self.attach_obligations(other, obligations)),
        }
    }

    // r[impl eval.forcing]
    fn eval_expr(
        &self,
        expr: &instr::Expr,
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        match expr {
            instr::Expr::LitInt(n) => BuildResult::Value(Value::U64(*n)),
            instr::Expr::LitFloat(n) => BuildResult::Value(Value::F64(*n)),
            instr::Expr::LitBool(b) => BuildResult::Value(Value::Bool(*b)),
            instr::Expr::LitString(s) => BuildResult::Value(Value::String(Arc::from(s.as_str()))),
            instr::Expr::StringTemplate { parts, provenance } => self.eval_template_expr(
                TemplateKind::String,
                parts,
                provenance.as_ref(),
                bindings,
                current_code_identity,
            ),
            instr::Expr::PathTemplate { parts, provenance } => self.eval_template_expr(
                TemplateKind::Path,
                parts,
                provenance.as_ref(),
                bindings,
                current_code_identity,
            ),
            instr::Expr::Unit => BuildResult::Value(Value::Unit),
            instr::Expr::Var(name) => {
                if let Some(value) = bindings.get(name) {
                    BuildResult::Value(value.clone())
                } else if name == "None" {
                    BuildResult::Value(option_none())
                } else if let Some(overloads) = self.functions.get(name) {
                    BuildResult::Value(Value::Function(
                        self.global_function_value(overloads[0].clone()),
                    ))
                } else if let Some(builtin) = builtin_namespace(name) {
                    BuildResult::Value(Value::Builtin(builtin))
                } else {
                    BuildResult::Poisoned(self.unresolved_name(name))
                }
            }
            instr::Expr::Binary {
                op,
                left,
                right,
                provenance,
            } => {
                let left = match self.eval_expr(left, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                let right = match self.eval_expr(right, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                if left.key().is_some() && right.key().is_some() {
                    BuildResult::from_result(self.compute_binary(*op, &left, &right))
                } else {
                    BuildResult::Value(self.pending_value(
                        NodeJob::Binary {
                            op: *op,
                            left,
                            right,
                        },
                        format!("binary({op:?})"),
                        provenance.clone(),
                    ))
                }
            }
            instr::Expr::Unary {
                op,
                operand,
                provenance,
            } => {
                let operand = match self.eval_expr(operand, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                if operand.key().is_some() {
                    BuildResult::from_result(self.compute_unary(*op, &operand))
                } else {
                    BuildResult::Value(self.pending_value(
                        NodeJob::Unary { op: *op, operand },
                        format!("unary({op:?})"),
                        provenance.clone(),
                    ))
                }
            }
            instr::Expr::Block(block) => self.eval_block(block, bindings, current_code_identity),
            // r[impl eval.control-demand]
            instr::Expr::If {
                cond,
                then_branch,
                else_branch,
                provenance,
            } => {
                let cond = match self.eval_expr(cond, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                match cond {
                    Value::Bool(true) => {
                        self.eval_block(then_branch, bindings, current_code_identity)
                    }
                    Value::Bool(false) => {
                        self.eval_block(else_branch, bindings, current_code_identity)
                    }
                    Value::Pending(_) => BuildResult::Value(self.pending_value(
                        NodeJob::If {
                            cond,
                            then_branch: then_branch.clone(),
                            else_branch: else_branch.clone(),
                            bindings: Arc::new(bindings.clone()),
                            scope_identity: Arc::from(current_code_identity),
                        },
                        "if(..)".into(),
                        provenance.clone(),
                    )),
                    other => BuildResult::Poisoned(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            // r[impl fn.closure]
            // r[impl fn.identity.closure]
            instr::Expr::Closure { closure_id, def } => {
                BuildResult::Value(Value::Function(self.closure_function_value(
                    def.clone(),
                    Arc::from(format!("{current_code_identity}::closure#{closure_id}")),
                    capture_referenced_bindings(bindings, &def.params, &def.body),
                )))
            }
            instr::Expr::Call {
                func,
                args,
                kwargs,
                kwargs_expr,
                provenance,
            } => {
                let function = match self.eval_expr(func, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    match self.eval_expr(arg, bindings, current_code_identity) {
                        BuildResult::Value(value) => arg_values.push(value),
                        other => return other,
                    }
                }
                let mut kwarg_values = Vec::with_capacity(kwargs.len());
                for (name, arg) in kwargs {
                    match self.eval_expr(arg, bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            kwarg_values.push((Arc::from(name.as_str()), value));
                        }
                        other => return other,
                    }
                }
                let kwargs_expr_value = match kwargs_expr {
                    Some(expr) => match self.eval_expr(expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => Some(value),
                        other => return other,
                    },
                    None => None,
                };
                BuildResult::Value(self.build_call_with_kwargs(
                    function,
                    arg_values,
                    kwarg_values,
                    kwargs_expr_value,
                    provenance.clone(),
                ))
            }
            instr::Expr::PartialCall {
                func,
                args,
                kwargs,
                kwargs_expr,
                provenance: _,
            } => {
                let function = match self.eval_expr(func, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    match self.eval_expr(arg, bindings, current_code_identity) {
                        BuildResult::Value(value) => arg_values.push(value),
                        other => return other,
                    }
                }
                let mut kwarg_values = Vec::with_capacity(kwargs.len());
                for (name, arg) in kwargs {
                    match self.eval_expr(arg, bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            kwarg_values.push((Arc::from(name.as_str()), value));
                        }
                        other => return other,
                    }
                }
                let kwargs_expr_value = match kwargs_expr {
                    Some(expr) => match self.eval_expr(expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => Some(value),
                        other => return other,
                    },
                    None => None,
                };
                BuildResult::Value(Value::Partial(Arc::new(PartialValue {
                    function,
                    args: Arc::from(arg_values),
                    kwargs: Arc::from(kwarg_values),
                    kwargs_expr: kwargs_expr_value.map(Box::new),
                })))
            }
            instr::Expr::StructLit { name, fields } => {
                let mut values = Vec::with_capacity(fields.len());
                for (field_name, field_expr) in fields {
                    match self.eval_expr(field_expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            values.push((Arc::from(field_name.as_str()), value));
                        }
                        other => return other,
                    }
                }
                BuildResult::Value(Value::Struct(StructValue {
                    type_name: Arc::from(name.as_str()),
                    fields: Arc::from(values),
                }))
            }
            instr::Expr::Record(items) => {
                let mut entries = Vec::new();
                for item in items {
                    match item {
                        instr::RecordItem::Field { name, value } => {
                            let value = match self.eval_expr(value, bindings, current_code_identity)
                            {
                                BuildResult::Value(value) => value,
                                other => return other,
                            };
                            entries.push((Value::String(Arc::from(name.as_str())), value));
                        }
                        instr::RecordItem::Spread(spread_expr) => {
                            let spread_value = match self.eval_expr(
                                spread_expr,
                                bindings,
                                current_code_identity,
                            ) {
                                BuildResult::Value(value) => value,
                                other => return other,
                            };
                            let spread_value = match self.force_value_shallow(spread_value) {
                                Ok(value) => value,
                                Err(error) => return BuildResult::Poisoned(error),
                            };
                            match self.extend_record_with_spread(&mut entries, spread_value) {
                                Ok(()) => {}
                                Err(error) => return BuildResult::Poisoned(error),
                            }
                        }
                    }
                }
                BuildResult::Value(Value::Map(MapValue {
                    entries: Arc::from(entries),
                }))
            }
            instr::Expr::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        instr::ArrayItem::Expr(item_expr) => {
                            match self.eval_expr(item_expr, bindings, current_code_identity) {
                                BuildResult::Value(value) => values.push(value),
                                other => return other,
                            }
                        }
                        instr::ArrayItem::Spread(spread_expr) => {
                            let spread_value = match self.eval_expr(
                                spread_expr,
                                bindings,
                                current_code_identity,
                            ) {
                                BuildResult::Value(value) => value,
                                other => return other,
                            };
                            let spread_value = match self.force_value_shallow(spread_value) {
                                Ok(value) => value,
                                Err(error) => return BuildResult::Poisoned(error),
                            };
                            match self.extend_array_with_spread(&mut values, spread_value) {
                                Ok(()) => {}
                                Err(error) => return BuildResult::Poisoned(error),
                            }
                        }
                    }
                }
                BuildResult::Value(Value::Array(Arc::from(values)))
            }
            instr::Expr::Map(entries) => {
                let mut values = Vec::with_capacity(entries.len());
                for (key_expr, value_expr) in entries {
                    let key = match self.eval_expr(key_expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    let value = match self.eval_expr(value_expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    values.push((key, value));
                }
                BuildResult::Value(Value::Map(MapValue {
                    entries: Arc::from(values),
                }))
            }
            instr::Expr::Field {
                receiver,
                field,
                provenance,
            } => {
                let receiver = match self.eval_expr(receiver, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                if matches!(receiver, Value::Pending(_)) {
                    BuildResult::Value(self.pending_value(
                        NodeJob::Field {
                            receiver,
                            field: Arc::from(field.as_str()),
                        },
                        format!("field(.{field})"),
                        provenance.clone(),
                    ))
                } else {
                    BuildResult::from_result(self.select_field(&receiver, field))
                }
            }
            // r[impl error.fail]
            instr::Expr::Fail {
                message,
                provenance,
            } => {
                let message = match self.eval_expr(message, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                BuildResult::Value(self.pending_value(
                    NodeJob::Fail { message },
                    "fail(..)".into(),
                    provenance.clone(),
                ))
            }
            instr::Expr::Fallible(expr) => {
                match self.eval_expr(expr, bindings, current_code_identity) {
                    BuildResult::Value(value) => {
                        BuildResult::Value(self.build_fallible_value(value))
                    }
                    BuildResult::Poisoned(_) => BuildResult::Value(option_none()),
                    other => other,
                }
            }
            instr::Expr::Tuple(elems) => {
                let mut values = Vec::with_capacity(elems.len());
                for elem in elems {
                    match self.eval_expr(elem, bindings, current_code_identity) {
                        BuildResult::Value(value) => values.push(value),
                        other => return other,
                    }
                }
                BuildResult::Value(Value::Tuple(Arc::from(values)))
            }
        }
    }

    fn eval_template_expr(
        &self,
        kind: TemplateKind,
        parts: &[instr::TemplatePart],
        provenance: Option<&instr::SourceProvenance>,
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                instr::TemplatePart::Text(text) => {
                    values.push(TemplatePartValue::Text(Arc::from(text.as_str())));
                }
                instr::TemplatePart::Expr(expr) => {
                    let value = match self.eval_expr(expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    values.push(TemplatePartValue::Expr(value));
                }
            }
        }

        let needs_node = values.iter().any(|part| match part {
            TemplatePartValue::Text(_) => false,
            TemplatePartValue::Expr(value) => value.key().is_none(),
        });
        if needs_node {
            return BuildResult::Value(self.pending_value(
                NodeJob::Template {
                    kind,
                    parts: Arc::from(values),
                },
                format!("template({kind:?})"),
                provenance.cloned(),
            ));
        }

        let result = match kind {
            TemplateKind::String => self.interpolate_string_parts(&values),
            TemplateKind::Path => self.interpolate_path_parts(&values),
        };
        BuildResult::from_result(result)
    }

    fn force_template_parts(
        &self,
        parts: &[TemplatePartValue],
    ) -> Result<Vec<TemplatePartValue>, PoisonError> {
        let values = parts
            .iter()
            .filter_map(|part| match part {
                TemplatePartValue::Text(_) => None,
                TemplatePartValue::Expr(value) => Some(value.clone()),
            })
            .collect::<Vec<_>>();
        let forced = self.force_many(values)?;
        let mut forced_iter = forced.into_iter();
        Ok(parts
            .iter()
            .map(|part| match part {
                TemplatePartValue::Text(text) => TemplatePartValue::Text(text.clone()),
                TemplatePartValue::Expr(_) => {
                    TemplatePartValue::Expr(forced_iter.next().expect("forced value for template"))
                }
            })
            .collect())
    }

    // r[expr.literal.interpolation]
    fn interpolate_string_parts(&self, parts: &[TemplatePartValue]) -> Result<Value, PoisonError> {
        let mut value = String::new();
        let mut obligations = BTreeMap::new();
        for part in parts {
            match part {
                TemplatePartValue::Text(text) => value.push_str(text),
                TemplatePartValue::Expr(value_part) => {
                    let (value_part, value_obligations) = self.split_obligations_ref(value_part);
                    Self::add_obligations(&mut obligations, value_obligations.iter().cloned());
                    value.push_str(&self.stringify_interpolated_value(value_part)?);
                }
            }
        }
        Ok(self.attach_obligations(Value::String(Arc::from(value)), obligations.into_values()))
    }

    // r[exec.arg.interpolation]
    fn interpolate_path_parts(&self, parts: &[TemplatePartValue]) -> Result<Value, PoisonError> {
        let mut plain = String::new();
        let mut mounted_tree: Option<(TreeValue, String)> = None;
        let mut mounted_blob: Option<(BlobValue, String)> = None;
        let mut obligations = BTreeMap::new();

        for part in parts {
            match part {
                TemplatePartValue::Text(text) => plain.push_str(text),
                TemplatePartValue::Expr(value) => {
                    let (value, value_obligations) = self.split_obligations_ref(value);
                    Self::add_obligations(&mut obligations, value_obligations.iter().cloned());
                    match value {
                        Value::String(text) => plain.push_str(text),
                        Value::Path(path) => plain.push_str(path.as_str()),
                        Value::Tree(tree) => {
                            if mounted_tree.is_some() || mounted_blob.is_some() {
                                return Err(self.type_error(
                                "path literal cannot interpolate more than one mount-bearing value",
                            ));
                            }
                            mounted_tree = Some((tree.clone(), plain.clone()));
                            plain.clear();
                        }
                        Value::Blob(blob) => {
                            if mounted_tree.is_some() || mounted_blob.is_some() {
                                return Err(self.type_error(
                                "path literal cannot interpolate more than one mount-bearing value",
                            ));
                            }
                            mounted_blob = Some((blob.clone(), plain.clone()));
                            plain.clear();
                        }
                        Value::ExecPath(exec_path) => match exec_path {
                            ExecPathValue::InTree { tree, suffix } => {
                                if mounted_tree.is_some() || mounted_blob.is_some() {
                                    return Err(self.type_error(
                                    "path literal cannot interpolate more than one mount-bearing value",
                                ));
                                }
                                plain.push_str(suffix.as_str());
                                mounted_tree = Some((tree.clone(), plain.clone()));
                                plain.clear();
                            }
                            ExecPathValue::InBlob { blob, leaf } => {
                                if mounted_tree.is_some() || mounted_blob.is_some() {
                                    return Err(self.type_error(
                                    "path literal cannot interpolate more than one mount-bearing value",
                                ));
                                }
                                plain.push_str(leaf.as_str());
                                mounted_blob = Some((blob.clone(), plain.clone()));
                                plain.clear();
                            }
                        },
                        other => {
                            return Err(self.type_error(&format!(
                            "path interpolation requires String, Path, Tree, Blob, or ExecPath, got {}",
                            other.type_name()
                        )));
                        }
                    }
                }
            }
        }

        if let Some((tree, prefix)) = mounted_tree {
            let suffix = PathValue::new(format!("{prefix}{plain}").trim_start_matches('/'));
            return Ok(self.attach_obligations(
                Value::ExecPath(ExecPathValue::InTree { tree, suffix }),
                obligations.into_values(),
            ));
        }

        if let Some((blob, leaf)) = mounted_blob {
            let leaf = format!("{leaf}{plain}");
            let leaf = leaf.trim_start_matches('/').to_string();
            let normalized = PathValue::new(&leaf);
            if normalized.as_str().contains('/') {
                return Err(self
                    .type_error("path literal cannot append path segments to a blob-backed path"));
            }
            return Ok(self.attach_obligations(
                Value::ExecPath(ExecPathValue::InBlob {
                    blob,
                    leaf: normalized,
                }),
                obligations.into_values(),
            ));
        }

        Ok(self.attach_obligations(
            Value::Path(PathValue::new(plain)),
            obligations.into_values(),
        ))
    }

    fn stringify_interpolated_value(&self, value: &Value) -> Result<String, PoisonError> {
        match value {
            Value::String(text) => Ok(text.to_string()),
            Value::Path(path) => Ok(path.as_str().to_string()),
            Value::Bool(_) | Value::U64(_) | Value::I64(_) | Value::F64(_) | Value::Unit => {
                Ok(value.to_string())
            }
            other => Err(self.type_error(&format!(
                "cannot interpolate {} into String",
                other.type_name()
            ))),
        }
    }

    fn select_field(&self, receiver: &Value, field: &str) -> Result<Value, PoisonError> {
        let (receiver, obligations) = self.split_obligations_ref(receiver);
        let result = match receiver {
            Value::Struct(struct_value)
                if field == "unwrap_or"
                    && matches!(struct_value.type_name.as_ref(), "Some" | "None") =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::OptionUnwrapOr {
                        receiver: Box::new(receiver.clone()),
                    },
                )))
            }
            Value::Struct(struct_value) => struct_value
                .fields
                .iter()
                .find(|(name, _)| name.as_ref() == field)
                .map(|(_, value)| value.clone())
                .or_else(|| {
                    self.user_method_value(receiver, struct_value.type_name.as_ref(), field)
                })
                .ok_or_else(|| {
                    self.type_error(&format!(
                        "no field '{}' on struct {}",
                        field, struct_value.type_name
                    ))
                }),
            Value::Tuple(elems) => {
                if let Some(index) = field.strip_prefix('_')
                    && let Ok(index) = index.parse::<usize>()
                    && let Some(value) = elems.get(index)
                {
                    return Ok(value.clone());
                }
                Err(self.type_error(&format!("invalid tuple field '{}'", field)))
            }
            Value::Array(_) if field == "map" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayMap {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Array(_) if field == "to_map" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayToMap {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Array(_) if field == "flatten" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayFlatten {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Array(_) if field == "merge" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayMerge {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Map(map) => {
                if let Some((_, value)) = map.entries.iter().find(|(key, _)| match key {
                    Value::String(name) => name.as_ref() == field,
                    _ => false,
                }) {
                    return Ok(self.attach_obligations(value.clone(), obligations.iter().cloned()));
                }

                if field == "map_values" {
                    Ok(Value::Builtin(BuiltinValue::Method(
                        BuiltinMethod::MapMapValues {
                            receiver: Box::new(receiver.clone()),
                        },
                    )))
                } else {
                    Err(self.type_error(&format!("no field '{}' on Map", field)))
                }
            }
            Value::Blob(_) if field == "to_string" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobToString {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Blob(_) if field == "executable" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobExecutable {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Blob(_) if field == "utf8" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobUtf8 {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Blob(_) if field == "utf8_lossy" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobUtf8Lossy {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "collect" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StreamCollect {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "utf8" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BytesUtf8 {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "utf8_lossy" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BytesUtf8Lossy {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "blob" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BytesBlob {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Tree(_) if field == "file" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::TreeFile {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Tree(_) if field == "merge" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::TreeMerge {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Tree(_) if field == "subtree" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::TreeSubtree {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "read" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostRead {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "hash" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostHash {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "which" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostWhich {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Blob))
                if field == "from_string" =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::BlobFromString,
                )))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Bytes)) => Ok(Value::Builtin(
                BuiltinValue::Method(BuiltinMethod::BytesFromString),
            )),
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree)) if field == "of" => {
                Ok(Value::Builtin(BuiltinValue::Method(BuiltinMethod::TreeOf)))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree)) if field == "empty" => {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::TreeEmpty,
                )))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree))
                if field == "from_path" =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::TreeFromPath,
                )))
            }
            other => Err(self.type_error(&format!(
                "cannot access field '{}' on {}",
                field,
                other.type_name()
            ))),
        }?;
        Ok(self.attach_obligations(result, obligations.iter().cloned()))
    }

    fn compute_binary(&self, op: BinOp, left: &Value, right: &Value) -> Result<Value, PoisonError> {
        let (left, left_obligations) = self.split_obligations_ref(left);
        let (right, right_obligations) = self.split_obligations_ref(right);
        let result = match (op, left, right) {
            (BinOp::Add, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_add(*b))),
            (BinOp::Sub, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_sub(*b))),
            (BinOp::Mul, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_mul(*b))),
            (BinOp::Div, Value::U64(_), Value::U64(0)) => {
                Err(self.arithmetic_error("division by zero"))
            }
            (BinOp::Div, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a / b)),
            (BinOp::Mod, Value::U64(_), Value::U64(0)) => {
                Err(self.arithmetic_error("modulo by zero"))
            }
            (BinOp::Mod, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a % b)),

            (BinOp::Add, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_add(*b))),
            (BinOp::Sub, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_sub(*b))),
            (BinOp::Mul, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_mul(*b))),
            (BinOp::Div, Value::I64(_), Value::I64(0)) => {
                Err(self.arithmetic_error("division by zero"))
            }
            (BinOp::Div, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a / b)),

            (BinOp::Div, Value::Path(base), Value::String(child)) => {
                Ok(Value::Path(base.join(child.as_ref())))
            }
            (BinOp::Div, Value::Path(base), Value::Path(child)) => {
                Ok(Value::Path(base.join(child.as_str())))
            }

            (BinOp::Add, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a + b)),
            (BinOp::Sub, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a - b)),
            (BinOp::Mul, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a * b)),
            (BinOp::Div, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a / b)),

            (BinOp::Eq, a, b) => Ok(Value::Bool(a == b)),
            (BinOp::Ne, a, b) => Ok(Value::Bool(a != b)),

            (BinOp::Lt, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::Lt, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::Lt, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::And, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
            (BinOp::Or, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),

            (BinOp::Add, Value::String(a), Value::String(b)) => {
                let mut value = String::with_capacity(a.len() + b.len());
                value.push_str(a);
                value.push_str(b);
                Ok(Value::String(Arc::from(value)))
            }

            _ => Err(self.type_error(&format!(
                "cannot apply {:?} to {} and {}",
                op,
                left.type_name(),
                right.type_name()
            ))),
        }?;
        Ok(self.attach_obligations(
            result,
            left_obligations
                .iter()
                .chain(right_obligations.iter())
                .cloned(),
        ))
    }

    fn compute_unary(&self, op: UnaryOp, value: &Value) -> Result<Value, PoisonError> {
        let (value, obligations) = self.split_obligations_ref(value);
        let result = match (op, value) {
            (UnaryOp::Neg, Value::I64(n)) => Ok(Value::I64(-n)),
            (UnaryOp::Neg, Value::F64(n)) => Ok(Value::F64(-n)),
            (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            _ => Err(self.type_error(&format!("cannot apply {:?} to {}", op, value.type_name()))),
        }?;
        Ok(self.attach_obligations(result, obligations.iter().cloned()))
    }

    fn user_method_value(&self, receiver: &Value, type_name: &str, field: &str) -> Option<Value> {
        let qualified = format!("{type_name}.{field}");
        let overloads = self.functions.get(&qualified)?;
        let function = Value::Function(self.global_function_value(overloads[0].clone()));
        Some(Value::Partial(Arc::new(PartialValue {
            function,
            args: Arc::from([receiver.clone()]),
            kwargs: Arc::from([]),
            kwargs_expr: None,
        })))
    }

    fn invoke_builtin(
        &self,
        builtin: &BuiltinValue,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        match builtin {
            BuiltinValue::Namespace(namespace) => match namespace {
                BuiltinNamespace::Blob => {
                    self.require_no_kwargs("Blob", kwargs)?;
                    self.check_arity("Blob", args, 1)?;
                    let value = self.force_value(args[0].clone())?;
                    let text = self.stringify_scalar(&value)?;
                    Ok(Value::Blob(BlobValue::from_string(text)))
                }
                BuiltinNamespace::Bytes => {
                    self.require_no_kwargs("Bytes", kwargs)?;
                    self.check_arity("Bytes", args, 1)?;
                    let value = self.force_value(args[0].clone())?;
                    let text = self.stringify_scalar(&value)?;
                    Ok(Value::Bytes(Arc::from(text.as_bytes().to_vec())))
                }
                BuiltinNamespace::Tree => {
                    self.require_no_kwargs("Tree", kwargs)?;
                    self.check_arity("Tree", args, 1)?;
                    let (value, _) =
                        self.split_obligations(self.force_value_shallow(args[0].clone())?);
                    match value {
                        Value::Map(map) => self.tree_from_map(&map),
                        Value::Array(_) => self.array_merge(&value),
                        Value::Path(path) => Ok(Value::Tree(TreeValue::FsDir(path))),
                        Value::String(path) => {
                            Ok(Value::Tree(TreeValue::FsDir(PathValue::new(path.as_ref()))))
                        }
                        other => Err(self.type_error(&format!(
                            "Tree expects Map, Array, or Path/String, got {}",
                            other.type_name()
                        ))),
                    }
                }
            },
            BuiltinValue::Function(BuiltinFunction::Exec) => {
                self.exec_builtin(args, kwargs, kwargs_expr)
            }
            BuiltinValue::Function(BuiltinFunction::Discover) => {
                self.discover_builtin(args, kwargs, kwargs_expr)
            }
            BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                self.executor_class_builtin(args, kwargs, kwargs_expr)
            }
            BuiltinValue::Function(BuiltinFunction::StructConstructor { type_name, fields }) => {
                self.require_no_kwargs(type_name.as_ref(), kwargs)?;
                self.check_arity(type_name.as_ref(), args, 1)?;
                let (value, _) = self.split_obligations(self.force_value_shallow(args[0].clone())?);
                let Value::Map(map) = value else {
                    return Err(self.type_error(&format!(
                        "{} expects a record/map payload, got {}",
                        type_name,
                        value.type_name()
                    )));
                };
                let mut values = Vec::with_capacity(fields.len());
                for field in fields.iter() {
                    let field_value = map
                        .entries
                        .iter()
                        .find_map(|(key, value)| match key {
                            Value::String(name) if name.as_ref() == field.as_ref() => {
                                Some(value.clone())
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            self.type_error(&format!("{} requires field '{}'", type_name, field))
                        })?;
                    values.push((Arc::<str>::clone(field), field_value));
                }
                Ok(Value::Struct(StructValue {
                    type_name: Arc::clone(type_name),
                    fields: Arc::from(values),
                }))
            }
            BuiltinValue::Method(BuiltinMethod::BlobFromString) => {
                self.require_no_kwargs("Blob.from_string", kwargs)?;
                self.check_arity("Blob.from_string", args, 1)?;
                let value = &args[0];
                let value = self.force_value(value.clone())?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Blob(BlobValue::from_string(text)))
            }
            BuiltinValue::Method(BuiltinMethod::BytesFromString) => {
                self.require_no_kwargs("Bytes", kwargs)?;
                self.check_arity("Bytes", args, 1)?;
                let value = self.force_value(args[0].clone())?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Bytes(Arc::from(text.as_bytes().to_vec())))
            }
            BuiltinValue::Method(BuiltinMethod::TreeOf) => {
                self.require_no_kwargs("Tree.of", kwargs)?;
                self.check_arity("Tree.of", args, 1)?;
                let value = &args[0];
                let (value, _) = self.split_obligations(self.force_value_shallow(value.clone())?);
                let Value::Map(map) = value else {
                    return Err(
                        self.type_error(&format!("Tree.of expects Map, got {}", value.type_name()))
                    );
                };
                self.tree_from_map(&map)
            }
            BuiltinValue::Method(BuiltinMethod::TreeEmpty) => {
                self.require_no_kwargs("Tree.empty", kwargs)?;
                self.check_arity("Tree.empty", args, 0)?;
                Ok(Value::Tree(TreeValue::Inline(Arc::from([]))))
            }
            BuiltinValue::Method(BuiltinMethod::TreeFromPath) => {
                self.require_no_kwargs("Tree.from_path", kwargs)?;
                self.check_arity("Tree.from_path", args, 1)?;
                let path = self.force_value(args[0].clone())?;
                let path = self.expect_path_value(&path, "Tree.from_path path")?;
                Ok(Value::Tree(TreeValue::FsDir(path)))
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMap { receiver }) => {
                self.require_no_kwargs("array.map", kwargs)?;
                self.check_arity("array.map", args, 1)?;
                self.array_map(receiver, args[0].clone())
            }
            BuiltinValue::Method(BuiltinMethod::ArrayToMap { receiver }) => {
                self.require_no_kwargs("array.to_map", kwargs)?;
                self.check_arity("array.to_map", args, 1)?;
                self.array_to_map(receiver, args[0].clone())
            }
            BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver }) => {
                self.require_no_kwargs("array.flatten", kwargs)?;
                self.check_arity("array.flatten", args, 0)?;
                self.array_flatten(receiver)
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver }) => {
                self.require_no_kwargs("array.merge", kwargs)?;
                self.check_arity("array.merge", args, 0)?;
                self.array_merge(receiver)
            }
            BuiltinValue::Method(BuiltinMethod::MapMapValues { receiver }) => {
                self.require_no_kwargs("map.map_values", kwargs)?;
                self.check_arity("map.map_values", args, 1)?;
                self.map_map_values(receiver, args[0].clone())
            }
            BuiltinValue::Method(BuiltinMethod::BlobToString { receiver }) => {
                self.require_no_kwargs("blob.to_string", kwargs)?;
                self.check_arity("blob.to_string", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value(receiver.as_ref().clone())?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.to_string receiver must be Blob"));
                };
                let text = blob.to_utf8_string().map_err(|message| {
                    PoisonError::new(
                        PoisonKind::TypeError,
                        message,
                        self.current_demand_chain(None),
                    )
                })?;
                Ok(self.attach_obligations(Value::String(text), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BlobExecutable { receiver }) => {
                self.require_no_kwargs("blob.executable", kwargs)?;
                self.check_arity("blob.executable", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value(receiver.as_ref().clone())?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.executable receiver must be Blob"));
                };
                let blob = match blob {
                    BlobValue::Inline { bytes, .. } => BlobValue::Inline {
                        bytes,
                        executable: true,
                    },
                    BlobValue::FsFile { path, .. } => BlobValue::FsFile {
                        path,
                        executable: true,
                    },
                    BlobValue::Store { blob_ref, .. } => BlobValue::Store {
                        blob_ref,
                        executable: true,
                    },
                };
                Ok(self.attach_obligations(Value::Blob(blob), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8 { receiver }) => {
                self.require_no_kwargs("blob.utf8", kwargs)?;
                self.check_arity("blob.utf8", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value(receiver.as_ref().clone())?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.utf8 receiver must be Blob"));
                };
                let text = blob.to_utf8_string().map_err(|message| {
                    PoisonError::new(
                        PoisonKind::TypeError,
                        message,
                        self.current_demand_chain(None),
                    )
                })?;
                Ok(self.attach_obligations(Value::String(text), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { receiver }) => {
                self.require_no_kwargs("blob.utf8_lossy", kwargs)?;
                self.check_arity("blob.utf8_lossy", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value(receiver.as_ref().clone())?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.utf8_lossy receiver must be Blob"));
                };
                let text = match blob {
                    BlobValue::Inline { bytes, .. } => {
                        String::from_utf8_lossy(bytes.as_ref()).into_owned()
                    }
                    BlobValue::FsFile { path, .. } => {
                        let bytes = std::fs::read(path.as_str()).map_err(|error| {
                            self.type_error(&format!("failed to read {}: {error}", path.as_str()))
                        })?;
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    BlobValue::Store { blob_ref, .. } => {
                        return Err(self.type_error(&format!(
                            "store-backed blob {:?} requires an external store resolver",
                            blob_ref
                        )));
                    }
                };
                Ok(self.attach_obligations(Value::String(Arc::from(text)), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8 { receiver }) => {
                self.require_no_kwargs("bytes.utf8", kwargs)?;
                self.check_arity("bytes.utf8", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value(receiver.as_ref().clone())?);
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.utf8 receiver must be Bytes"));
                };
                let text = String::from_utf8(bytes.as_ref().to_vec()).map_err(|error| {
                    PoisonError::new(
                        PoisonKind::TypeError,
                        format!("bytes are not UTF-8: {error}"),
                        self.current_demand_chain(None),
                    )
                })?;
                Ok(self.attach_obligations(Value::String(Arc::from(text)), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { receiver }) => {
                self.require_no_kwargs("bytes.utf8_lossy", kwargs)?;
                self.check_arity("bytes.utf8_lossy", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value(receiver.as_ref().clone())?);
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.utf8_lossy receiver must be Bytes"));
                };
                let text = String::from_utf8_lossy(bytes.as_ref()).into_owned();
                Ok(self.attach_obligations(Value::String(Arc::from(text)), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BytesBlob { receiver }) => {
                self.require_no_kwargs("bytes.blob", kwargs)?;
                self.check_arity("bytes.blob", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value(receiver.as_ref().clone())?);
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.blob receiver must be Bytes"));
                };
                Ok(self.attach_obligations(
                    Value::Blob(BlobValue::Inline {
                        bytes,
                        executable: false,
                    }),
                    obligations,
                ))
            }
            BuiltinValue::Method(BuiltinMethod::StreamCollect { receiver }) => {
                self.require_no_kwargs("stream.collect", kwargs)?;
                self.check_arity("stream.collect", args, 0)?;
                self.force_value(receiver.as_ref().clone())
            }
            BuiltinValue::Method(BuiltinMethod::TreeFile { receiver }) => {
                self.require_no_kwargs("tree.file", kwargs)?;
                self.check_arity("tree.file", args, 1)?;
                let path = &args[0];
                let path = self.force_value(path.clone())?;
                let path = self.expect_relative_path(&path, "tree.file path")?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value_shallow(receiver.as_ref().clone())?);
                let Value::Tree(tree) = receiver else {
                    return Err(self.type_error("tree.file receiver must be Tree"));
                };
                let result = self.tree_file(&tree, path)?;
                Ok(self.attach_obligations(result, obligations))
            }
            BuiltinValue::Method(BuiltinMethod::TreeMerge { receiver }) => {
                self.require_no_kwargs("tree.merge", kwargs)?;
                self.check_arity("tree.merge", args, 1)?;
                let (other, other_obligations) =
                    self.split_obligations(self.force_value_shallow(args[0].clone())?);
                let Value::Tree(other) = other else {
                    return Err(self.type_error("tree.merge expects Tree"));
                };
                let (receiver, obligations) =
                    self.split_obligations(self.force_value_shallow(receiver.as_ref().clone())?);
                let Value::Tree(receiver) = receiver else {
                    return Err(self.type_error("tree.merge receiver must be Tree"));
                };
                let result = self.tree_merge(&receiver, &other)?;
                Ok(self
                    .attach_obligations(result, obligations.into_iter().chain(other_obligations)))
            }
            BuiltinValue::Method(BuiltinMethod::TreeSubtree { receiver }) => {
                self.require_no_kwargs("tree.subtree", kwargs)?;
                self.check_arity("tree.subtree", args, 1)?;
                let path = self.force_value(args[0].clone())?;
                let path = self.expect_relative_path(&path, "tree.subtree path")?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value_shallow(receiver.as_ref().clone())?);
                let Value::Tree(tree) = receiver else {
                    return Err(self.type_error("tree.subtree receiver must be Tree"));
                };
                let result = self.tree_subtree(&tree, path)?;
                Ok(self.attach_obligations(result, obligations))
            }
            BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver }) => {
                self.require_no_kwargs("option.unwrap_or", kwargs)?;
                self.check_arity("option.unwrap_or", args, 1)?;
                let (receiver, obligations) =
                    self.split_obligations(self.force_value_shallow(receiver.as_ref().clone())?);
                let Value::Struct(option) = receiver else {
                    return Err(self.type_error("option.unwrap_or receiver must be Option"));
                };
                let result = match option.type_name.as_ref() {
                    "Some" => option
                        .fields
                        .iter()
                        .find(|(name, _)| name.as_ref() == "value")
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| self.type_error("Some.unwrap_or requires a value field")),
                    "None" => Ok(args[0].clone()),
                    _ => Err(self.type_error("option.unwrap_or receiver must be Option")),
                }?;
                Ok(self.attach_obligations(result, obligations))
            }
            BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver }) => {
                self.require_no_kwargs("fallible", kwargs)?;
                self.check_arity("fallible", args, 0)?;
                Ok(self.build_fallible_value(receiver.as_ref().clone()))
            }
            BuiltinValue::Method(BuiltinMethod::HostRead { receiver }) => {
                self.require_no_kwargs("host.read", kwargs)?;
                self.check_arity("host.read", args, 1)?;
                let path = self.force_value(args[0].clone())?;
                let path = self.expect_host_path(&path, "host.read path")?;
                let receiver = self.force_value_shallow(receiver.as_ref().clone())?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.read receiver must be Host"));
                };
                self.host_read(&path)
            }
            BuiltinValue::Method(BuiltinMethod::HostHash { receiver }) => {
                self.require_no_kwargs("host.hash", kwargs)?;
                self.check_arity("host.hash", args, 1)?;
                let path = self.force_value(args[0].clone())?;
                let path = self.expect_host_path(&path, "host.hash path")?;
                let receiver = self.force_value_shallow(receiver.as_ref().clone())?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.hash receiver must be Host"));
                };
                self.host_hash(&path)
            }
            BuiltinValue::Method(BuiltinMethod::HostWhich { receiver }) => {
                self.require_no_kwargs("host.which", kwargs)?;
                self.check_arity("host.which", args, 1)?;
                let name = self.force_value(args[0].clone())?;
                let name = self.expect_host_path(&name, "host.which name")?;
                let receiver = self.force_value_shallow(receiver.as_ref().clone())?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.which receiver must be Host"));
                };
                self.host_which(&name)
            }
        }
    }

    fn invoke_builtin_scheduled(
        &self,
        builtin: &BuiltinValue,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        match builtin {
            BuiltinValue::Namespace(BuiltinNamespace::Blob) => {
                self.require_no_kwargs("Blob", kwargs)?;
                self.check_arity("Blob", args, 1)?;
                let value =
                    self.materialize_scheduled_value_deep(args[0].clone(), "forcing Blob value")?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Blob(BlobValue::from_string(text)))
            }
            BuiltinValue::Namespace(BuiltinNamespace::Bytes) => {
                self.require_no_kwargs("Bytes", kwargs)?;
                self.check_arity("Bytes", args, 1)?;
                let value =
                    self.materialize_scheduled_value_deep(args[0].clone(), "forcing Bytes value")?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Bytes(Arc::from(text.as_bytes().to_vec())))
            }
            BuiltinValue::Namespace(BuiltinNamespace::Tree) => {
                self.require_no_kwargs("Tree", kwargs)?;
                self.check_arity("Tree", args, 1)?;
                let (value, _) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        args[0].clone(),
                        "forcing Tree input",
                    )?);
                match value {
                    Value::Map(map) => self.tree_from_map_scheduled(&map),
                    Value::Array(_) => self.array_merge_scheduled(&value),
                    Value::Path(path) => Ok(Value::Tree(TreeValue::FsDir(path))),
                    Value::String(path) => {
                        Ok(Value::Tree(TreeValue::FsDir(PathValue::new(path.as_ref()))))
                    }
                    other => Err(self.type_error(&format!(
                        "Tree expects Map, Array, or Path/String, got {}",
                        other.type_name()
                    ))),
                }
            }
            BuiltinValue::Function(BuiltinFunction::Exec) => {
                self.exec_builtin_scheduled(args, kwargs, kwargs_expr)
            }
            BuiltinValue::Function(BuiltinFunction::Discover) => {
                self.discover_builtin_scheduled(args, kwargs, kwargs_expr)
            }
            BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                self.executor_class_builtin_scheduled(args, kwargs, kwargs_expr)
            }
            BuiltinValue::Function(BuiltinFunction::StructConstructor { type_name, fields }) => {
                self.require_no_kwargs(type_name.as_ref(), kwargs)?;
                self.check_arity(type_name.as_ref(), args, 1)?;
                let (value, _) = self.split_obligations(self.materialize_scheduled_value_shallow(
                    args[0].clone(),
                    &format!("forcing {type_name} payload"),
                )?);
                let Value::Map(map) = value else {
                    return Err(self.type_error(&format!(
                        "{} expects a record/map payload, got {}",
                        type_name,
                        value.type_name()
                    )));
                };
                let mut values = Vec::with_capacity(fields.len());
                for field in fields.iter() {
                    let field_value = map
                        .entries
                        .iter()
                        .find_map(|(key, value)| match key {
                            Value::String(name) if name.as_ref() == field.as_ref() => {
                                Some(value.clone())
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            self.type_error(&format!("{} requires field '{}'", type_name, field))
                        })?;
                    values.push((Arc::<str>::clone(field), field_value));
                }
                Ok(Value::Struct(StructValue {
                    type_name: Arc::clone(type_name),
                    fields: Arc::from(values),
                }))
            }
            BuiltinValue::Method(BuiltinMethod::BlobFromString) => {
                self.require_no_kwargs("Blob.from_string", kwargs)?;
                self.check_arity("Blob.from_string", args, 1)?;
                let value = self.materialize_scheduled_value_deep(
                    args[0].clone(),
                    "forcing Blob.from_string value",
                )?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Blob(BlobValue::from_string(text)))
            }
            BuiltinValue::Method(BuiltinMethod::BytesFromString) => {
                self.require_no_kwargs("Bytes", kwargs)?;
                self.check_arity("Bytes", args, 1)?;
                let value = self.materialize_scheduled_value_deep(
                    args[0].clone(),
                    "forcing Bytes.from_string value",
                )?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Bytes(Arc::from(text.as_bytes().to_vec())))
            }
            BuiltinValue::Method(BuiltinMethod::TreeOf) => {
                self.require_no_kwargs("Tree.of", kwargs)?;
                self.check_arity("Tree.of", args, 1)?;
                let (value, _) = self.split_obligations(self.materialize_scheduled_value_shallow(
                    args[0].clone(),
                    "forcing Tree.of input",
                )?);
                let Value::Map(map) = value else {
                    return Err(
                        self.type_error(&format!("Tree.of expects Map, got {}", value.type_name()))
                    );
                };
                self.tree_from_map_scheduled(&map)
            }
            BuiltinValue::Method(BuiltinMethod::TreeEmpty) => {
                self.require_no_kwargs("Tree.empty", kwargs)?;
                self.check_arity("Tree.empty", args, 0)?;
                Ok(Value::Tree(TreeValue::Inline(Arc::from([]))))
            }
            BuiltinValue::Method(BuiltinMethod::TreeFromPath) => {
                self.require_no_kwargs("Tree.from_path", kwargs)?;
                self.check_arity("Tree.from_path", args, 1)?;
                let path = self
                    .materialize_scheduled_value_deep(args[0].clone(), "forcing Tree.from_path")?;
                let path = self.expect_path_value(&path, "Tree.from_path path")?;
                Ok(Value::Tree(TreeValue::FsDir(path)))
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMap { receiver }) => {
                self.require_no_kwargs("array.map", kwargs)?;
                self.check_arity("array.map", args, 1)?;
                self.array_map_scheduled(receiver, args[0].clone())
            }
            BuiltinValue::Method(BuiltinMethod::ArrayToMap { receiver }) => {
                self.require_no_kwargs("array.to_map", kwargs)?;
                self.check_arity("array.to_map", args, 1)?;
                self.array_to_map_scheduled(receiver, args[0].clone())
            }
            BuiltinValue::Method(BuiltinMethod::MapMapValues { receiver }) => {
                self.require_no_kwargs("map.map_values", kwargs)?;
                self.check_arity("map.map_values", args, 1)?;
                self.map_map_values_scheduled(receiver, args[0].clone())
            }
            BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver }) => {
                self.require_no_kwargs("array.flatten", kwargs)?;
                self.check_arity("array.flatten", args, 0)?;
                self.array_flatten_scheduled(receiver)
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver }) => {
                self.require_no_kwargs("array.merge", kwargs)?;
                self.check_arity("array.merge", args, 0)?;
                self.array_merge_scheduled(receiver)
            }
            BuiltinValue::Method(BuiltinMethod::BlobToString { receiver }) => {
                self.require_no_kwargs("blob.to_string", kwargs)?;
                self.check_arity("blob.to_string", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.to_string receiver",
                    )?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.to_string receiver must be Blob"));
                };
                let text = blob.to_utf8_string().map_err(|message| {
                    PoisonError::new(
                        PoisonKind::TypeError,
                        message,
                        self.current_demand_chain(None),
                    )
                })?;
                Ok(self.attach_obligations(Value::String(text), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BlobExecutable { receiver }) => {
                self.require_no_kwargs("blob.executable", kwargs)?;
                self.check_arity("blob.executable", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.executable receiver",
                    )?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.executable receiver must be Blob"));
                };
                let blob = match blob {
                    BlobValue::Inline { bytes, .. } => BlobValue::Inline {
                        bytes,
                        executable: true,
                    },
                    BlobValue::FsFile { path, .. } => BlobValue::FsFile {
                        path,
                        executable: true,
                    },
                    BlobValue::Store { blob_ref, .. } => BlobValue::Store {
                        blob_ref,
                        executable: true,
                    },
                };
                Ok(self.attach_obligations(Value::Blob(blob), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8 { receiver }) => {
                self.require_no_kwargs("blob.utf8", kwargs)?;
                self.check_arity("blob.utf8", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.utf8 receiver",
                    )?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.utf8 receiver must be Blob"));
                };
                let text = blob.to_utf8_string().map_err(|message| {
                    PoisonError::new(
                        PoisonKind::TypeError,
                        message,
                        self.current_demand_chain(None),
                    )
                })?;
                Ok(self.attach_obligations(Value::String(text), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { receiver }) => {
                self.require_no_kwargs("blob.utf8_lossy", kwargs)?;
                self.check_arity("blob.utf8_lossy", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.utf8_lossy receiver",
                    )?);
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.utf8_lossy receiver must be Blob"));
                };
                let text = match blob {
                    BlobValue::Inline { bytes, .. } => {
                        String::from_utf8_lossy(bytes.as_ref()).into_owned()
                    }
                    BlobValue::FsFile { path, .. } => {
                        let bytes = std::fs::read(path.as_str()).map_err(|error| {
                            self.type_error(&format!("failed to read {}: {error}", path.as_str()))
                        })?;
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                    BlobValue::Store { blob_ref, .. } => {
                        return Err(self.type_error(&format!(
                            "store-backed blob {:?} requires an external store resolver",
                            blob_ref
                        )));
                    }
                };
                Ok(self.attach_obligations(Value::String(Arc::from(text)), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8 { receiver }) => {
                self.require_no_kwargs("bytes.utf8", kwargs)?;
                self.check_arity("bytes.utf8", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing bytes.utf8 receiver",
                    )?);
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.utf8 receiver must be Bytes"));
                };
                let text = String::from_utf8(bytes.as_ref().to_vec()).map_err(|error| {
                    PoisonError::new(
                        PoisonKind::TypeError,
                        format!("bytes are not UTF-8: {error}"),
                        self.current_demand_chain(None),
                    )
                })?;
                Ok(self.attach_obligations(Value::String(Arc::from(text)), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { receiver }) => {
                self.require_no_kwargs("bytes.utf8_lossy", kwargs)?;
                self.check_arity("bytes.utf8_lossy", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing bytes.utf8_lossy receiver",
                    )?);
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.utf8_lossy receiver must be Bytes"));
                };
                let text = String::from_utf8_lossy(bytes.as_ref()).into_owned();
                Ok(self.attach_obligations(Value::String(Arc::from(text)), obligations))
            }
            BuiltinValue::Method(BuiltinMethod::BytesBlob { receiver }) => {
                self.require_no_kwargs("bytes.blob", kwargs)?;
                self.check_arity("bytes.blob", args, 0)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing bytes.blob receiver",
                    )?);
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.blob receiver must be Bytes"));
                };
                Ok(self.attach_obligations(
                    Value::Blob(BlobValue::Inline {
                        bytes,
                        executable: false,
                    }),
                    obligations,
                ))
            }
            BuiltinValue::Method(BuiltinMethod::StreamCollect { receiver }) => {
                self.require_no_kwargs("stream.collect", kwargs)?;
                self.check_arity("stream.collect", args, 0)?;
                self.materialize_scheduled_value_deep(
                    receiver.as_ref().clone(),
                    "forcing stream.collect receiver",
                )
            }
            BuiltinValue::Method(BuiltinMethod::TreeFile { receiver }) => {
                self.require_no_kwargs("tree.file", kwargs)?;
                self.check_arity("tree.file", args, 1)?;
                let path = self
                    .materialize_scheduled_value_deep(args[0].clone(), "forcing tree.file path")?;
                let path = self.expect_relative_path(&path, "tree.file path")?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing tree.file receiver",
                    )?);
                let Value::Tree(tree) = receiver else {
                    return Err(self.type_error("tree.file receiver must be Tree"));
                };
                let result = self.tree_file(&tree, path)?;
                Ok(self.attach_obligations(result, obligations))
            }
            BuiltinValue::Method(BuiltinMethod::TreeMerge { receiver }) => {
                self.require_no_kwargs("tree.merge", kwargs)?;
                self.check_arity("tree.merge", args, 1)?;
                let (other, other_obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        args[0].clone(),
                        "forcing tree.merge other",
                    )?);
                let Value::Tree(other) = other else {
                    return Err(self.type_error("tree.merge expects Tree"));
                };
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing tree.merge receiver",
                    )?);
                let Value::Tree(receiver) = receiver else {
                    return Err(self.type_error("tree.merge receiver must be Tree"));
                };
                let result = self.tree_merge(&receiver, &other)?;
                Ok(self
                    .attach_obligations(result, obligations.into_iter().chain(other_obligations)))
            }
            BuiltinValue::Method(BuiltinMethod::TreeSubtree { receiver }) => {
                self.require_no_kwargs("tree.subtree", kwargs)?;
                self.check_arity("tree.subtree", args, 1)?;
                let path = self.materialize_scheduled_value_deep(
                    args[0].clone(),
                    "forcing tree.subtree path",
                )?;
                let path = self.expect_relative_path(&path, "tree.subtree path")?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing tree.subtree receiver",
                    )?);
                let Value::Tree(tree) = receiver else {
                    return Err(self.type_error("tree.subtree receiver must be Tree"));
                };
                let result = self.tree_subtree(&tree, path)?;
                Ok(self.attach_obligations(result, obligations))
            }
            BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver }) => {
                self.require_no_kwargs("option.unwrap_or", kwargs)?;
                self.check_arity("option.unwrap_or", args, 1)?;
                let (receiver, obligations) =
                    self.split_obligations(self.materialize_scheduled_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing option.unwrap_or receiver",
                    )?);
                let Value::Struct(option) = receiver else {
                    return Err(self.type_error("option.unwrap_or receiver must be Option"));
                };
                let result = match option.type_name.as_ref() {
                    "Some" => option
                        .fields
                        .iter()
                        .find(|(name, _)| name.as_ref() == "value")
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| self.type_error("Some.unwrap_or requires a value field")),
                    "None" => Ok(args[0].clone()),
                    _ => Err(self.type_error("option.unwrap_or receiver must be Option")),
                }?;
                Ok(self.attach_obligations(result, obligations))
            }
            BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver }) => {
                self.require_no_kwargs("fallible", kwargs)?;
                self.check_arity("fallible", args, 0)?;
                Ok(self.build_fallible_value(receiver.as_ref().clone()))
            }
            BuiltinValue::Method(BuiltinMethod::HostRead { receiver }) => {
                self.require_no_kwargs("host.read", kwargs)?;
                self.check_arity("host.read", args, 1)?;
                let path =
                    self.materialize_scheduled_value_deep(args[0].clone(), "forcing host.read")?;
                let path = self.expect_host_path(&path, "host.read path")?;
                let receiver = self.materialize_scheduled_value_shallow(
                    receiver.as_ref().clone(),
                    "forcing host.read receiver",
                )?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.read receiver must be Host"));
                };
                self.host_read(&path)
            }
            BuiltinValue::Method(BuiltinMethod::HostHash { receiver }) => {
                self.require_no_kwargs("host.hash", kwargs)?;
                self.check_arity("host.hash", args, 1)?;
                let path =
                    self.materialize_scheduled_value_deep(args[0].clone(), "forcing host.hash")?;
                let path = self.expect_host_path(&path, "host.hash path")?;
                let receiver = self.materialize_scheduled_value_shallow(
                    receiver.as_ref().clone(),
                    "forcing host.hash receiver",
                )?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.hash receiver must be Host"));
                };
                self.host_hash(&path)
            }
            BuiltinValue::Method(BuiltinMethod::HostWhich { receiver }) => {
                self.require_no_kwargs("host.which", kwargs)?;
                self.check_arity("host.which", args, 1)?;
                let name =
                    self.materialize_scheduled_value_deep(args[0].clone(), "forcing host.which")?;
                let name = self.expect_host_path(&name, "host.which name")?;
                let receiver = self.materialize_scheduled_value_shallow(
                    receiver.as_ref().clone(),
                    "forcing host.which receiver",
                )?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.which receiver must be Host"));
                };
                self.host_which(&name)
            }
        }
    }

    fn tree_from_map(&self, map: &MapValue) -> Result<Value, PoisonError> {
        fn insert_tree_path(
            engine: &Engine,
            entries: &mut BTreeMap<Arc<str>, Value>,
            segments: &[&str],
            value: Value,
        ) -> Result<(), PoisonError> {
            let Some((head, tail)) = segments.split_first() else {
                return Err(engine.type_error("Tree entry path cannot be empty"));
            };
            if tail.is_empty() {
                if entries.contains_key(*head) {
                    return Err(engine.type_error(&format!(
                        "Tree contains duplicate entry '{}'",
                        segments.join("/")
                    )));
                }
                entries.insert(Arc::from(*head), value);
                return Ok(());
            }

            let child = entries
                .entry(Arc::from(*head))
                .or_insert_with(|| Value::Tree(TreeValue::Inline(Arc::from([]))));
            match child {
                Value::Tree(TreeValue::Inline(existing)) => {
                    let mut nested = existing
                        .iter()
                        .cloned()
                        .collect::<BTreeMap<Arc<str>, Value>>();
                    insert_tree_path(engine, &mut nested, tail, value)?;
                    *child = Value::Tree(TreeValue::Inline(Arc::from(
                        nested.into_iter().collect::<Vec<_>>(),
                    )));
                    Ok(())
                }
                Value::Tree(TreeValue::FsDir(_))
                | Value::Tree(TreeValue::Store(_))
                | Value::Blob(_)
                | Value::Pending(_) => Err(engine.type_error(&format!(
                    "Tree entry path '{}' collides with an existing non-inline tree entry",
                    segments.join("/")
                ))),
                other => Err(engine.type_error(&format!(
                    "tree entries must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            }
        }

        let mut nested = BTreeMap::<Arc<str>, Value>::new();
        for (key, value) in map.entries.iter() {
            let key = self.force_value(key.clone())?;
            let path = self.expect_relative_path(&key, "Tree entry path")?;
            let raw = path.as_str();
            if raw.is_empty() || raw == "." {
                return Err(self.type_error("Tree entry path cannot be empty"));
            }
            let segments = raw.split('/').collect::<Vec<_>>();
            insert_tree_path(self, &mut nested, &segments, value.clone())?;
        }
        self.validate_tree_entries(&nested.values().cloned().collect::<Vec<_>>())?;
        Ok(Value::Tree(TreeValue::Inline(Arc::from(
            nested.into_iter().collect::<Vec<_>>(),
        ))))
    }

    fn tree_from_map_scheduled(&self, map: &MapValue) -> Result<Value, PoisonError> {
        fn insert_tree_path(
            engine: &Engine,
            entries: &mut BTreeMap<Arc<str>, Value>,
            segments: &[&str],
            value: Value,
        ) -> Result<(), PoisonError> {
            let Some((head, tail)) = segments.split_first() else {
                return Err(engine.type_error("Tree entry path cannot be empty"));
            };
            if tail.is_empty() {
                if entries.contains_key(*head) {
                    return Err(engine.type_error(&format!(
                        "Tree contains duplicate entry '{}'",
                        segments.join("/")
                    )));
                }
                entries.insert(Arc::from(*head), value);
                return Ok(());
            }

            let child = entries
                .entry(Arc::from(*head))
                .or_insert_with(|| Value::Tree(TreeValue::Inline(Arc::from([]))));
            match child {
                Value::Tree(TreeValue::Inline(existing)) => {
                    let mut nested = existing
                        .iter()
                        .cloned()
                        .collect::<BTreeMap<Arc<str>, Value>>();
                    insert_tree_path(engine, &mut nested, tail, value)?;
                    *child = Value::Tree(TreeValue::Inline(Arc::from(
                        nested.into_iter().collect::<Vec<_>>(),
                    )));
                    Ok(())
                }
                Value::Tree(TreeValue::FsDir(_))
                | Value::Tree(TreeValue::Store(_))
                | Value::Blob(_)
                | Value::Pending(_) => Err(engine.type_error(&format!(
                    "Tree entry path '{}' collides with an existing non-inline tree entry",
                    segments.join("/")
                ))),
                other => Err(engine.type_error(&format!(
                    "tree entries must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            }
        }

        let mut nested = BTreeMap::<Arc<str>, Value>::new();
        for (key, value) in map.entries.iter() {
            let key =
                self.materialize_scheduled_value_deep(key.clone(), "forcing Tree entry path")?;
            let path = self.expect_relative_path(&key, "Tree entry path")?;
            let raw = path.as_str();
            if raw.is_empty() || raw == "." {
                return Err(self.type_error("Tree entry path cannot be empty"));
            }
            let segments = raw.split('/').collect::<Vec<_>>();
            insert_tree_path(self, &mut nested, &segments, value.clone())?;
        }
        self.validate_tree_entries(&nested.values().cloned().collect::<Vec<_>>())?;
        Ok(Value::Tree(TreeValue::Inline(Arc::from(
            nested.into_iter().collect::<Vec<_>>(),
        ))))
    }

    fn array_map(&self, receiver: &Value, callback: Value) -> Result<Value, PoisonError> {
        let callback = self.force_value(callback)?;
        let (receiver, obligations) =
            self.split_obligations(self.force_value_shallow(receiver.clone())?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.map receiver must be Array"));
        };

        let mut out = Vec::with_capacity(items.len());
        for item in items.iter() {
            out.push(self.execute_call(
                callback.clone(),
                Arc::from(vec![item.clone()]),
                Arc::from(Vec::<(Arc<str>, Value)>::new()),
                None,
            )?);
        }

        Ok(self.attach_obligations(Value::Array(Arc::from(out)), obligations))
    }

    fn array_map_scheduled(&self, receiver: &Value, callback: Value) -> Result<Value, PoisonError> {
        let (receiver, obligations) =
            self.split_obligations(self.materialize_scheduled_value_shallow(
                receiver.clone(),
                "forcing array.map receiver",
            )?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.map receiver must be Array"));
        };

        let out = items
            .iter()
            .map(|item| self.build_call(callback.clone(), vec![item.clone()], None))
            .collect::<Vec<_>>();

        Ok(self.attach_obligations(Value::Array(Arc::from(out)), obligations))
    }

    fn array_to_map(&self, receiver: &Value, callback: Value) -> Result<Value, PoisonError> {
        let callback = self.force_value(callback)?;
        let (receiver, obligations) =
            self.split_obligations(self.force_value_shallow(receiver.clone())?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.to_map receiver must be Array"));
        };

        let mut entries = Vec::with_capacity(items.len());
        for item in items.iter() {
            let value = self.execute_call(
                callback.clone(),
                Arc::from(vec![item.clone()]),
                Arc::from(Vec::<(Arc<str>, Value)>::new()),
                None,
            )?;
            entries.push((item.clone(), value));
        }

        Ok(self.attach_obligations(
            Value::Map(MapValue {
                entries: Arc::from(entries),
            }),
            obligations,
        ))
    }

    fn array_to_map_scheduled(
        &self,
        receiver: &Value,
        callback: Value,
    ) -> Result<Value, PoisonError> {
        let (receiver, obligations) =
            self.split_obligations(self.materialize_scheduled_value_shallow(
                receiver.clone(),
                "forcing array.to_map receiver",
            )?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.to_map receiver must be Array"));
        };

        let entries = items
            .iter()
            .map(|item| {
                (
                    item.clone(),
                    self.build_call(callback.clone(), vec![item.clone()], None),
                )
            })
            .collect::<Vec<_>>();

        Ok(self.attach_obligations(
            Value::Map(MapValue {
                entries: Arc::from(entries),
            }),
            obligations,
        ))
    }

    fn array_flatten(&self, receiver: &Value) -> Result<Value, PoisonError> {
        let (receiver, obligations) =
            self.split_obligations(self.force_value_shallow(receiver.clone())?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.flatten receiver must be Array"));
        };

        let forced_items = self.force_many_shallow(items.iter().cloned().collect())?;
        let mut out = Vec::new();
        for item in forced_items {
            let (item, item_obligations) = self.split_obligations(item);
            let Value::Array(inner) = item else {
                return Err(self.type_error("array.flatten expects Array items"));
            };
            for value in inner.iter() {
                out.push(self.attach_obligations(value.clone(), item_obligations.iter().cloned()));
            }
        }

        Ok(self.attach_obligations(Value::Array(Arc::from(out)), obligations))
    }

    fn array_flatten_scheduled(&self, receiver: &Value) -> Result<Value, PoisonError> {
        let (receiver, obligations) =
            self.split_obligations(self.materialize_scheduled_value_shallow(
                receiver.clone(),
                "forcing array.flatten receiver",
            )?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.flatten receiver must be Array"));
        };

        let mut out = Vec::new();
        for item in items.iter() {
            let (item, item_obligations) =
                self.split_obligations(self.materialize_scheduled_value_shallow(
                    item.clone(),
                    "forcing array.flatten item",
                )?);
            let Value::Array(inner) = item else {
                return Err(self.type_error("array.flatten expects Array items"));
            };
            for value in inner.iter() {
                out.push(self.attach_obligations(value.clone(), item_obligations.iter().cloned()));
            }
        }

        Ok(self.attach_obligations(Value::Array(Arc::from(out)), obligations))
    }

    fn array_merge(&self, receiver: &Value) -> Result<Value, PoisonError> {
        let (receiver, receiver_obligations) =
            self.split_obligations(self.force_value_shallow(receiver.clone())?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.merge receiver must be Array"));
        };

        if items.is_empty() {
            return Ok(self.attach_obligations(
                Value::Tree(TreeValue::Inline(Arc::from([]))),
                receiver_obligations,
            ));
        }

        let mut forced_items = self.force_many(items.iter().cloned().collect())?;
        let first = forced_items.remove(0);
        let (first, mut obligations) = self.split_obligations(first);
        let Value::Tree(mut merged) = first else {
            return Err(self.type_error("array.merge expects Tree items"));
        };
        obligations.extend(receiver_obligations);

        for item in forced_items {
            let (item, item_obligations) = self.split_obligations(item);
            let Value::Tree(tree) = item else {
                return Err(self.type_error("array.merge expects Tree items"));
            };
            merged = match self.tree_merge(&merged, &tree)? {
                Value::Tree(tree) => tree,
                other => {
                    return Err(self.type_error(&format!(
                        "array.merge expected Tree result, got {}",
                        other.type_name()
                    )));
                }
            };
            obligations.extend(item_obligations);
        }

        Ok(self.attach_obligations(Value::Tree(merged), obligations))
    }

    fn array_merge_scheduled(&self, receiver: &Value) -> Result<Value, PoisonError> {
        let (receiver, receiver_obligations) =
            self.split_obligations(self.materialize_scheduled_value_shallow(
                receiver.clone(),
                "forcing array.merge receiver",
            )?);
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.merge receiver must be Array"));
        };

        if items.is_empty() {
            return Ok(self.attach_obligations(
                Value::Tree(TreeValue::Inline(Arc::from([]))),
                receiver_obligations,
            ));
        }

        let mut materialized = self.materialize_scheduled_many_deep(
            items.iter().cloned().collect(),
            "forcing array.merge items",
        )?;
        let first = materialized.remove(0);
        let (first, mut obligations) = self.split_obligations(first);
        let Value::Tree(mut merged) = first else {
            return Err(self.type_error("array.merge expects Tree items"));
        };
        obligations.extend(receiver_obligations);

        for item in materialized {
            let (item, item_obligations) = self.split_obligations(item);
            let Value::Tree(tree) = item else {
                return Err(self.type_error("array.merge expects Tree items"));
            };
            merged = match self.tree_merge(&merged, &tree)? {
                Value::Tree(tree) => tree,
                other => {
                    return Err(self.type_error(&format!(
                        "array.merge expected Tree result, got {}",
                        other.type_name()
                    )));
                }
            };
            obligations.extend(item_obligations);
        }

        Ok(self.attach_obligations(Value::Tree(merged), obligations))
    }

    fn map_map_values(&self, receiver: &Value, callback: Value) -> Result<Value, PoisonError> {
        let callback = self.force_value(callback)?;
        let (receiver, obligations) =
            self.split_obligations(self.force_value_shallow(receiver.clone())?);
        let Value::Map(map) = receiver else {
            return Err(self.type_error("map.map_values receiver must be Map"));
        };

        let mut entries = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let mapped = self.execute_call(
                callback.clone(),
                Arc::from(vec![value.clone()]),
                Arc::from(Vec::<(Arc<str>, Value)>::new()),
                None,
            )?;
            entries.push((key.clone(), mapped));
        }

        Ok(self.attach_obligations(
            Value::Map(MapValue {
                entries: Arc::from(entries),
            }),
            obligations,
        ))
    }

    fn map_map_values_scheduled(
        &self,
        receiver: &Value,
        callback: Value,
    ) -> Result<Value, PoisonError> {
        let (receiver, obligations) =
            self.split_obligations(self.materialize_scheduled_value_shallow(
                receiver.clone(),
                "forcing map.map_values receiver",
            )?);
        let Value::Map(map) = receiver else {
            return Err(self.type_error("map.map_values receiver must be Map"));
        };

        let entries = map
            .entries
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    self.build_call(callback.clone(), vec![value.clone()], None),
                )
            })
            .collect::<Vec<_>>();

        Ok(self.attach_obligations(
            Value::Map(MapValue {
                entries: Arc::from(entries),
            }),
            obligations,
        ))
    }

    fn exec_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        let kwargs = self.materialize_kwargs(kwargs, kwargs_expr, "exec")?;
        let request = self.parse_exec_request(args, &kwargs)?;
        if let Some(exec_handler) = &self.exec_handler {
            let request = self.ship_exec_request(&request)?;
            return exec_handler(request);
        }
        self.exec_builtin_local(request)
    }

    fn exec_builtin_scheduled(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        let kwargs = self.materialize_kwargs_scheduled(kwargs, kwargs_expr, "exec")?;
        let request = self.parse_exec_request_scheduled(args, &kwargs)?;
        if let Some(exec_handler) = &self.exec_handler {
            let request = self.ship_exec_request(&request)?;
            return exec_handler(request);
        }
        self.exec_builtin_local(request)
    }

    fn discover_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        if kwargs_expr.is_some() {
            return Err(self.type_error("discover does not accept keyword records"));
        }
        self.require_no_kwargs("discover", kwargs)?;
        self.check_arity("discover", args, 2)?;
        let class = self.force_value(args[0].clone())?;
        let class = self.expect_executor_class(&class, "discover class")?;
        let probe = self.force_value(args[1].clone())?;

        if let Some(discover_handler) = &self.discover_handler {
            let request = DiscoverRequest {
                class,
                demand_chain: self.current_demand_chain(None),
                module: self.transport_module(),
                probe: self.ship_function(probe)?,
            };
            return discover_handler(request);
        }

        self.invoke_discover_probe(probe)
    }

    fn discover_builtin_scheduled(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        if kwargs_expr.is_some() {
            return Err(self.type_error("discover does not accept keyword records"));
        }
        self.require_no_kwargs("discover", kwargs)?;
        self.check_arity("discover", args, 2)?;
        let class =
            self.materialize_scheduled_value_deep(args[0].clone(), "forcing discover class")?;
        let class = self.expect_executor_class(&class, "discover class")?;
        let probe =
            self.materialize_scheduled_value_deep(args[1].clone(), "forcing discover probe")?;

        if let Some(discover_handler) = &self.discover_handler {
            let request = DiscoverRequest {
                class,
                demand_chain: self.current_demand_chain(None),
                module: self.transport_module(),
                probe: self.ship_function(probe)?,
            };
            return discover_handler(request);
        }

        self.invoke_discover_probe(probe)
    }

    fn executor_class_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        if kwargs_expr.is_some() {
            return Err(self.type_error("executor_class does not accept keyword records"));
        }
        self.require_no_kwargs("executor_class", kwargs)?;
        self.check_arity("executor_class", args, 1)?;
        let name = self.force_value(args[0].clone())?;
        let name = self.expect_host_path(&name, "executor_class name")?;
        Ok(Value::ExecutorClass(Arc::from(name)))
    }

    fn executor_class_builtin_scheduled(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        if kwargs_expr.is_some() {
            return Err(self.type_error("executor_class does not accept keyword records"));
        }
        self.require_no_kwargs("executor_class", kwargs)?;
        self.check_arity("executor_class", args, 1)?;
        let name =
            self.materialize_scheduled_value_deep(args[0].clone(), "forcing executor_class name")?;
        let name = self.expect_host_path(&name, "executor_class name")?;
        Ok(Value::ExecutorClass(Arc::from(name)))
    }

    fn materialize_kwargs(
        &self,
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
        context: &str,
    ) -> Result<Vec<(Arc<str>, Value)>, PoisonError> {
        let mut out = kwargs.to_vec();
        let Some(kwargs_expr) = kwargs_expr else {
            return Ok(out);
        };

        let kwargs_expr = self.force_value_shallow(kwargs_expr)?;
        let (value, obligations) = self.split_obligations(kwargs_expr);
        let Value::Map(map) = value else {
            return Err(self.type_error(&format!(
                "{context} keyword record must be a record/map, got {}",
                value.type_name()
            )));
        };

        for (key, value) in map.entries.iter() {
            let key = self.force_value(key.clone())?;
            let Value::String(name) = key else {
                return Err(self.type_error(&format!(
                    "{context} keyword record keys must be String, got {}",
                    key.type_name()
                )));
            };
            out.push((
                Arc::from(name.as_ref()),
                self.attach_obligations(value.clone(), obligations.iter().cloned()),
            ));
        }

        Ok(out)
    }

    fn materialize_kwargs_scheduled(
        &self,
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
        context: &str,
    ) -> Result<Vec<(Arc<str>, Value)>, PoisonError> {
        let mut out = kwargs.to_vec();
        let Some(kwargs_expr) = kwargs_expr else {
            return Ok(out);
        };

        let kwargs_expr = self.materialize_scheduled_value_shallow(
            kwargs_expr,
            &format!("forcing {context} keyword record"),
        )?;
        let (value, obligations) = self.split_obligations(kwargs_expr);
        let Value::Map(map) = value else {
            return Err(self.type_error(&format!(
                "{context} keyword record must be a record/map, got {}",
                value.type_name()
            )));
        };

        for (key, value) in map.entries.iter() {
            let key = self.materialize_scheduled_value_deep(
                key.clone(),
                &format!("forcing {context} keyword key"),
            )?;
            let Value::String(name) = key else {
                return Err(self.type_error(&format!(
                    "{context} keyword record keys must be String, got {}",
                    key.type_name()
                )));
            };
            out.push((
                Arc::from(name.as_ref()),
                self.attach_obligations(value.clone(), obligations.iter().cloned()),
            ));
        }

        Ok(out)
    }

    fn merge_keyword_records(&self, left: Value, right: Value) -> Result<Value, PoisonError> {
        let mut entries = Vec::new();
        let left = self.force_value_shallow(left)?;
        self.extend_record_with_spread(&mut entries, left)?;
        let right = self.force_value_shallow(right)?;
        self.extend_record_with_spread(&mut entries, right)?;
        Ok(Value::Map(MapValue {
            entries: Arc::from(entries),
        }))
    }

    fn merge_keyword_records_scheduled(
        &self,
        left: Value,
        right: Value,
    ) -> Result<Value, PoisonError> {
        let mut entries = Vec::new();
        let left =
            self.materialize_scheduled_value_shallow(left, "forcing keyword record spread")?;
        self.extend_record_with_spread(&mut entries, left)?;
        let right =
            self.materialize_scheduled_value_shallow(right, "forcing keyword record spread")?;
        self.extend_record_with_spread(&mut entries, right)?;
        Ok(Value::Map(MapValue {
            entries: Arc::from(entries),
        }))
    }

    fn parse_exec_request(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<ParsedExecRequest, PoisonError> {
        self.check_arity("exec", args, 1)?;

        let mut env_value = None;
        let mut mounts_value = None;
        let mut observer_value = None;
        let mut executor_value = None;
        let mut writable_value = None;

        for (name, value) in kwargs {
            match name.as_ref() {
                "env" => env_value = Some(value.clone()),
                "mounts" => mounts_value = Some(value.clone()),
                "observer" => observer_value = Some(value.clone()),
                "executor" => executor_value = Some(value.clone()),
                "writable" => writable_value = Some(value.clone()),
                other => {
                    return Err(self
                        .type_error(&format!("exec does not accept keyword argument '{other}'")));
                }
            }
        }

        let args_value = self.force_value_shallow(args[0].clone())?;
        let (args_value, top_level_arg_obligations) = self.split_obligations(args_value);
        let Value::Array(exec_args_raw) = args_value else {
            return Err(self.type_error(&format!(
                "exec expects Array[String | Path | Blob | Tree] for args, got {}",
                args_value.type_name()
            )));
        };

        let observer =
            observer_value.ok_or_else(|| self.type_error("exec requires an observer function"))?;
        let observer = self.force_value(observer)?;
        match self.split_obligations_ref(&observer).0 {
            Value::Function(_) | Value::Builtin(_) => {}
            other => {
                return Err(self.type_error(&format!(
                    "exec observer must be callable, got {}",
                    other.type_name()
                )));
            }
        }

        if let Some(executor) = executor_value {
            let executor = self.force_value(executor)?;
            let _ = self.expect_executor_class(&executor, "exec executor")?;
        }

        let mut verification_obligations = BTreeMap::new();
        Self::add_obligations(&mut verification_obligations, top_level_arg_obligations);
        self.collect_value_obligations(
            &Value::Array(exec_args_raw.clone()),
            &mut verification_obligations,
        );
        if let Some(env_value) = &env_value {
            self.collect_value_obligations(env_value, &mut verification_obligations);
        }
        if let Some(mounts_value) = &mounts_value {
            self.collect_value_obligations(mounts_value, &mut verification_obligations);
        }
        if let Some(writable_value) = &writable_value {
            self.collect_value_obligations(writable_value, &mut verification_obligations);
        }
        self.collect_value_obligations(&observer, &mut verification_obligations);

        let mut mounts = self.exec_mounts_from_value(mounts_value)?;
        let explicit_writable = self.exec_writable_from_value(writable_value)?;
        let env = self.exec_env_from_value(env_value, &mut mounts, &explicit_writable)?;
        let exec_args = exec_args_raw
            .iter()
            .map(|value| self.parse_exec_arg(value, &mut mounts, &explicit_writable, "exec arg"))
            .collect::<Result<Vec<_>, _>>()?;
        if exec_args.is_empty() {
            return Err(self.type_error("exec requires at least one program argument"));
        }
        let writable_roots = if explicit_writable.is_empty() {
            self.infer_exec_writable_roots(&mounts, &exec_args, &env)
        } else {
            explicit_writable
        };

        Ok(ParsedExecRequest {
            args: exec_args,
            env,
            mounts,
            writable_roots,
            observer,
            verification_obligations: verification_obligations.into_values().collect(),
        })
    }

    fn parse_exec_request_scheduled(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<ParsedExecRequest, PoisonError> {
        self.check_arity("exec", args, 1)?;

        let mut env_value = None;
        let mut mounts_value = None;
        let mut observer_value = None;
        let mut executor_value = None;
        let mut writable_value = None;

        for (name, value) in kwargs {
            match name.as_ref() {
                "env" => env_value = Some(value.clone()),
                "mounts" => mounts_value = Some(value.clone()),
                "observer" => observer_value = Some(value.clone()),
                "executor" => executor_value = Some(value.clone()),
                "writable" => writable_value = Some(value.clone()),
                other => {
                    return Err(self
                        .type_error(&format!("exec does not accept keyword argument '{other}'")));
                }
            }
        }

        let args_value =
            self.materialize_scheduled_value_shallow(args[0].clone(), "forcing exec args")?;
        let (args_value, top_level_arg_obligations) = self.split_obligations(args_value);
        let Value::Array(exec_args_raw) = args_value else {
            return Err(self.type_error(&format!(
                "exec expects Array[String | Path | Blob | Tree] for args, got {}",
                args_value.type_name()
            )));
        };

        let observer =
            observer_value.ok_or_else(|| self.type_error("exec requires an observer function"))?;
        let observer = self.materialize_scheduled_value_deep(observer, "forcing exec observer")?;
        match self.split_obligations_ref(&observer).0 {
            Value::Function(_) | Value::Builtin(_) => {}
            other => {
                return Err(self.type_error(&format!(
                    "exec observer must be callable, got {}",
                    other.type_name()
                )));
            }
        }

        if let Some(executor) = executor_value {
            let executor =
                self.materialize_scheduled_value_deep(executor, "forcing exec executor")?;
            let _ = self.expect_executor_class(&executor, "exec executor")?;
        }

        let mut verification_obligations = BTreeMap::new();
        Self::add_obligations(&mut verification_obligations, top_level_arg_obligations);
        self.collect_value_obligations(
            &Value::Array(exec_args_raw.clone()),
            &mut verification_obligations,
        );
        if let Some(env_value) = &env_value {
            self.collect_value_obligations(env_value, &mut verification_obligations);
        }
        if let Some(mounts_value) = &mounts_value {
            self.collect_value_obligations(mounts_value, &mut verification_obligations);
        }
        if let Some(writable_value) = &writable_value {
            self.collect_value_obligations(writable_value, &mut verification_obligations);
        }
        self.collect_value_obligations(&observer, &mut verification_obligations);

        let mut mounts = self.exec_mounts_from_value_scheduled(mounts_value)?;
        let explicit_writable = self.exec_writable_from_value_scheduled(writable_value)?;
        let env = self.exec_env_from_value_scheduled(env_value, &mut mounts, &explicit_writable)?;
        let exec_args = exec_args_raw
            .iter()
            .map(|value| {
                self.parse_exec_arg_scheduled(value, &mut mounts, &explicit_writable, "exec arg")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if exec_args.is_empty() {
            return Err(self.type_error("exec requires at least one program argument"));
        }
        let writable_roots = if explicit_writable.is_empty() {
            self.infer_exec_writable_roots(&mounts, &exec_args, &env)
        } else {
            explicit_writable
        };

        Ok(ParsedExecRequest {
            args: exec_args,
            env,
            mounts,
            writable_roots,
            observer,
            verification_obligations: verification_obligations.into_values().collect(),
        })
    }

    fn ship_exec_request(&self, request: &ParsedExecRequest) -> Result<ExecRequest, PoisonError> {
        Ok(ExecRequest {
            args: request.args.clone(),
            env: request.env.clone(),
            mounts: request
                .mounts
                .iter()
                .map(|(path, value)| {
                    Ok(ExecMount {
                        path: path.clone(),
                        value: self.snapshot_value(value.clone())?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            writable_roots: request.writable_roots.clone(),
            demand_chain: self.current_demand_chain(None),
            module: self.transport_module(),
            observer: self.ship_function(request.observer.clone())?,
            verification_obligations: request.verification_obligations.clone(),
        })
    }

    fn exec_builtin_local(&self, request: ParsedExecRequest) -> Result<Value, PoisonError> {
        let sandbox = TempDir::new().map_err(|error| {
            self.internal_error(&format!("failed to create exec sandbox: {error}"))
        })?;
        let sandbox_root = PathValue::new(sandbox.path().to_string_lossy());

        for (mount_path, value) in &request.mounts {
            self.materialize_mount_value(&sandbox_root, mount_path, value)?;
        }
        for root in &request.writable_roots {
            self.ensure_dir(&sandbox_root.join(root.as_str()))?;
        }

        let verification_task = if request.verification_obligations.is_empty() {
            None
        } else {
            let verification_obligations = request.verification_obligations.clone();
            Some(thread::spawn(move || {
                verify_host_obligations(&verification_obligations)
            }))
        };

        let rendered_args = request
            .args
            .iter()
            .map(|value| self.render_exec_arg(value, &sandbox_root, "exec arg"))
            .collect::<Result<Vec<_>, _>>()?;

        let mut command = Command::new(&rendered_args[0]);
        command.args(&rendered_args[1..]);
        command.current_dir(sandbox.path());
        for (key, value) in &request.env {
            command.env(
                key,
                self.render_exec_arg(value, &sandbox_root, "exec env value")?,
            );
        }

        let output = command.output().map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to execute {}: {error}", rendered_args[0]),
                self.current_demand_chain(None),
            )
        })?;

        let process = self.exec_process_value(&output);
        let tree = Value::Tree(TreeValue::FsDir(sandbox_root.clone()));
        let observed = self.execute_call(
            request.observer,
            Arc::from(vec![process, tree]),
            Arc::from([]),
            None,
        );

        if let Some(verification_task) = verification_task {
            match verification_task.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(PoisonError::new(
                        PoisonKind::VerificationFailure,
                        format!("exec verification failed: {error}"),
                        self.current_demand_chain(None),
                    ));
                }
                Err(_) => {
                    return Err(PoisonError::new(
                        PoisonKind::Internal,
                        "exec verification task panicked",
                        self.current_demand_chain(None),
                    ));
                }
            }
        }

        let observed = observed?;
        self.snapshot_value(observed)
    }

    fn exec_mounts_from_value(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<(PathValue, Value)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(self.force_value_shallow(value)?);
        let Value::Map(map) = value else {
            return Err(self.type_error(&format!(
                "exec mounts must be Map, got {}",
                value.type_name()
            )));
        };

        let mut mounts = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self.force_value(key.clone())?;
            let path = self.expect_relative_path(&key, "exec mount path")?;
            let (value, _) = self.split_obligations(self.force_value_shallow(value.clone())?);
            match value {
                Value::Blob(_) | Value::Tree(_) => mounts.push((path, value)),
                other => {
                    return Err(self.type_error(&format!(
                        "exec mount values must be Blob or Tree, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        Ok(mounts)
    }

    fn exec_mounts_from_value_scheduled(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<(PathValue, Value)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(
            self.materialize_scheduled_value_shallow(value, "forcing exec mounts")?,
        );
        let Value::Map(map) = value else {
            return Err(self.type_error(&format!(
                "exec mounts must be Map, got {}",
                value.type_name()
            )));
        };

        let mut mounts = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key =
                self.materialize_scheduled_value_deep(key.clone(), "forcing exec mount path")?;
            let path = self.expect_relative_path(&key, "exec mount path")?;
            let (value, _) =
                self.split_obligations(self.materialize_scheduled_value_shallow(
                    value.clone(),
                    "forcing exec mount value",
                )?);
            match value {
                Value::Blob(_) | Value::Tree(_) => mounts.push((path, value)),
                other => {
                    return Err(self.type_error(&format!(
                        "exec mount values must be Blob or Tree, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        Ok(mounts)
    }

    fn exec_writable_from_value(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<PathValue>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(self.force_value_shallow(value)?);
        let Value::Array(items) = value else {
            return Err(self.type_error(&format!(
                "exec writable must be Array[Path], got {}",
                value.type_name()
            )));
        };

        let mut writable = Vec::with_capacity(items.len());
        for item in items.iter() {
            let item = self.force_value(item.clone())?;
            writable.push(self.expect_relative_path(&item, "exec writable path")?);
        }
        Ok(writable)
    }

    fn exec_writable_from_value_scheduled(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<PathValue>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(
            self.materialize_scheduled_value_shallow(value, "forcing exec writable")?,
        );
        let Value::Array(items) = value else {
            return Err(self.type_error(&format!(
                "exec writable must be Array[Path], got {}",
                value.type_name()
            )));
        };

        let mut writable = Vec::with_capacity(items.len());
        for item in items.iter() {
            let item =
                self.materialize_scheduled_value_deep(item.clone(), "forcing exec writable path")?;
            writable.push(self.expect_relative_path(&item, "exec writable path")?);
        }
        Ok(writable)
    }

    fn infer_exec_writable_roots(
        &self,
        _mounts: &[(PathValue, Value)],
        args: &[ExecArgValue],
        env: &[(String, ExecArgValue)],
    ) -> Vec<PathValue> {
        let mut roots = BTreeSet::new();

        let mut add_parent_paths = |path: &PathValue| {
            let mut current = path.as_str();
            while let Some((parent, _)) = current.rsplit_once('/') {
                if parent.is_empty() {
                    break;
                }
                roots.insert(parent.to_string());
                current = parent;
            }
        };

        for arg in args {
            if let ExecArgValue::Path(path) = arg {
                add_parent_paths(path);
            }
        }

        for (_, value) in env {
            if let ExecArgValue::Path(path) = value {
                add_parent_paths(path);
            }
        }

        roots.into_iter().map(PathValue::new).collect()
    }

    fn exec_env_from_value(
        &self,
        value: Option<Value>,
        mounts: &mut Vec<(PathValue, Value)>,
        writable_roots: &[PathValue],
    ) -> Result<Vec<(String, ExecArgValue)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(self.force_value_shallow(value)?);
        let Value::Map(map) = value else {
            return Err(
                self.type_error(&format!("exec env must be Map, got {}", value.type_name()))
            );
        };

        let mut env = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self.force_value(key.clone())?;
            let key = match key {
                Value::String(key) => key.to_string(),
                other => {
                    return Err(self.type_error(&format!(
                        "exec env keys must be String, got {}",
                        other.type_name()
                    )));
                }
            };
            let value = self.parse_exec_arg(value, mounts, writable_roots, "exec env value")?;
            env.push((key, value));
        }
        Ok(env)
    }

    fn exec_env_from_value_scheduled(
        &self,
        value: Option<Value>,
        mounts: &mut Vec<(PathValue, Value)>,
        writable_roots: &[PathValue],
    ) -> Result<Vec<(String, ExecArgValue)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(
            self.materialize_scheduled_value_shallow(value, "forcing exec env")?,
        );
        let Value::Map(map) = value else {
            return Err(
                self.type_error(&format!("exec env must be Map, got {}", value.type_name()))
            );
        };

        let mut env = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self.materialize_scheduled_value_deep(key.clone(), "forcing exec env key")?;
            let key = match key {
                Value::String(key) => key.to_string(),
                other => {
                    return Err(self.type_error(&format!(
                        "exec env keys must be String, got {}",
                        other.type_name()
                    )));
                }
            };
            let value =
                self.parse_exec_arg_scheduled(value, mounts, writable_roots, "exec env value")?;
            env.push((key, value));
        }
        Ok(env)
    }

    fn parse_exec_arg(
        &self,
        value: &Value,
        mounts: &mut Vec<(PathValue, Value)>,
        _writable_roots: &[PathValue],
        context: &str,
    ) -> Result<ExecArgValue, PoisonError> {
        let (value, _) = self.split_obligations(self.force_value(value.clone())?);
        match value {
            Value::String(text) => Ok(ExecArgValue::Literal(text.to_string())),
            Value::Path(path) => {
                let path = self.expect_relative_path(&Value::Path(path), context)?;
                Ok(ExecArgValue::Path(path))
            }
            Value::ExecPath(path) => self.exec_arg_from_exec_path(mounts, &path),
            Value::Blob(blob) => Ok(ExecArgValue::Path(
                self.ensure_blob_exec_mount(mounts, &blob),
            )),
            Value::Tree(tree) => Ok(ExecArgValue::Path(
                self.ensure_tree_exec_mount(mounts, &tree),
            )),
            other => {
                if let Some(text) = self.try_stringify_exec_arg(&other)? {
                    return Ok(ExecArgValue::Literal(text));
                }
                Err(self.type_error(&format!(
                    "{context} must be String, Path, Blob, Tree, ExecPath, or a value with to_string(), got {}",
                    other.type_name()
                )))
            }
        }
    }

    fn parse_exec_arg_scheduled(
        &self,
        value: &Value,
        mounts: &mut Vec<(PathValue, Value)>,
        _writable_roots: &[PathValue],
        context: &str,
    ) -> Result<ExecArgValue, PoisonError> {
        let (value, _) = self.split_obligations(
            self.materialize_scheduled_value_deep(value.clone(), &format!("forcing {context}"))?,
        );
        match value {
            Value::String(text) => Ok(ExecArgValue::Literal(text.to_string())),
            Value::Path(path) => {
                let path = self.expect_relative_path(&Value::Path(path), context)?;
                Ok(ExecArgValue::Path(path))
            }
            Value::ExecPath(path) => self.exec_arg_from_exec_path(mounts, &path),
            Value::Blob(blob) => Ok(ExecArgValue::Path(
                self.ensure_blob_exec_mount(mounts, &blob),
            )),
            Value::Tree(tree) => Ok(ExecArgValue::Path(
                self.ensure_tree_exec_mount(mounts, &tree),
            )),
            other => {
                if let Some(text) = self.try_stringify_exec_arg(&other)? {
                    return Ok(ExecArgValue::Literal(text));
                }
                Err(self.type_error(&format!(
                    "{context} must be String, Path, Blob, Tree, ExecPath, or a value with to_string(), got {}",
                    other.type_name()
                )))
            }
        }
    }

    fn try_stringify_exec_arg(&self, value: &Value) -> Result<Option<String>, PoisonError> {
        let Some(method) = self.select_field(value, "to_string").ok() else {
            return Ok(None);
        };
        let result = self.execute_call(method, Arc::from([]), Arc::from([]), None)?;
        let (result, _) = self.split_obligations(self.force_value(result)?);
        match result {
            Value::String(text) => Ok(Some(text.to_string())),
            _ => Ok(None),
        }
    }

    fn extend_array_with_spread(
        &self,
        values: &mut Vec<Value>,
        spread_value: Value,
    ) -> Result<(), PoisonError> {
        let (spread_value, obligations) = self.split_obligations(spread_value);
        match spread_value {
            Value::Array(items) => {
                values.extend(
                    items
                        .iter()
                        .cloned()
                        .map(|value| self.attach_obligations(value, obligations.clone())),
                );
                Ok(())
            }
            Value::Tree(tree) => {
                values.extend(
                    self.tree_exec_path_values(&tree)?
                        .into_iter()
                        .map(|value| self.attach_obligations(value, obligations.clone())),
                );
                Ok(())
            }
            other => Err(self.type_error(&format!(
                "array spread requires Array or Tree, got {}",
                other.type_name()
            ))),
        }
    }

    fn extend_record_with_spread(
        &self,
        entries: &mut Vec<(Value, Value)>,
        spread_value: Value,
    ) -> Result<(), PoisonError> {
        let (spread_value, obligations) = self.split_obligations(spread_value);
        match spread_value {
            Value::Map(map) => {
                entries.extend(map.entries.iter().cloned().map(|(key, value)| {
                    (
                        self.attach_obligations(key, obligations.clone()),
                        self.attach_obligations(value, obligations.clone()),
                    )
                }));
                Ok(())
            }
            Value::Struct(struct_value) => {
                entries.extend(struct_value.fields.iter().cloned().map(|(name, value)| {
                    (
                        self.attach_obligations(Value::String(name), obligations.clone()),
                        self.attach_obligations(value, obligations.clone()),
                    )
                }));
                Ok(())
            }
            other => Err(self.type_error(&format!(
                "record spread requires Map or Struct, got {}",
                other.type_name()
            ))),
        }
    }

    fn bind_pattern_into(
        &self,
        pattern: &instr::Pattern,
        value: Value,
        bindings: &mut HashMap<String, Value>,
    ) -> Result<(), PoisonError> {
        match pattern {
            instr::Pattern::Binding(name) => {
                bindings.insert(name.clone(), value);
                Ok(())
            }
            instr::Pattern::Struct(fields) => {
                let (value, obligations) = self.split_obligations(self.force_value_shallow(value)?);
                match value {
                    Value::Map(map) => {
                        for (name, nested) in fields {
                            let field_value = map
                                .entries
                                .iter()
                                .find_map(|(key, value)| match key {
                                    Value::String(key_name) if key_name.as_ref() == name => {
                                        Some(value.clone())
                                    }
                                    _ => None,
                                })
                                .ok_or_else(|| {
                                    self.type_error(&format!(
                                        "record pattern requires field '{name}'"
                                    ))
                                })?;
                            let field_value =
                                self.attach_obligations(field_value, obligations.iter().cloned());
                            match nested {
                                Some(nested) => {
                                    self.bind_pattern_into(nested, field_value, bindings)?
                                }
                                None => {
                                    bindings.insert(name.clone(), field_value);
                                }
                            }
                        }
                        Ok(())
                    }
                    Value::Struct(struct_value) => {
                        for (name, nested) in fields {
                            let field_value = struct_value
                                .fields
                                .iter()
                                .find(|(field_name, _)| field_name.as_ref() == name)
                                .map(|(_, value)| value.clone())
                                .ok_or_else(|| {
                                    self.type_error(&format!(
                                        "struct pattern requires field '{name}'"
                                    ))
                                })?;
                            let field_value =
                                self.attach_obligations(field_value, obligations.iter().cloned());
                            match nested {
                                Some(nested) => {
                                    self.bind_pattern_into(nested, field_value, bindings)?
                                }
                                None => {
                                    bindings.insert(name.clone(), field_value);
                                }
                            }
                        }
                        Ok(())
                    }
                    other => Err(self.type_error(&format!(
                        "struct pattern requires Map or Struct, got {}",
                        other.type_name()
                    ))),
                }
            }
        }
    }

    fn exec_arg_from_exec_path(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        path: &ExecPathValue,
    ) -> Result<ExecArgValue, PoisonError> {
        match path {
            ExecPathValue::InTree { tree, suffix } => Ok(ExecArgValue::Path(
                self.ensure_tree_exec_mount(mounts, tree)
                    .join(suffix.as_str()),
            )),
            ExecPathValue::InBlob { blob, leaf } => Ok(ExecArgValue::Path(self.ensure_exec_mount(
                mounts,
                Value::Blob(blob.clone()),
                Some(leaf.as_str().to_string()),
            ))),
        }
    }

    fn tree_exec_path_values(&self, tree: &TreeValue) -> Result<Vec<Value>, PoisonError> {
        let suffixes = self.tree_leaf_suffixes(tree)?;
        Ok(suffixes
            .into_iter()
            .map(|suffix| {
                Value::ExecPath(ExecPathValue::InTree {
                    tree: tree.clone(),
                    suffix,
                })
            })
            .collect())
    }

    fn tree_leaf_suffixes(&self, tree: &TreeValue) -> Result<Vec<PathValue>, PoisonError> {
        let mut suffixes = Vec::new();
        self.collect_tree_leaf_suffixes(tree, &PathValue::new(""), &mut suffixes)?;
        Ok(suffixes)
    }

    fn collect_tree_leaf_suffixes(
        &self,
        tree: &TreeValue,
        prefix: &PathValue,
        suffixes: &mut Vec<PathValue>,
    ) -> Result<(), PoisonError> {
        match tree {
            TreeValue::Inline(entries) => {
                let mut sorted = entries.iter().collect::<Vec<_>>();
                sorted.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
                for (name, value) in sorted {
                    let child_path = prefix.join(name.as_ref());
                    let value = self.force_value_shallow(value.clone())?;
                    match value {
                        Value::Blob(_) => suffixes.push(child_path),
                        Value::Tree(tree) => {
                            self.collect_tree_leaf_suffixes(&tree, &child_path, suffixes)?
                        }
                        other => {
                            return Err(self.type_error(&format!(
                                "tree spread entries must be Blob or Tree, got {}",
                                other.type_name()
                            )));
                        }
                    }
                }
                Ok(())
            }
            TreeValue::FsDir(root) => {
                let mut names = Vec::new();
                for entry in fs::read_dir(root.as_str()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to list {} for tree spread: {error}",
                        root.as_str()
                    ))
                })? {
                    let entry = entry.map_err(|error| {
                        self.internal_error(&format!(
                            "failed to read {} for tree spread: {error}",
                            root.as_str()
                        ))
                    })?;
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                names.sort();

                for name in names {
                    let child_root = root.join(&name);
                    let child_path = prefix.join(&name);
                    let metadata = fs::metadata(child_root.as_str()).map_err(|error| {
                        self.internal_error(&format!(
                            "failed to stat {} for tree spread: {error}",
                            child_root.as_str()
                        ))
                    })?;
                    if metadata.is_dir() {
                        self.collect_tree_leaf_suffixes(
                            &TreeValue::FsDir(child_root),
                            &child_path,
                            suffixes,
                        )?;
                    } else {
                        suffixes.push(child_path);
                    }
                }
                Ok(())
            }
            TreeValue::Store(root) => Err(self.internal_error(&format!(
                "cannot spread store-backed tree {root} without an external store resolver"
            ))),
        }
    }

    // r[impl exec.arg.auto-mount]
    fn ensure_tree_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        tree: &TreeValue,
    ) -> PathValue {
        let value = Value::Tree(tree.clone());
        self.ensure_exec_mount(mounts, value, None)
    }

    // r[impl exec.arg.auto-mount]
    fn ensure_blob_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        blob: &BlobValue,
    ) -> PathValue {
        let value = Value::Blob(blob.clone());
        let leaf = self.exec_blob_leaf_name(blob);
        self.ensure_exec_mount(mounts, value, Some(leaf))
    }

    // r[impl exec.mount]
    fn ensure_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        value: Value,
        leaf: Option<String>,
    ) -> PathValue {
        if let Some((path, _)) = mounts.iter().find(|(_, mounted)| *mounted == value) {
            return path.clone();
        }

        let mount_root = self.next_exec_mount_root(mounts);
        let mount_path = match leaf {
            Some(leaf) => mount_root.join(&leaf),
            None => mount_root,
        };
        mounts.push((mount_path.clone(), value));
        mount_path
    }

    fn next_exec_mount_root(&self, mounts: &[(PathValue, Value)]) -> PathValue {
        for index in 0usize.. {
            let candidate = PathValue::new(format!("{IMPLICIT_EXEC_MOUNT_PREFIX}/{index}"));
            if mounts
                .iter()
                .all(|(path, _)| !self.exec_paths_overlap(&candidate, path))
            {
                return candidate;
            }
        }
        unreachable!("implicit exec mount path space is infinite")
    }

    fn exec_paths_overlap(&self, left: &PathValue, right: &PathValue) -> bool {
        if left == right {
            return true;
        }
        let left = left.as_str();
        let right = right.as_str();
        right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
            || left
                .strip_prefix(right)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    fn exec_blob_leaf_name(&self, blob: &BlobValue) -> String {
        match blob {
            BlobValue::FsFile { path, .. } => Path::new(path.as_str())
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| self.synthetic_blob_leaf(blob)),
            BlobValue::Inline { .. } | BlobValue::Store { .. } => self.synthetic_blob_leaf(blob),
        }
    }

    fn synthetic_blob_leaf(&self, blob: &BlobValue) -> String {
        let digest = match blob {
            BlobValue::Inline { bytes, executable } => {
                let mut keyed = Vec::with_capacity(bytes.len() + 1);
                keyed.push(u8::from(*executable));
                keyed.extend_from_slice(bytes);
                blake3::hash(&keyed).to_hex().to_string()
            }
            BlobValue::FsFile { path, executable } => {
                blake3::hash(format!("{executable}:{}", path.as_str()).as_bytes())
                    .to_hex()
                    .to_string()
            }
            BlobValue::Store {
                blob_ref,
                executable,
            } => blake3::hash(format!("{executable}:{blob_ref:?}").as_bytes())
                .to_hex()
                .to_string(),
        };
        format!("blob-{}", &digest[..12])
    }

    fn render_exec_arg(
        &self,
        value: &ExecArgValue,
        sandbox_root: &PathValue,
        _context: &str,
    ) -> Result<String, PoisonError> {
        match value {
            ExecArgValue::Literal(text) => Ok(text.clone()),
            ExecArgValue::Path(path) => Ok(sandbox_root.join(path.as_str()).as_str().to_string()),
        }
    }

    fn materialize_mount_value(
        &self,
        sandbox_root: &PathValue,
        mount_path: &PathValue,
        value: &Value,
    ) -> Result<(), PoisonError> {
        let destination = sandbox_root.join(mount_path.as_str());
        self.materialize_value_to_fs(value, &destination)
    }

    fn materialize_value_to_fs(
        &self,
        value: &Value,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        let value = self.force_value_shallow(value.clone())?;
        match value {
            Value::Blob(blob) => self.write_blob_to_path(&blob, destination),
            Value::Tree(tree) => self.write_tree_to_path(&tree, destination),
            Value::ExecPath(_) => {
                Err(self.type_error("cannot materialize ExecPath into exec sandbox"))
            }
            other => Err(self.type_error(&format!(
                "cannot materialize {} into exec sandbox",
                other.type_name()
            ))),
        }
    }

    fn write_blob_to_path(
        &self,
        blob: &BlobValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        if let Some(parent) = Path::new(destination.as_str()).parent() {
            fs::create_dir_all(parent).map_err(|error| {
                self.internal_error(&format!("failed to create {}: {error}", parent.display()))
            })?;
        }

        match blob {
            BlobValue::Inline { bytes, executable } => {
                fs::write(destination.as_str(), bytes.as_ref()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to write {}: {error}",
                        destination.as_str()
                    ))
                })?;
                set_executable_if_needed(destination.as_str(), *executable).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to set executable bit on {}: {error}",
                        destination.as_str()
                    ))
                })
            }
            BlobValue::FsFile { path, executable } => {
                fs::copy(path.as_str(), destination.as_str()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to copy {} to {}: {error}",
                        path.as_str(),
                        destination.as_str()
                    ))
                })?;
                set_executable_if_needed(destination.as_str(), *executable).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to set executable bit on {}: {error}",
                        destination.as_str()
                    ))
                })
            }
            BlobValue::Store { blob_ref, .. } => Err(self.internal_error(&format!(
                "cannot materialize store-backed blob {:?} without an external store resolver",
                blob_ref
            ))),
        }
    }

    fn write_tree_to_path(
        &self,
        tree: &TreeValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        match tree {
            TreeValue::Inline(entries) => {
                self.ensure_dir(destination)?;
                for (name, value) in entries.iter() {
                    self.materialize_value_to_fs(value, &destination.join(name.as_ref()))?;
                }
                Ok(())
            }
            TreeValue::FsDir(path) => self.copy_dir_recursive(path, destination),
            TreeValue::Store(root) => Err(self.internal_error(&format!(
                "cannot materialize store-backed tree {root} without an external store resolver"
            ))),
        }
    }

    fn copy_dir_recursive(
        &self,
        source: &PathValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        self.ensure_dir(destination)?;
        let mut entries = fs::read_dir(source.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", source.as_str()))
        })?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next().transpose().map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", source.as_str()))
        })? {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();

        for name in names {
            let source_path = source.join(&name);
            let destination_path = destination.join(&name);
            let metadata = fs::metadata(source_path.as_str()).map_err(|error| {
                self.internal_error(&format!("failed to stat {}: {error}", source_path.as_str()))
            })?;
            if metadata.is_dir() {
                self.copy_dir_recursive(&source_path, &destination_path)?;
            } else {
                self.write_blob_to_path(
                    &BlobValue::FsFile {
                        path: source_path.clone(),
                        executable: host_path_is_executable(Path::new(source_path.as_str())),
                    },
                    &destination_path,
                )?;
            }
        }
        Ok(())
    }

    fn ensure_dir(&self, path: &PathValue) -> Result<(), PoisonError> {
        fs::create_dir_all(path.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to create {}: {error}", path.as_str()))
        })
    }

    fn expect_host_path(&self, value: &Value, context: &str) -> Result<String, PoisonError> {
        let (value, _) = self.split_obligations_ref(value);
        match value {
            Value::String(path) => Ok(path.to_string()),
            other => Err(self.type_error(&format!(
                "{context} must be String, got {}",
                other.type_name()
            ))),
        }
    }

    fn expect_executor_class(&self, value: &Value, context: &str) -> Result<String, PoisonError> {
        let (value, _) = self.split_obligations_ref(value);
        match value {
            Value::ExecutorClass(label) => Ok(label.to_string()),
            Value::String(label) => Ok(label.to_string()),
            other => Err(self.type_error(&format!(
                "{context} must be ExecutorClass, got {}",
                other.type_name()
            ))),
        }
    }

    fn host_read(&self, path: &str) -> Result<Value, PoisonError> {
        let metadata = fs::metadata(path).map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to read host path {path}: {error}"),
                self.current_demand_chain(None),
            )
        })?;
        if metadata.is_dir() {
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("host.read expects a file, got directory {path}"),
                self.current_demand_chain(None),
            ));
        }
        let content_hash = fs::read(path)
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
            .map_err(|error| {
                PoisonError::new(
                    PoisonKind::UserFail,
                    format!("failed to read host path {path}: {error}"),
                    self.current_demand_chain(None),
                )
            })?;
        self.record_discovery_observation(HostObservation::Read {
            path: path.to_string(),
            content_hash,
        });
        Ok(Value::Blob(BlobValue::FsFile {
            path: PathValue::new(path),
            executable: host_path_is_executable(Path::new(path)),
        }))
    }

    fn host_hash(&self, path: &str) -> Result<Value, PoisonError> {
        let bytes = fs::read(path).map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to hash host path {path}: {error}"),
                self.current_demand_chain(None),
            )
        })?;
        let content_hash = blake3::hash(&bytes).to_hex().to_string();
        self.record_discovery_observation(HostObservation::Hash {
            path: path.to_string(),
            content_hash: content_hash.clone(),
        });
        Ok(Value::Blob(BlobValue::from_string(content_hash)))
    }

    fn host_which(&self, name: &str) -> Result<Value, PoisonError> {
        if host_lookup_candidate(name).is_some_and(host_path_is_executable) {
            self.record_discovery_observation(HostObservation::Which {
                name: name.to_string(),
                expected_path: Some(name.to_string()),
            });
            return Ok(Value::String(Arc::from(name)));
        }

        let Some(path) = env::var_os("PATH") else {
            self.record_discovery_observation(HostObservation::Which {
                name: name.to_string(),
                expected_path: None,
            });
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("host.which could not find {name}"),
                self.current_demand_chain(None),
            ));
        };
        for dir in env::split_paths(&path) {
            let candidate = dir.join(name);
            if host_path_is_executable(&candidate) {
                let candidate = candidate.to_string_lossy().to_string();
                self.record_discovery_observation(HostObservation::Which {
                    name: name.to_string(),
                    expected_path: Some(candidate.clone()),
                });
                return Ok(Value::String(Arc::from(candidate)));
            }
        }
        self.record_discovery_observation(HostObservation::Which {
            name: name.to_string(),
            expected_path: None,
        });
        Err(PoisonError::new(
            PoisonKind::UserFail,
            format!("host.which could not find {name}"),
            self.current_demand_chain(None),
        ))
    }

    fn exec_process_value(&self, output: &std::process::Output) -> Value {
        let exit_code = output.status.code().unwrap_or(1).max(0) as u64;
        Value::Struct(StructValue {
            type_name: Arc::from("Process"),
            fields: Arc::from([
                (Arc::from("exit_code"), Value::U64(exit_code)),
                (
                    Arc::from("stdout"),
                    Value::Bytes(Arc::from(output.stdout.clone())),
                ),
                (
                    Arc::from("stderr"),
                    Value::Bytes(Arc::from(output.stderr.clone())),
                ),
            ]),
        })
    }

    fn snapshot_value(&self, value: Value) -> Result<Value, PoisonError> {
        let value = self.force_value_shallow(value)?;
        match value {
            Value::Pending(node_id) => {
                self.snapshot_value(self.force_value(Value::Pending(node_id))?)
            }
            Value::Host(_) => {
                Err(self
                    .internal_error("Host values cannot cross the local exec observer boundary"))
            }
            Value::Discovered(discovered) => {
                let value = self.snapshot_value(*discovered.value)?;
                Ok(self.attach_obligations(value, discovered.obligations.iter().cloned()))
            }
            Value::ExecPath(_) => Err(self
                .internal_error("ExecPath values cannot cross the local exec observer boundary")),
            Value::Blob(BlobValue::Inline { .. })
            | Value::Blob(BlobValue::Store { .. })
            | Value::Bytes(_)
            | Value::Path(_)
            | Value::ExecutorClass(_)
            | Value::U64(_)
            | Value::I64(_)
            | Value::F64(_)
            | Value::Bool(_)
            | Value::String(_)
            | Value::Unit
            | Value::Function(_)
            | Value::Partial(_)
            | Value::Builtin(_) => Ok(value),
            Value::Blob(BlobValue::FsFile { path, executable }) => {
                let bytes = fs::read(path.as_str()).map_err(|error| {
                    self.internal_error(&format!("failed to read {}: {error}", path.as_str()))
                })?;
                Ok(Value::Blob(BlobValue::Inline {
                    bytes: Arc::from(bytes),
                    executable,
                }))
            }
            Value::Tree(TreeValue::Inline(entries)) => {
                let mut snapshotted = Vec::with_capacity(entries.len());
                for (name, value) in entries.iter() {
                    snapshotted.push((name.clone(), self.snapshot_value(value.clone())?));
                }
                Ok(Value::Tree(TreeValue::Inline(Arc::from(snapshotted))))
            }
            Value::Tree(TreeValue::FsDir(path)) => self.snapshot_fs_tree(&path),
            Value::Tree(TreeValue::Store(root)) => Ok(Value::Tree(TreeValue::Store(root))),
            Value::Tuple(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_value(value.clone())?);
                }
                Ok(Value::Tuple(Arc::from(snapshotted)))
            }
            Value::Array(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_value(value.clone())?);
                }
                Ok(Value::Array(Arc::from(snapshotted)))
            }
            Value::Map(map) => {
                let mut snapshotted = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    snapshotted.push((
                        self.snapshot_value(key.clone())?,
                        self.snapshot_value(value.clone())?,
                    ));
                }
                Ok(Value::Map(MapValue {
                    entries: Arc::from(snapshotted),
                }))
            }
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields.iter() {
                    fields.push((name.clone(), self.snapshot_value(value.clone())?));
                }
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
        }
    }

    fn snapshot_fs_tree(&self, root: &PathValue) -> Result<Value, PoisonError> {
        let mut read_dir = fs::read_dir(root.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", root.as_str()))
        })?;
        let mut names = Vec::new();
        while let Some(entry) = read_dir.next().transpose().map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", root.as_str()))
        })? {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();

        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let child = root.join(&name);
            let metadata = fs::metadata(child.as_str()).map_err(|error| {
                self.internal_error(&format!("failed to stat {}: {error}", child.as_str()))
            })?;
            let value = if metadata.is_dir() {
                self.snapshot_fs_tree(&child)?
            } else {
                let bytes = fs::read(child.as_str()).map_err(|error| {
                    self.internal_error(&format!("failed to read {}: {error}", child.as_str()))
                })?;
                Value::Blob(BlobValue::Inline {
                    bytes: Arc::from(bytes),
                    executable: host_path_is_executable(Path::new(child.as_str())),
                })
            };
            entries.push((Arc::from(name), value));
        }

        Ok(Value::Tree(TreeValue::Inline(Arc::from(entries))))
    }

    // r[impl store.tree.file]
    fn tree_file(&self, tree: &TreeValue, path: PathValue) -> Result<Value, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => self.tree_file_inline(entries, &path),
            TreeValue::FsDir(root) => {
                let full_path = root.join(path.as_str());
                let metadata = fs::metadata(full_path.as_str()).map_err(|error| {
                    PoisonError::new(
                        PoisonKind::UserFail,
                        format!("missing file {}: {error}", full_path.as_str()),
                        self.current_demand_chain(None),
                    )
                })?;
                if metadata.is_dir() {
                    return Err(PoisonError::new(
                        PoisonKind::UserFail,
                        format!("{} is a directory", full_path.as_str()),
                        self.current_demand_chain(None),
                    ));
                }
                Ok(Value::Blob(BlobValue::FsFile {
                    executable: host_path_is_executable(Path::new(full_path.as_str())),
                    path: full_path,
                }))
            }
            TreeValue::Store(root) => Err(self.internal_error(&format!(
                "cannot read store-backed tree {root} without an external store resolver"
            ))),
        }
    }

    fn tree_file_inline(
        &self,
        entries: &[(Arc<str>, Value)],
        path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segments = path
            .as_str()
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect::<Vec<_>>();
        if segments.is_empty() {
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                "tree.file path must not be empty",
                self.current_demand_chain(None),
            ));
        }
        self.tree_file_inline_segments(entries, &segments, path)
    }

    fn tree_subtree(&self, tree: &TreeValue, path: PathValue) -> Result<Value, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => self.tree_subtree_inline(entries, &path),
            TreeValue::FsDir(root) => {
                if path.as_str().is_empty() {
                    return Ok(Value::Tree(TreeValue::FsDir(root.clone())));
                }
                let full_path = root.join(path.as_str());
                let metadata = fs::metadata(full_path.as_str()).map_err(|error| {
                    PoisonError::new(
                        PoisonKind::UserFail,
                        format!("missing subtree {}: {error}", full_path.as_str()),
                        self.current_demand_chain(None),
                    )
                })?;
                if !metadata.is_dir() {
                    return Err(PoisonError::new(
                        PoisonKind::UserFail,
                        format!("{} is not a directory", full_path.as_str()),
                        self.current_demand_chain(None),
                    ));
                }
                Ok(Value::Tree(TreeValue::FsDir(full_path)))
            }
            TreeValue::Store(root) => Err(self.internal_error(&format!(
                "cannot read store-backed tree {root} without an external store resolver"
            ))),
        }
    }

    fn tree_subtree_inline(
        &self,
        entries: &[(Arc<str>, Value)],
        path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segments = path
            .as_str()
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect::<Vec<_>>();
        if segments.is_empty() {
            return Ok(Value::Tree(TreeValue::Inline(Arc::from(entries.to_vec()))));
        }
        self.tree_subtree_inline_segments(entries, &segments, path)
    }

    fn tree_subtree_inline_segments(
        &self,
        entries: &[(Arc<str>, Value)],
        segments: &[&str],
        original_path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segment = segments[0];
        let (_, value) = entries
            .iter()
            .find(|(name, _)| name.as_ref() == segment)
            .ok_or_else(|| {
                PoisonError::new(
                    PoisonKind::UserFail,
                    format!("missing subtree {}", original_path.as_str()),
                    self.current_demand_chain(None),
                )
            })?;
        let value = self.force_value_shallow(value.clone())?;

        if segments.len() == 1 {
            return match value {
                Value::Tree(tree) => Ok(Value::Tree(tree)),
                Value::Blob(_) => Err(PoisonError::new(
                    PoisonKind::UserFail,
                    format!("{} is not a directory", original_path.as_str()),
                    self.current_demand_chain(None),
                )),
                other => Err(self.type_error(&format!(
                    "tree entry must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            };
        }

        match value {
            Value::Tree(TreeValue::Inline(entries)) => {
                self.tree_subtree_inline_segments(entries.as_ref(), &segments[1..], original_path)
            }
            Value::Tree(TreeValue::FsDir(root)) => self.tree_subtree(
                &TreeValue::FsDir(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Tree(TreeValue::Store(root)) => self.tree_subtree(
                &TreeValue::Store(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Blob(_) => Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("{} points inside a file", original_path.as_str()),
                self.current_demand_chain(None),
            )),
            other => Err(self.type_error(&format!(
                "tree entry must be Blob or Tree, got {}",
                other.type_name()
            ))),
        }
    }

    fn tree_merge(&self, left: &TreeValue, right: &TreeValue) -> Result<Value, PoisonError> {
        let mut merged = BTreeMap::<Arc<str>, Value>::new();
        for (name, value) in self.tree_entries(left)? {
            merged.insert(name, value);
        }
        for (name, value) in self.tree_entries(right)? {
            match (merged.get(&name).cloned(), &value) {
                (Some(Value::Tree(left_tree)), Value::Tree(right_tree)) => {
                    merged.insert(name, self.tree_merge(&left_tree, right_tree)?);
                }
                _ => {
                    merged.insert(name, value);
                }
            }
        }
        Ok(Value::Tree(TreeValue::Inline(Arc::from(
            merged.into_iter().collect::<Vec<_>>(),
        ))))
    }

    fn tree_entries(&self, tree: &TreeValue) -> Result<Vec<(Arc<str>, Value)>, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => entries
                .iter()
                .map(|(name, value)| Ok((name.clone(), self.force_value_shallow(value.clone())?)))
                .collect(),
            TreeValue::FsDir(root) => {
                let mut names = Vec::new();
                for entry in fs::read_dir(root.as_str()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to list {} for tree merge: {error}",
                        root.as_str()
                    ))
                })? {
                    let entry = entry.map_err(|error| {
                        self.internal_error(&format!(
                            "failed to read {} for tree merge: {error}",
                            root.as_str()
                        ))
                    })?;
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                names.sort();

                let mut entries = Vec::with_capacity(names.len());
                for name in names {
                    let child = root.join(&name);
                    let metadata = fs::metadata(child.as_str()).map_err(|error| {
                        self.internal_error(&format!(
                            "failed to stat {} for tree merge: {error}",
                            child.as_str()
                        ))
                    })?;
                    let value = if metadata.is_dir() {
                        Value::Tree(TreeValue::FsDir(child))
                    } else {
                        Value::Blob(BlobValue::FsFile {
                            executable: host_path_is_executable(Path::new(child.as_str())),
                            path: child,
                        })
                    };
                    entries.push((Arc::from(name), value));
                }
                Ok(entries)
            }
            TreeValue::Store(root) => Err(self.internal_error(&format!(
                "cannot read store-backed tree {root} without an external store resolver"
            ))),
        }
    }

    fn tree_file_inline_segments(
        &self,
        entries: &[(Arc<str>, Value)],
        segments: &[&str],
        original_path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segment = segments[0];
        let (_, value) = entries
            .iter()
            .find(|(name, _)| name.as_ref() == segment)
            .ok_or_else(|| {
                PoisonError::new(
                    PoisonKind::UserFail,
                    format!("missing file {}", original_path.as_str()),
                    self.current_demand_chain(None),
                )
            })?;
        let value = self.force_value(value.clone())?;

        if segments.len() == 1 {
            return match value {
                Value::Blob(blob) => Ok(Value::Blob(blob)),
                Value::Tree(_) => Err(PoisonError::new(
                    PoisonKind::UserFail,
                    format!("{} points to a directory", original_path.as_str()),
                    self.current_demand_chain(None),
                )),
                other => Err(self.type_error(&format!(
                    "tree entry must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            };
        }

        match value {
            Value::Tree(TreeValue::Inline(entries)) => {
                self.tree_file_inline_segments(entries.as_ref(), &segments[1..], original_path)
            }
            Value::Tree(TreeValue::FsDir(root)) => self.tree_file(
                &TreeValue::FsDir(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Tree(TreeValue::Store(root)) => self.tree_file(
                &TreeValue::Store(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Blob(_) => Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("{} points inside a file", original_path.as_str()),
                self.current_demand_chain(None),
            )),
            other => Err(self.type_error(&format!(
                "tree entry must be Blob or Tree, got {}",
                other.type_name()
            ))),
        }
    }

    fn validate_tree_entries(&self, values: &[Value]) -> Result<(), PoisonError> {
        for value in values {
            match value {
                Value::Blob(_) | Value::Tree(_) | Value::Pending(_) => {}
                other => {
                    return Err(self.type_error(&format!(
                        "tree entries must be Blob or Tree, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        Ok(())
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

    fn arithmetic_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::ArithmeticError,
            message,
            self.current_demand_chain(None),
        )
    }

    fn internal_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::Internal,
            message,
            self.current_demand_chain(None),
        )
    }

    fn user_fail(&self, message: Value) -> Result<PoisonError, PoisonError> {
        let message = self.force_value(message)?;
        Ok(PoisonError::new(
            PoisonKind::UserFail,
            message.to_string(),
            self.current_demand_chain(None),
        ))
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

    fn force_pending_shallow(&self, node_id: NodeId) -> Result<Value, PoisonError> {
        self.with_demand(node_id, || self.force_node(node_id))
    }

    // r[impl eval.demand-depth]
    fn with_demand<T>(
        &self,
        node_id: NodeId,
        f: impl FnOnce() -> Result<T, PoisonError>,
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
        let result = f();
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

fn capture_referenced_bindings(
    bindings: &HashMap<String, Value>,
    params: &[instr::Param],
    body: &instr::Block,
) -> Arc<[(Arc<str>, Value)]> {
    let mut visible_locals = params
        .iter()
        .map(|param| param.name.clone())
        .collect::<HashSet<_>>();
    let mut captured = BTreeSet::new();
    collect_block_capture_names(body, &mut visible_locals, bindings, &mut captured);
    Arc::from(
        captured
            .into_iter()
            .filter_map(|name| {
                Some((
                    Arc::<str>::from(name.as_str()),
                    bindings.get(&name)?.clone(),
                ))
            })
            .collect::<Vec<_>>(),
    )
}

fn collect_block_capture_names(
    block: &instr::Block,
    visible_locals: &mut HashSet<String>,
    bindings: &HashMap<String, Value>,
    captured: &mut BTreeSet<String>,
) {
    let mut local_scope = visible_locals.clone();

    for stmt in &block.stmts {
        match stmt {
            instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                collect_expr_capture_names(value, &local_scope, bindings, captured);
                local_scope.insert(name.clone());
            }
            instr::Stmt::LetPattern { pattern, value } => {
                collect_expr_capture_names(value, &local_scope, bindings, captured);
                collect_pattern_bindings(pattern, &mut local_scope);
            }
            instr::Stmt::Expr(expr) | instr::Stmt::Return(expr) => {
                collect_expr_capture_names(expr, &local_scope, bindings, captured);
            }
            instr::Stmt::FnDecl(def) => {
                local_scope.insert(def.name.clone());
                let mut fn_scope = local_scope.clone();
                for param in &def.params {
                    fn_scope.insert(param.name.clone());
                }
                collect_block_capture_names(&def.body, &mut fn_scope, bindings, captured);
            }
            instr::Stmt::StructDecl(decl) => {
                local_scope.insert(decl.name.clone());
            }
        }
    }
}

fn collect_expr_capture_names(
    expr: &instr::Expr,
    visible_locals: &HashSet<String>,
    bindings: &HashMap<String, Value>,
    captured: &mut BTreeSet<String>,
) {
    match expr {
        instr::Expr::Var(name) => {
            if !visible_locals.contains(name) && bindings.contains_key(name) {
                captured.insert(name.clone());
            }
        }
        instr::Expr::LitInt(_)
        | instr::Expr::LitFloat(_)
        | instr::Expr::LitBool(_)
        | instr::Expr::LitString(_)
        | instr::Expr::Unit => {}
        instr::Expr::StringTemplate { parts, .. } | instr::Expr::PathTemplate { parts, .. } => {
            for part in parts {
                if let instr::TemplatePart::Expr(expr) = part {
                    collect_expr_capture_names(expr, visible_locals, bindings, captured);
                }
            }
        }
        instr::Expr::Binary { left, right, .. } => {
            collect_expr_capture_names(left, visible_locals, bindings, captured);
            collect_expr_capture_names(right, visible_locals, bindings, captured);
        }
        instr::Expr::Unary { operand, .. }
        | instr::Expr::Fail {
            message: operand, ..
        }
        | instr::Expr::Fallible(operand) => {
            collect_expr_capture_names(operand, visible_locals, bindings, captured);
        }
        instr::Expr::Block(block) => {
            let mut nested_scope = visible_locals.clone();
            collect_block_capture_names(block, &mut nested_scope, bindings, captured);
        }
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_expr_capture_names(cond, visible_locals, bindings, captured);
            let mut then_scope = visible_locals.clone();
            collect_block_capture_names(then_branch, &mut then_scope, bindings, captured);
            let mut else_scope = visible_locals.clone();
            collect_block_capture_names(else_branch, &mut else_scope, bindings, captured);
        }
        instr::Expr::Closure { def, .. } => {
            let mut closure_scope = visible_locals.clone();
            for param in &def.params {
                closure_scope.insert(param.name.clone());
            }
            collect_block_capture_names(&def.body, &mut closure_scope, bindings, captured);
        }
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        }
        | instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            collect_expr_capture_names(func, visible_locals, bindings, captured);
            for arg in args {
                collect_expr_capture_names(arg, visible_locals, bindings, captured);
            }
            for (_, arg) in kwargs {
                collect_expr_capture_names(arg, visible_locals, bindings, captured);
            }
            if let Some(kwargs_expr) = kwargs_expr {
                collect_expr_capture_names(kwargs_expr, visible_locals, bindings, captured);
            }
        }
        instr::Expr::StructLit { fields, .. } => {
            for (_, value) in fields {
                collect_expr_capture_names(value, visible_locals, bindings, captured);
            }
        }
        instr::Expr::Record(items) => {
            for item in items {
                match item {
                    instr::RecordItem::Field { value, .. } | instr::RecordItem::Spread(value) => {
                        collect_expr_capture_names(value, visible_locals, bindings, captured);
                    }
                }
            }
        }
        instr::Expr::Array(items) => {
            for item in items {
                match item {
                    instr::ArrayItem::Expr(expr) | instr::ArrayItem::Spread(expr) => {
                        collect_expr_capture_names(expr, visible_locals, bindings, captured);
                    }
                }
            }
        }
        instr::Expr::Map(entries) => {
            for (key, value) in entries {
                collect_expr_capture_names(key, visible_locals, bindings, captured);
                collect_expr_capture_names(value, visible_locals, bindings, captured);
            }
        }
        instr::Expr::Field { receiver, .. } => {
            collect_expr_capture_names(receiver, visible_locals, bindings, captured);
        }
        instr::Expr::Tuple(elems) => {
            for elem in elems {
                collect_expr_capture_names(elem, visible_locals, bindings, captured);
            }
        }
    }
}

fn builtin_namespace(name: &str) -> Option<BuiltinValue> {
    match name {
        "exec" => Some(BuiltinValue::Function(BuiltinFunction::Exec)),
        "discover" => Some(BuiltinValue::Function(BuiltinFunction::Discover)),
        "executor_class" => Some(BuiltinValue::Function(BuiltinFunction::ExecutorClass)),
        "Blob" => Some(BuiltinValue::Namespace(BuiltinNamespace::Blob)),
        "Bytes" => Some(BuiltinValue::Namespace(BuiltinNamespace::Bytes)),
        "Tree" => Some(BuiltinValue::Namespace(BuiltinNamespace::Tree)),
        _ => None,
    }
}

fn fn_ptr(def: &Arc<instr::FnDef>) -> usize {
    Arc::as_ptr(def) as usize
}

fn stable_fn_repr(def: &instr::FnDef) -> String {
    let mut out = String::new();
    write_fn_def_repr(&mut out, def);
    out
}

fn write_fn_def_repr(out: &mut String, def: &instr::FnDef) {
    let _ = write!(out, "fn {}(", def.name);
    for (index, param) in def.params.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&param.name);
    }
    out.push(')');
    write_block_repr(out, &def.body);
}

fn write_block_repr(out: &mut String, block: &instr::Block) {
    out.push('{');
    for stmt in &block.stmts {
        write_stmt_repr(out, stmt);
        out.push(';');
    }
    out.push('}');
}

fn write_stmt_repr(out: &mut String, stmt: &instr::Stmt) {
    match stmt {
        instr::Stmt::Let { name, value } => {
            let _ = write!(out, "let {name}=");
            write_expr_repr(out, value);
        }
        instr::Stmt::LetPattern { pattern, value } => {
            out.push_str("let_pattern ");
            write_pattern_repr(out, pattern);
            out.push('=');
            write_expr_repr(out, value);
        }
        instr::Stmt::Assign { name, value } => {
            let _ = write!(out, "assign {name}=");
            write_expr_repr(out, value);
        }
        instr::Stmt::Expr(expr) => write_expr_repr(out, expr),
        instr::Stmt::Return(expr) => {
            out.push_str("return ");
            write_expr_repr(out, expr);
        }
        instr::Stmt::FnDecl(def) => {
            out.push_str("fn_decl ");
            write_fn_def_repr(out, def);
        }
        instr::Stmt::StructDecl(decl) => {
            let _ = write!(out, "struct {}(", decl.name);
            for (index, field) in decl.fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(field);
            }
            out.push(')');
        }
    }
}

fn write_pattern_repr(out: &mut String, pattern: &instr::Pattern) {
    match pattern {
        instr::Pattern::Binding(name) => {
            let _ = write!(out, "bind({name})");
        }
        instr::Pattern::Struct(fields) => {
            out.push_str("struct(");
            for (index, (name, nested)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(name);
                if let Some(nested) = nested {
                    out.push(':');
                    write_pattern_repr(out, nested);
                }
            }
            out.push(')');
        }
    }
}

fn write_expr_repr(out: &mut String, expr: &instr::Expr) {
    match expr {
        instr::Expr::LitInt(value) => {
            let _ = write!(out, "u64({value})");
        }
        instr::Expr::LitFloat(value) => {
            let _ = write!(out, "f64({value:?})");
        }
        instr::Expr::LitBool(value) => {
            let _ = write!(out, "bool({value})");
        }
        instr::Expr::LitString(value) => {
            let _ = write!(out, "string({value:?})");
        }
        instr::Expr::StringTemplate { parts, .. } => {
            out.push_str("string_template(");
            write_template_parts_repr(out, parts);
            out.push(')');
        }
        instr::Expr::PathTemplate { parts, .. } => {
            out.push_str("path_template(");
            write_template_parts_repr(out, parts);
            out.push(')');
        }
        instr::Expr::Unit => out.push_str("unit"),
        instr::Expr::Var(name) => {
            let _ = write!(out, "var({name})");
        }
        instr::Expr::Binary {
            op, left, right, ..
        } => {
            let _ = write!(out, "binary({op:?},");
            write_expr_repr(out, left);
            out.push(',');
            write_expr_repr(out, right);
            out.push(')');
        }
        instr::Expr::Unary { op, operand, .. } => {
            let _ = write!(out, "unary({op:?},");
            write_expr_repr(out, operand);
            out.push(')');
        }
        instr::Expr::Block(block) => write_block_repr(out, block),
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            out.push_str("if(");
            write_expr_repr(out, cond);
            out.push(')');
            write_block_repr(out, then_branch);
            write_block_repr(out, else_branch);
        }
        instr::Expr::Closure { closure_id, def } => {
            let _ = write!(out, "closure({closure_id},");
            write_fn_def_repr(out, def);
            out.push(')');
        }
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            out.push_str("call(");
            write_expr_repr(out, func);
            write_expr_list_repr(out, args);
            write_kwargs_repr(out, kwargs);
            if let Some(kwargs_expr) = kwargs_expr {
                out.push_str("kwargs_expr=");
                write_expr_repr(out, kwargs_expr);
            }
            out.push(')');
        }
        instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            out.push_str("partial_call(");
            write_expr_repr(out, func);
            write_expr_list_repr(out, args);
            write_kwargs_repr(out, kwargs);
            if let Some(kwargs_expr) = kwargs_expr {
                out.push_str("kwargs_expr=");
                write_expr_repr(out, kwargs_expr);
            }
            out.push(')');
        }
        instr::Expr::StructLit { name, fields } => {
            let _ = write!(out, "struct_lit({name}:");
            out.push('[');
            for (index, (field, value)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(field);
                out.push('=');
                write_expr_repr(out, value);
            }
            out.push_str("])");
        }
        instr::Expr::Record(items) => {
            out.push_str("record(");
            write_record_items_repr(out, items);
            out.push(')');
        }
        instr::Expr::Array(items) => {
            out.push_str("array(");
            write_array_items_repr(out, items);
            out.push(')');
        }
        instr::Expr::Map(entries) => {
            out.push_str("map(");
            out.push('[');
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_expr_repr(out, key);
                out.push_str("=>");
                write_expr_repr(out, value);
            }
            out.push_str("])");
        }
        instr::Expr::Field {
            receiver, field, ..
        } => {
            out.push_str("field(");
            write_expr_repr(out, receiver);
            let _ = write!(out, ",{field})");
        }
        instr::Expr::Fail { message, .. } => {
            out.push_str("fail(");
            write_expr_repr(out, message);
            out.push(')');
        }
        instr::Expr::Fallible(expr) => {
            out.push_str("fallible(");
            write_expr_repr(out, expr);
            out.push(')');
        }
        instr::Expr::Tuple(items) => {
            out.push_str("tuple(");
            write_expr_list_repr(out, items);
            out.push(')');
        }
    }
}

fn write_template_parts_repr(out: &mut String, parts: &[instr::TemplatePart]) {
    out.push('[');
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match part {
            instr::TemplatePart::Text(text) => {
                let _ = write!(out, "text({text:?})");
            }
            instr::TemplatePart::Expr(expr) => {
                out.push_str("expr(");
                write_expr_repr(out, expr);
                out.push(')');
            }
        }
    }
    out.push(']');
}

fn write_expr_list_repr(out: &mut String, exprs: &[instr::Expr]) {
    out.push('[');
    for (index, expr) in exprs.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_expr_repr(out, expr);
    }
    out.push(']');
}

fn write_kwargs_repr(out: &mut String, kwargs: &[(String, instr::Expr)]) {
    out.push('[');
    for (index, (name, expr)) in kwargs.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(name);
        out.push('=');
        write_expr_repr(out, expr);
    }
    out.push(']');
}

fn write_record_items_repr(out: &mut String, items: &[instr::RecordItem]) {
    out.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match item {
            instr::RecordItem::Field { name, value } => {
                out.push_str(name);
                out.push('=');
                write_expr_repr(out, value);
            }
            instr::RecordItem::Spread(expr) => {
                out.push_str("..");
                write_expr_repr(out, expr);
            }
        }
    }
    out.push(']');
}

fn write_array_items_repr(out: &mut String, items: &[instr::ArrayItem]) {
    out.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match item {
            instr::ArrayItem::Expr(expr) => write_expr_repr(out, expr),
            instr::ArrayItem::Spread(expr) => {
                out.push_str("..");
                write_expr_repr(out, expr);
            }
        }
    }
    out.push(']');
}

fn option_some(value: Value) -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("Some"),
        fields: Arc::from([(Arc::from("value"), value)]),
    })
}

fn option_none() -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("None"),
        fields: Arc::from([]),
    })
}

pub fn verify_host_obligations(obligations: &[HostObservation]) -> Result<(), String> {
    for obligation in obligations {
        match obligation {
            HostObservation::Which {
                name,
                expected_path,
            } => {
                let actual = resolve_host_which(name);
                match (expected_path.as_deref(), actual.as_deref()) {
                    (Some(expected), Some(actual)) if expected == actual => {}
                    (Some(expected), Some(actual)) => {
                        return Err(format!(
                            "host.which(\"{name}\") changed: expected {expected}, got {actual}"
                        ));
                    }
                    (Some(expected), None) => {
                        return Err(format!(
                            "host.which(\"{name}\") changed: expected {expected}, but it is now missing"
                        ));
                    }
                    (None, Some(actual)) => {
                        return Err(format!(
                            "host.which(\"{name}\") changed: expected missing, got {actual}"
                        ));
                    }
                    (None, None) => {}
                }
            }
            HostObservation::Read { path, content_hash } => {
                let actual = hash_host_file(path).map_err(|error| {
                    format!("host.read verification failed for {path}: {error}")
                })?;
                if actual != *content_hash {
                    return Err(format!(
                        "host.read(\"{path}\") changed: expected {content_hash}, got {actual}"
                    ));
                }
            }
            HostObservation::Hash { path, content_hash } => {
                let actual = hash_host_file(path).map_err(|error| {
                    format!("host.hash verification failed for {path}: {error}")
                })?;
                if actual != *content_hash {
                    return Err(format!(
                        "host.hash(\"{path}\") changed: expected {content_hash}, got {actual}"
                    ));
                }
            }
        }
    }

    Ok(())
}

fn hash_host_file(path: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn host_lookup_candidate(name: &str) -> Option<&Path> {
    let candidate = Path::new(name);
    if candidate.is_absolute() || name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        Some(candidate)
    } else {
        None
    }
}

fn resolve_host_which(name: &str) -> Option<String> {
    if host_lookup_candidate(name).is_some_and(host_path_is_executable) {
        return Some(name.to_string());
    }

    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if host_path_is_executable(&candidate) {
            return Some(candidate.to_string_lossy().to_string());
        }
    }

    None
}

fn host_path_is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn set_executable_if_needed(path: &str, executable: bool) -> std::io::Result<()> {
    if !executable {
        return Ok(());
    }

    #[cfg(unix)]
    {
        let metadata = fs::metadata(path)?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(path, permissions)?;
    }

    Ok(())
}

// r[impl fn.identity]
// r[impl fn.identity.overloads]
// r[impl fn.identity.scc]
// r[impl fn.identity.stability]
// r[impl eval.memoization]
fn compute_function_identities(functions: &[Arc<instr::FnDef>]) -> HashMap<usize, Arc<str>> {
    let top_level_names = functions
        .iter()
        .map(|function| function.name.clone())
        .collect::<HashSet<_>>();
    let overloads = functions.iter().enumerate().fold(
        BTreeMap::<String, Vec<usize>>::new(),
        |mut overloads, (index, function)| {
            overloads
                .entry(function.name.clone())
                .or_default()
                .push(index);
            overloads
        },
    );

    let edges = functions
        .iter()
        .map(|function| {
            collect_global_callee_names(function, &top_level_names)
                .into_iter()
                .flat_map(|name| overloads.get(&name).into_iter().flatten().copied())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    let (component_of, components) = strongly_connected_components(&edges);
    let mut component_identities = vec![None; components.len()];

    for component_index in 0..components.len() {
        component_identity(
            component_index,
            &components,
            &component_of,
            &edges,
            functions,
            &mut component_identities,
        );
    }

    functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            (
                fn_ptr(function),
                component_identities[component_of[index]]
                    .clone()
                    .expect("component identity should be computed"),
            )
        })
        .collect()
}

fn component_identity(
    component_index: usize,
    components: &[Vec<usize>],
    component_of: &[usize],
    edges: &[BTreeSet<usize>],
    functions: &[Arc<instr::FnDef>],
    component_identities: &mut [Option<Arc<str>>],
) -> Arc<str> {
    if let Some(identity) = &component_identities[component_index] {
        return identity.clone();
    }

    let mut members = components[component_index]
        .iter()
        .map(|&index| stable_fn_repr(&functions[index]))
        .collect::<Vec<_>>();
    members.sort();

    let mut callees = components[component_index]
        .iter()
        .flat_map(|&index| edges[index].iter().copied())
        .map(|callee| component_of[callee])
        .filter(|&callee_component| callee_component != component_index)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|callee_component| {
            component_identity(
                callee_component,
                components,
                component_of,
                edges,
                functions,
                component_identities,
            )
            .to_string()
        })
        .collect::<Vec<_>>();
    callees.sort();

    let identity: Arc<str> = Arc::from(format!(
        "component:[{}]->[{}]",
        members.join("|"),
        callees.join("|")
    ));
    component_identities[component_index] = Some(identity.clone());
    identity
}

fn strongly_connected_components(edges: &[BTreeSet<usize>]) -> (Vec<usize>, Vec<Vec<usize>>) {
    struct Tarjan<'a> {
        edges: &'a [BTreeSet<usize>],
        index: usize,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        indices: Vec<Option<usize>>,
        lowlinks: Vec<usize>,
        component_of: Vec<usize>,
        components: Vec<Vec<usize>>,
    }

    impl<'a> Tarjan<'a> {
        fn visit(&mut self, node: usize) {
            let node_index = self.index;
            self.indices[node] = Some(node_index);
            self.lowlinks[node] = node_index;
            self.index += 1;
            self.stack.push(node);
            self.on_stack[node] = true;

            for &next in &self.edges[node] {
                if self.indices[next].is_none() {
                    self.visit(next);
                    self.lowlinks[node] = self.lowlinks[node].min(self.lowlinks[next]);
                } else if self.on_stack[next] {
                    self.lowlinks[node] =
                        self.lowlinks[node].min(self.indices[next].expect("known index"));
                }
            }

            if self.lowlinks[node] == node_index {
                let component_index = self.components.len();
                let mut component = Vec::new();
                loop {
                    let member = self.stack.pop().expect("stack should not be empty");
                    self.on_stack[member] = false;
                    self.component_of[member] = component_index;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                self.components.push(component);
            }
        }
    }

    let mut tarjan = Tarjan {
        edges,
        index: 0,
        stack: Vec::new(),
        on_stack: vec![false; edges.len()],
        indices: vec![None; edges.len()],
        lowlinks: vec![0; edges.len()],
        component_of: vec![0; edges.len()],
        components: Vec::new(),
    };

    for node in 0..edges.len() {
        if tarjan.indices[node].is_none() {
            tarjan.visit(node);
        }
    }

    (tarjan.component_of, tarjan.components)
}

fn collect_global_callee_names(
    function: &instr::FnDef,
    top_level_names: &HashSet<String>,
) -> BTreeSet<String> {
    let mut visible_locals = function
        .params
        .iter()
        .map(|param| param.name.clone())
        .collect::<HashSet<_>>();
    let mut deps = BTreeSet::new();
    collect_block_callee_names(
        &function.body,
        &mut visible_locals,
        top_level_names,
        &mut deps,
    );
    deps
}

fn collect_block_callee_names(
    block: &instr::Block,
    visible_locals: &mut HashSet<String>,
    top_level_names: &HashSet<String>,
    deps: &mut BTreeSet<String>,
) {
    let mut local_scope = visible_locals.clone();

    for stmt in &block.stmts {
        match stmt {
            instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                collect_expr_callee_names(value, &local_scope, top_level_names, deps);
                local_scope.insert(name.clone());
            }
            instr::Stmt::LetPattern { pattern, value } => {
                collect_expr_callee_names(value, &local_scope, top_level_names, deps);
                collect_pattern_bindings(pattern, &mut local_scope);
            }
            instr::Stmt::Expr(expr) | instr::Stmt::Return(expr) => {
                collect_expr_callee_names(expr, &local_scope, top_level_names, deps);
            }
            instr::Stmt::FnDecl(def) => {
                local_scope.insert(def.name.clone());
                let mut fn_scope = local_scope.clone();
                for param in &def.params {
                    fn_scope.insert(param.name.clone());
                }
                collect_block_callee_names(&def.body, &mut fn_scope, top_level_names, deps);
            }
            instr::Stmt::StructDecl(_) => {}
        }
    }
}

fn collect_expr_callee_names(
    expr: &instr::Expr,
    visible_locals: &HashSet<String>,
    top_level_names: &HashSet<String>,
    deps: &mut BTreeSet<String>,
) {
    match expr {
        instr::Expr::LitInt(_)
        | instr::Expr::LitFloat(_)
        | instr::Expr::LitBool(_)
        | instr::Expr::LitString(_)
        | instr::Expr::Unit
        | instr::Expr::Var(_) => {}
        instr::Expr::StringTemplate { parts, .. } | instr::Expr::PathTemplate { parts, .. } => {
            for part in parts {
                if let instr::TemplatePart::Expr(expr) = part {
                    collect_expr_callee_names(expr, visible_locals, top_level_names, deps);
                }
            }
        }
        instr::Expr::Binary { left, right, .. } => {
            collect_expr_callee_names(left, visible_locals, top_level_names, deps);
            collect_expr_callee_names(right, visible_locals, top_level_names, deps);
        }
        instr::Expr::Unary { operand, .. }
        | instr::Expr::Fail {
            message: operand, ..
        }
        | instr::Expr::Fallible(operand) => {
            collect_expr_callee_names(operand, visible_locals, top_level_names, deps);
        }
        instr::Expr::Block(block) => {
            let mut nested_scope = visible_locals.clone();
            collect_block_callee_names(block, &mut nested_scope, top_level_names, deps);
        }
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_expr_callee_names(cond, visible_locals, top_level_names, deps);
            let mut then_scope = visible_locals.clone();
            collect_block_callee_names(then_branch, &mut then_scope, top_level_names, deps);
            let mut else_scope = visible_locals.clone();
            collect_block_callee_names(else_branch, &mut else_scope, top_level_names, deps);
        }
        instr::Expr::Closure { def, .. } => {
            let mut closure_scope = visible_locals.clone();
            for param in &def.params {
                closure_scope.insert(param.name.clone());
            }
            collect_block_callee_names(&def.body, &mut closure_scope, top_level_names, deps);
        }
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        }
        | instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            if let instr::Expr::Var(name) = func.as_ref()
                && !visible_locals.contains(name)
                && top_level_names.contains(name)
            {
                deps.insert(name.clone());
            }
            collect_expr_callee_names(func, visible_locals, top_level_names, deps);
            for arg in args {
                collect_expr_callee_names(arg, visible_locals, top_level_names, deps);
            }
            for (_, arg) in kwargs {
                collect_expr_callee_names(arg, visible_locals, top_level_names, deps);
            }
            if let Some(kwargs_expr) = kwargs_expr {
                collect_expr_callee_names(kwargs_expr, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::StructLit { fields, .. } => {
            for (_, value) in fields {
                collect_expr_callee_names(value, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::Record(items) => {
            for item in items {
                match item {
                    instr::RecordItem::Field { value, .. } | instr::RecordItem::Spread(value) => {
                        collect_expr_callee_names(value, visible_locals, top_level_names, deps);
                    }
                }
            }
        }
        instr::Expr::Array(items) => {
            for item in items {
                match item {
                    instr::ArrayItem::Expr(expr) | instr::ArrayItem::Spread(expr) => {
                        collect_expr_callee_names(expr, visible_locals, top_level_names, deps);
                    }
                }
            }
        }
        instr::Expr::Map(entries) => {
            for (key, value) in entries {
                collect_expr_callee_names(key, visible_locals, top_level_names, deps);
                collect_expr_callee_names(value, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::Field { receiver, .. } => {
            collect_expr_callee_names(receiver, visible_locals, top_level_names, deps);
        }
        instr::Expr::Tuple(elems) => {
            for elem in elems {
                collect_expr_callee_names(elem, visible_locals, top_level_names, deps);
            }
        }
    }
}

fn collect_pattern_bindings(pattern: &instr::Pattern, visible_locals: &mut HashSet<String>) {
    match pattern {
        instr::Pattern::Binding(name) => {
            visible_locals.insert(name.clone());
        }
        instr::Pattern::Struct(fields) => {
            for (name, nested) in fields {
                if let Some(nested) = nested {
                    collect_pattern_bindings(nested, visible_locals);
                } else {
                    visible_locals.insert(name.clone());
                }
            }
        }
    }
}
