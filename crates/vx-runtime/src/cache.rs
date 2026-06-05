use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use vx_cache_index::LocalCacheIndex;
use vx_exec_protocol::WireValue;
use vx_runtime_core::NodeOutput;
use vx_runtime_driver::{CallCache, CallCacheKey};

use crate::strand::HandleId;

#[derive(facet::Facet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct HostExecCacheKey {
    pub(crate) argv: Vec<String>,
    pub(crate) program_fingerprint: String,
}

#[derive(facet::Facet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExecInvocationCacheKey {
    pub(crate) command_package: String,
    pub(crate) command_name: String,
    pub(crate) program: String,
    pub(crate) program_fingerprint: String,
    pub(crate) declared_tool_fingerprint: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) output_roots: Vec<String>,
    pub(crate) observer: ObserverCacheIdentity,
}

#[derive(facet::Facet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FunctionOutputCacheKey {
    function: String,
    code_identity: String,
    args: Vec<CacheableNodeOutput>,
}

#[derive(facet::Facet, Debug, Clone, PartialEq, Eq, Hash)]
#[repr(u8)]
enum CacheableNodeOutput {
    U64(u64),
    Bool(bool),
    Str(String),
    Unit,
}

impl CacheableNodeOutput {
    fn from_output(output: &NodeOutput) -> Option<Self> {
        match output {
            NodeOutput::U64(value) => Some(Self::U64(*value)),
            NodeOutput::Bool(value) => Some(Self::Bool(*value)),
            NodeOutput::Str(value) => Some(Self::Str(value.clone())),
            NodeOutput::Unit => Some(Self::Unit),
            NodeOutput::OutputTree
            | NodeOutput::LazyData { .. }
            | NodeOutput::Struct { .. }
            | NodeOutput::Variant { .. }
            | NodeOutput::Failure { .. }
            | NodeOutput::FailureContext(_) => None,
        }
    }

    fn into_output(self) -> NodeOutput {
        match self {
            Self::U64(value) => NodeOutput::U64(value),
            Self::Bool(value) => NodeOutput::Bool(value),
            Self::Str(value) => NodeOutput::Str(value),
            Self::Unit => NodeOutput::Unit,
        }
    }
}

impl FunctionOutputCacheKey {
    fn from_call_key(key: &CallCacheKey) -> Option<Self> {
        let args = key
            .args
            .iter()
            .map(CacheableNodeOutput::from_output)
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            function: key.function.clone(),
            code_identity: key.code_identity.clone(),
            args,
        })
    }
}

#[derive(facet::Facet, Debug, Clone, PartialEq, Eq, Hash)]
#[repr(u8)]
pub(crate) enum ObserverCacheIdentity {
    Default,
    Bundle(String),
}

#[derive(Debug, Clone)]
pub(crate) enum CachedExecOutcome {
    Value(WireValue),
    Tree(HandleId),
}

trait PersistentCacheLayer {
    fn get_exec_invocation(&self, key: &ExecInvocationCacheKey) -> Option<WireValue>;
    fn put_exec_invocation(&self, key: &ExecInvocationCacheKey, value: &WireValue);
    fn get_host_exec(&self, key: &HostExecCacheKey) -> Option<crate::host::HostExec>;
    fn put_host_exec(&self, key: &HostExecCacheKey, value: &crate::host::HostExec);
    fn get_function_output(&self, key: &FunctionOutputCacheKey) -> Option<CacheableNodeOutput>;
    fn put_function_output(&self, key: &FunctionOutputCacheKey, value: &CacheableNodeOutput);
}

struct LocalIndexCacheLayer {
    index: LocalCacheIndex,
}

impl LocalIndexCacheLayer {
    fn new(cache_dir: PathBuf) -> Self {
        Self {
            index: LocalCacheIndex::new(cache_dir),
        }
    }
}

impl PersistentCacheLayer for LocalIndexCacheLayer {
    fn get_exec_invocation(&self, key: &ExecInvocationCacheKey) -> Option<WireValue> {
        self.index.read("exec-value-v1", key)
    }

    fn put_exec_invocation(&self, key: &ExecInvocationCacheKey, value: &WireValue) {
        self.index.write("exec-value-v1", key, value);
    }

    fn get_host_exec(&self, key: &HostExecCacheKey) -> Option<crate::host::HostExec> {
        self.index.read("host-exec-v1", key)
    }

