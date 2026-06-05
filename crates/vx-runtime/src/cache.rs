use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;

use vx_cache_index::LocalCacheIndex;
use vx_exec_protocol::WireValue;
use vx_runtime_core::NodeOutput;
use vx_runtime_driver::{CallCache, CallCacheKey};

use crate::strand::{CacheClientId, HandleId};

const EXEC_VALUE_NAMESPACE: &str = "exec-value-v1";
const HOST_EXEC_NAMESPACE: &str = "host-exec-v1";
const FUNCTION_OUTPUT_NAMESPACE: &str = "function-output-v1";

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuntimeCacheRecordKey {
    namespace: &'static str,
    input_digest: String,
}

impl RuntimeCacheRecordKey {
    pub fn namespace(&self) -> &str {
        self.namespace
    }

    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }
}

pub trait RuntimeCacheBackend: Send {
    fn get_record(&self, key: &RuntimeCacheRecordKey) -> Option<String>;
    fn put_record(&self, key: &RuntimeCacheRecordKey, record: &str);
}

pub type RuntimeCacheFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait RuntimeCacheService: Send + Sync + 'static {
    fn get_record<'a>(
        &'a self,
        key: RuntimeCacheRecordKey,
    ) -> RuntimeCacheFuture<'a, Result<Option<String>, String>>;

    fn put_record<'a>(
        &'a self,
        key: RuntimeCacheRecordKey,
        record: String,
    ) -> RuntimeCacheFuture<'a, Result<(), String>>;
}

#[derive(Debug, Clone, Copy)]
pub struct StrandCacheBackend {
    client: CacheClientId,
}

impl StrandCacheBackend {
    pub fn new(client: CacheClientId) -> Self {
        Self { client }
    }
}

impl RuntimeCacheBackend for StrandCacheBackend {
    fn get_record(&self, key: &RuntimeCacheRecordKey) -> Option<String> {
        crate::strand::dispatch_cache_get(self.client, key.clone())
            .ok()
            .flatten()
    }

    fn put_record(&self, key: &RuntimeCacheRecordKey, record: &str) {
        let _ = crate::strand::dispatch_cache_put(self.client, key.clone(), record.to_string());
    }
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

impl RuntimeCacheBackend for LocalIndexCacheLayer {
    fn get_record(&self, key: &RuntimeCacheRecordKey) -> Option<String> {
        self.index.read_record(key.namespace, &key.input_digest)
    }

    fn put_record(&self, key: &RuntimeCacheRecordKey, record: &str) {
        self.index
            .write_record(key.namespace, &key.input_digest, record);
    }
}

pub(crate) struct RuntimeCache {
    host_exec_l1: RefCell<HashMap<HostExecCacheKey, crate::host::HostExec>>,
    exec_invocation_l1: RefCell<HashMap<ExecInvocationCacheKey, CachedExecOutcome>>,
    persistent: Option<Box<dyn RuntimeCacheBackend>>,
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

    pub(crate) fn with_backend(backend: Box<dyn RuntimeCacheBackend>) -> Self {
        Self {
            persistent: Some(backend),
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
        let wire: WireValue = self
            .persistent
            .as_ref()
            .and_then(|cache| get_persistent_record(cache.as_ref(), EXEC_VALUE_NAMESPACE, key))?;
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
            put_persistent_record(cache.as_ref(), EXEC_VALUE_NAMESPACE, &key, wire);
        }
        self.exec_invocation_l1.borrow_mut().insert(key, outcome);
    }

    pub(crate) fn get_host_exec(&self, key: &HostExecCacheKey) -> Option<crate::host::HostExec> {
        if let Some(cached) = self.host_exec_l1.borrow().get(key).cloned() {
            return Some(cached);
        }
        let cached: crate::host::HostExec = self
            .persistent
            .as_ref()
            .and_then(|cache| get_persistent_record(cache.as_ref(), HOST_EXEC_NAMESPACE, key))?;
        self.host_exec_l1
            .borrow_mut()
            .insert(key.clone(), cached.clone());
        Some(cached)
    }

    pub(crate) fn put_host_exec(&self, key: HostExecCacheKey, value: crate::host::HostExec) {
        if let Some(cache) = &self.persistent {
            put_persistent_record(cache.as_ref(), HOST_EXEC_NAMESPACE, &key, &value);
        }
        self.host_exec_l1.borrow_mut().insert(key, value);
    }

    fn get_function_output(&self, key: &CallCacheKey) -> Option<NodeOutput> {
        let key = FunctionOutputCacheKey::from_call_key(key)?;
        get_persistent_record::<_, CacheableNodeOutput>(
            self.persistent.as_ref()?.as_ref(),
            FUNCTION_OUTPUT_NAMESPACE,
            &key,
        )
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
        put_persistent_record(cache.as_ref(), FUNCTION_OUTPUT_NAMESPACE, &key, &value);
    }
}

fn get_persistent_record<K, V>(
    cache: &dyn RuntimeCacheBackend,
    namespace: &'static str,
    key: &K,
) -> Option<V>
where
    K: for<'a> facet::Facet<'a>,
    V: for<'a> facet::Facet<'a>,
{
    let key = persistent_cache_key(namespace, key)?;
    let record = cache.get_record(&key)?;
    facet_json::from_str(&record).ok()
}

fn put_persistent_record<K, V>(
    cache: &dyn RuntimeCacheBackend,
    namespace: &'static str,
    key: &K,
    value: &V,
) where
    K: for<'a> facet::Facet<'a>,
    V: for<'a> facet::Facet<'a>,
{
    let Some(key) = persistent_cache_key(namespace, key) else {
        return;
    };
    let Ok(record) = facet_json::to_string(value) else {
        return;
    };
    cache.put_record(&key, &record);
}

fn persistent_cache_key<K>(namespace: &'static str, key: &K) -> Option<RuntimeCacheRecordKey>
where
    K: for<'a> facet::Facet<'a>,
{
    let key_json = facet_json::to_string(key).ok()?;
    Some(RuntimeCacheRecordKey {
        namespace,
        input_digest: blake3::hash(key_json.as_bytes()).to_hex().to_string(),
    })
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