    fn put_host_exec(&self, key: &HostExecCacheKey, value: &crate::host::HostExec) {
        self.index.write("host-exec-v1", key, value);
    }

    fn get_function_output(&self, key: &FunctionOutputCacheKey) -> Option<CacheableNodeOutput> {
        self.index.read("function-output-v1", key)
    }

    fn put_function_output(&self, key: &FunctionOutputCacheKey, value: &CacheableNodeOutput) {
        self.index.write("function-output-v1", key, value);
    }
}

pub(crate) struct RuntimeCache {
    host_exec_l1: RefCell<HashMap<HostExecCacheKey, crate::host::HostExec>>,
    exec_invocation_l1: RefCell<HashMap<ExecInvocationCacheKey, CachedExecOutcome>>,
    persistent: Option<Box<dyn PersistentCacheLayer>>,
}

impl RuntimeCache {
    pub(crate) fn new() -> Self {
        Self {
            host_exec_l1: RefCell::new(HashMap::new()),
            exec_invocation_l1: RefCell::new(HashMap::new()),
            persistent: None,
        }
    }

    pub(crate) fn with_local_index_dir(cache_dir: PathBuf) -> Self {
        Self {
            persistent: Some(Box::new(LocalIndexCacheLayer::new(cache_dir))),
            ..Self::new()
        }
    }

    pub(crate) fn has_persistent_layer(&self) -> bool {
        self.persistent.is_some()
    }

    pub(crate) fn function_output_call_cache(self: Rc<Self>) -> FunctionOutputCallCache {
        FunctionOutputCallCache { cache: self }
    }

    pub(crate) fn get_exec_invocation(
        &self,
        key: &ExecInvocationCacheKey,
    ) -> Option<CachedExecOutcome> {
        if let Some(cached) = self.exec_invocation_l1.borrow().get(key).cloned() {
            return Some(cached);
        }
        let wire = self
            .persistent
            .as_ref()
            .and_then(|cache| cache.get_exec_invocation(key))?;
        let cached = CachedExecOutcome::Value(wire);
        self.exec_invocation_l1
            .borrow_mut()
            .insert(key.clone(), cached.clone());
        Some(cached)
    }

    pub(crate) fn put_exec_invocation(
        &self,
        key: ExecInvocationCacheKey,
        outcome: CachedExecOutcome,
    ) {
        if let (Some(cache), CachedExecOutcome::Value(wire)) = (&self.persistent, &outcome) {
            cache.put_exec_invocation(&key, wire);
        }
        self.exec_invocation_l1.borrow_mut().insert(key, outcome);
    }

    pub(crate) fn get_host_exec(&self, key: &HostExecCacheKey) -> Option<crate::host::HostExec> {
        if let Some(cached) = self.host_exec_l1.borrow().get(key).cloned() {
            return Some(cached);
        }
        let cached = self
            .persistent
            .as_ref()
            .and_then(|cache| cache.get_host_exec(key))?;
        self.host_exec_l1
            .borrow_mut()
            .insert(key.clone(), cached.clone());
        Some(cached)
    }

    pub(crate) fn put_host_exec(&self, key: HostExecCacheKey, value: crate::host::HostExec) {
        if let Some(cache) = &self.persistent {
            cache.put_host_exec(&key, &value);
        }
        self.host_exec_l1.borrow_mut().insert(key, value);
    }

    fn get_function_output(&self, key: &CallCacheKey) -> Option<NodeOutput> {
        let key = FunctionOutputCacheKey::from_call_key(key)?;
        self.persistent
            .as_ref()?
            .get_function_output(&key)
            .map(|value| value.into_output())
    }

    fn put_function_output(&self, key: &CallCacheKey, output: &NodeOutput) {
        let Some(cache) = &self.persistent else {
            return;
        };
        let Some(key) = FunctionOutputCacheKey::from_call_key(key) else {
            return;
        };
        let Some(value) = CacheableNodeOutput::from_output(output) else {
            return;
        };
        cache.put_function_output(&key, &value);
    }
}

pub(crate) struct FunctionOutputCallCache {
    cache: Rc<RuntimeCache>,
}

impl CallCache for FunctionOutputCallCache {
    fn get(&mut self, key: &CallCacheKey) -> Option<NodeOutput> {
        self.cache.get_function_output(key)
    }

    fn put(&mut self, key: &CallCacheKey, output: &NodeOutput) {
        self.cache.put_function_output(key, output);
    }
}
