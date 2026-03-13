//! Runtime value representation.
//!
//! Values are immutable. Compound values can carry pending children, and
//! `Value::Pending` points at a node whose result will be demanded later.

use std::fmt;
use std::fs;
use std::sync::Arc;

use facet::Facet;
use vx_store_proto::{BlobRef, NodeHash};

use crate::instr::FnDef;
use crate::node::NodeId;

// r[impl eval.pending]
// i[impl impl.scalar]
// i[impl impl.string]
// i[impl impl.cycle-free]
/// A runtime value.
#[derive(Debug, Clone)]
pub enum Value {
    /// A lazily computed value backed by a node.
    Pending(NodeId),
    /// 64-bit unsigned integer
    U64(u64),
    /// 64-bit signed integer
    I64(i64),
    /// 64-bit float
    F64(f64),
    /// Boolean
    Bool(bool),
    /// String
    String(Arc<str>),
    // r[impl store.path]
    /// A normalized slash-delimited path.
    Path(PathValue),
    /// An exec-only path carrying implicit mount provenance.
    ExecPath(ExecPathValue),
    // r[impl store.blob]
    /// A file-like content handle.
    Blob(BlobValue),
    /// Raw in-memory bytes.
    Bytes(Arc<[u8]>),
    // r[impl store.tree]
    /// A directory-like content handle.
    Tree(TreeValue),
    /// An explicit executor selector used by runtime boundaries.
    Executor(ExecutorSelector),
    /// An executor-local verifier handle.
    Host(HostValue),
    /// A value with hidden discovery verification obligations.
    Discovered(DiscoveredValue),
    /// Unit `()`
    Unit,
    /// Tuple
    Tuple(Arc<[Value]>),
    // i[impl impl.array]
    /// Ordered array.
    Array(Arc<[Value]>),
    // i[impl impl.map]
    /// Ordered map.
    Map(MapValue),
    /// Struct instance
    Struct(StructValue),
    /// A function (closure or named function)
    Function(FunctionValue),
    /// A partially applied call surface.
    Partial(Arc<PartialValue>),
    /// Builtin namespace or bound builtin method.
    Builtin(BuiltinValue),
}

/// An ordered map.
#[derive(Debug, Clone)]
pub struct MapValue {
    pub entries: Arc<[(Value, Value)]>,
}

/// A normalized slash-delimited path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathValue {
    raw: Arc<str>,
}

/// A provenance-bearing exec path.
#[derive(Debug, Clone)]
pub enum ExecPathValue {
    InTree { tree: TreeValue, suffix: PathValue },
    InBlob { blob: BlobValue, leaf: PathValue },
}

/// Blob storage.
#[derive(Debug, Clone)]
pub enum BlobValue {
    Inline { bytes: Arc<[u8]>, executable: bool },
    FsFile { path: PathValue, executable: bool },
    Store { blob_ref: BlobRef, executable: bool },
}

/// Tree storage.
#[derive(Debug, Clone)]
pub enum TreeValue {
    Inline(Arc<[(Arc<str>, Value)]>),
    FsDir(PathValue),
    Store(NodeHash),
}

/// How the orchestrator reaches and manages an executor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[repr(u8)]
pub enum ExecutorBackendKind {
    Class,
    Local,
    SshBootstrap,
    Daemon,
}

/// A concrete executor selector understood by runtime boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct ExecutorSelector {
    pub id: String,
    pub backend: ExecutorBackendKind,
}

/// An opaque verifier-only view of the executor host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostValue;

/// Host facts observed during discovery and rechecked during exec.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[repr(u8)]
pub enum HostObservation {
    Which {
        name: String,
        expected_path: Option<String>,
    },
    Read {
        path: String,
        content_hash: String,
    },
    Hash {
        path: String,
        content_hash: String,
    },
}

/// A runtime wrapper for values produced by discovery.
#[derive(Debug, Clone)]
pub struct DiscoveredValue {
    pub value: Box<Value>,
    pub obligations: Arc<[HostObservation]>,
}

// i[impl impl.persistent]
// i[impl impl.struct]
/// A struct value: type name + fields.
#[derive(Debug, Clone)]
pub struct StructValue {
    pub type_name: Arc<str>,
    pub fields: Arc<[(Arc<str>, Value)]>,
}

// i[impl impl.closure]
/// A function value — either a named top-level function or a closure.
#[derive(Debug, Clone)]
pub struct FunctionValue {
    pub def: Arc<FnDef>,
    pub code_identity: Arc<str>,
    /// Captured bindings from the enclosing scope, kept in sorted order.
    pub captures: Arc<[(Arc<str>, Value)]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialValue {
    pub function: Value,
    pub args: Arc<[Value]>,
    pub kwargs: Arc<[(Arc<str>, Value)]>,
    pub kwargs_expr: Option<Box<Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuiltinNamespace {
    Blob,
    Bytes,
    Tree,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuiltinFunction {
    Acquire,
    Stabilize,
    Render,
    Exec,
    Discover,
    ExecutorClass,
    StructConstructor {
        type_name: Arc<str>,
        fields: Arc<[Arc<str>]>,
    },
}

#[derive(Debug, Clone)]
pub enum BuiltinMethod {
    StringStartsWith { receiver: Box<Value> },
    StringContains { receiver: Box<Value> },
    StringSplit { receiver: Box<Value> },
    StringLines { receiver: Box<Value> },
    StringTrim { receiver: Box<Value> },
    StringToLower { receiver: Box<Value> },
    BlobFromString,
    BytesFromString,
    TreeOf,
    TreeEmpty,
    TreeFromPath,
    ArrayMap { receiver: Box<Value> },
    ArrayToMap { receiver: Box<Value> },
    ArrayFlatten { receiver: Box<Value> },
    ArrayMerge { receiver: Box<Value> },
    MapMapValues { receiver: Box<Value> },
    BlobToString { receiver: Box<Value> },
    BlobExecutable { receiver: Box<Value> },
    BlobUtf8 { receiver: Box<Value> },
    BlobUtf8Lossy { receiver: Box<Value> },
    BytesUtf8 { receiver: Box<Value> },
    BytesUtf8Lossy { receiver: Box<Value> },
    BytesBlob { receiver: Box<Value> },
    StreamCollect { receiver: Box<Value> },
    TreeFile { receiver: Box<Value> },
    TreeMerge { receiver: Box<Value> },
    TreeSubtree { receiver: Box<Value> },
    OptionUnwrapOr { receiver: Box<Value> },
    FallibleCall { receiver: Box<Value> },
    HostRead { receiver: Box<Value> },
    HostHash { receiver: Box<Value> },
    HostWhich { receiver: Box<Value> },
}

#[derive(Debug, Clone)]
pub enum BuiltinValue {
    Namespace(BuiltinNamespace),
    Function(BuiltinFunction),
    Method(BuiltinMethod),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Pending(_) => "Pending",
            Value::U64(_) => "U64",
            Value::I64(_) => "I64",
            Value::F64(_) => "F64",
            Value::Bool(_) => "Bool",
            Value::String(_) => "String",
            Value::Path(_) => "Path",
            Value::ExecPath(_) => "ExecPath",
            Value::Blob(_) => "Blob",
            Value::Bytes(_) => "Bytes",
            Value::Tree(_) => "Tree",
            Value::Executor(_) => "ExecutorClass",
            Value::Host(_) => "Host",
            Value::Discovered(discovered) => discovered.value.type_name(),
            Value::Unit => "()",
            Value::Tuple(_) => "Tuple",
            Value::Array(_) => "Array",
            Value::Map(_) => "Map",
            Value::Struct(_) => "Struct",
            Value::Function(_) => "Fn",
            Value::Partial(_) => "Partial",
            Value::Builtin(_) => "Builtin",
        }
    }

    pub fn collect_pending(&self, deps: &mut Vec<NodeId>) {
        match self {
            Value::Pending(node_id) => deps.push(*node_id),
            Value::ExecPath(path) => path.collect_pending(deps),
            Value::Discovered(discovered) => discovered.value.collect_pending(deps),
            Value::Tuple(elems) | Value::Array(elems) => {
                for elem in elems.iter() {
                    elem.collect_pending(deps);
                }
            }
            Value::Map(map) => {
                for (key, value) in map.entries.iter() {
                    key.collect_pending(deps);
                    value.collect_pending(deps);
                }
            }
            Value::Tree(tree) => tree.collect_pending(deps),
            Value::Struct(struct_value) => {
                for (_, value) in struct_value.fields.iter() {
                    value.collect_pending(deps);
                }
            }
            Value::Function(function) => function.collect_pending(deps),
            Value::Partial(partial) => {
                partial.function.collect_pending(deps);
                for arg in partial.args.iter() {
                    arg.collect_pending(deps);
                }
                for (_, value) in partial.kwargs.iter() {
                    value.collect_pending(deps);
                }
                if let Some(kwargs_expr) = &partial.kwargs_expr {
                    kwargs_expr.collect_pending(deps);
                }
            }
            Value::Builtin(builtin) => builtin.collect_pending(deps),
            Value::U64(_)
            | Value::I64(_)
            | Value::F64(_)
            | Value::Bool(_)
            | Value::String(_)
            | Value::Path(_)
            | Value::Blob(_)
            | Value::Bytes(_)
            | Value::Executor(_)
            | Value::Host(_)
            | Value::Unit => {}
        }
    }

    pub fn collect_pending_shallow(&self, deps: &mut Vec<NodeId>) {
        match self {
            Value::Pending(node_id) => deps.push(*node_id),
            Value::Discovered(discovered) => discovered.value.collect_pending_shallow(deps),
            _ => {}
        }
    }

    pub fn is_fully_resolved(&self) -> bool {
        match self {
            Value::Pending(_) => false,
            Value::Tuple(elems) => elems.iter().all(Self::is_fully_resolved),
            Value::Array(items) => items.iter().all(Self::is_fully_resolved),
            Value::Map(map) => map
                .entries
                .iter()
                .all(|(key, value)| key.is_fully_resolved() && value.is_fully_resolved()),
            Value::ExecPath(path) => path.is_fully_resolved(),
            Value::Tree(tree) => tree.is_fully_resolved(),
            Value::Discovered(discovered) => discovered.value.is_fully_resolved(),
            Value::Struct(s) => s.fields.iter().all(|(_, value)| value.is_fully_resolved()),
            Value::Function(f) => f
                .captures
                .iter()
                .all(|(_, value)| value.is_fully_resolved()),
            Value::Partial(partial) => {
                partial.function.is_fully_resolved()
                    && partial.args.iter().all(Value::is_fully_resolved)
                    && partial
                        .kwargs
                        .iter()
                        .all(|(_, value)| value.is_fully_resolved())
                    && partial
                        .kwargs_expr
                        .as_ref()
                        .is_none_or(|value| value.is_fully_resolved())
            }
            Value::Builtin(builtin) => builtin.is_fully_resolved(),
            _ => true,
        }
    }

    pub fn key(&self) -> Option<String> {
        match self {
            Value::Pending(_) => None,
            Value::U64(n) => Some(format!("u64:{n}")),
            Value::I64(n) => Some(format!("i64:{n}")),
            Value::F64(n) => Some(format!("f64:{}", float_key(*n))),
            Value::Bool(b) => Some(format!("bool:{b}")),
            Value::String(s) => Some(format!("string:{s:?}")),
            Value::Path(path) => Some(path.key()),
            Value::ExecPath(path) => path.key(),
            Value::Blob(blob) => blob.key(),
            Value::Bytes(bytes) => Some(format!("bytes:{}", hex_bytes(bytes))),
            Value::Tree(tree) => tree.key(),
            Value::Executor(selector) => Some(format!(
                "executor:{}:{}",
                selector.backend.as_tag(),
                selector.id
            )),
            Value::Host(_) => None,
            Value::Discovered(discovered) => {
                let obligations = discovered
                    .obligations
                    .iter()
                    .map(HostObservation::key)
                    .collect::<Vec<_>>()
                    .join(",");
                Some(format!(
                    "discovered:{}:[{}]",
                    discovered.value.key()?,
                    obligations
                ))
            }
            Value::Unit => Some("unit".into()),
            Value::Tuple(elems) => {
                let parts = elems.iter().map(Value::key).collect::<Option<Vec<_>>>()?;
                Some(format!("tuple:[{}]", parts.join(",")))
            }
            Value::Array(items) => {
                let parts = items.iter().map(Value::key).collect::<Option<Vec<_>>>()?;
                Some(format!("array:[{}]", parts.join(",")))
            }
            Value::Map(map) => map.key(),
            Value::Struct(s) => {
                let parts = s
                    .fields
                    .iter()
                    .map(|(name, value)| Some(format!("{name}:{}", value.key()?)))
                    .collect::<Option<Vec<_>>>()?;
                Some(format!("struct:{}{{{}}}", s.type_name, parts.join(",")))
            }
            Value::Function(function) => Some(function.key()?),
            Value::Partial(_) => None,
            Value::Builtin(builtin) => builtin.key(),
        }
    }
}

impl HostObservation {
    pub fn key(&self) -> String {
        match self {
            Self::Which {
                name,
                expected_path,
            } => match expected_path {
                Some(path) => format!("which:{name}:{path}"),
                None => format!("which:{name}:<missing>"),
            },
            Self::Read { path, content_hash } => format!("read:{path}:{content_hash}"),
            Self::Hash { path, content_hash } => format!("hash:{path}:{content_hash}"),
        }
    }
}

impl MapValue {
    pub fn key(&self) -> Option<String> {
        let parts = self
            .entries
            .iter()
            .map(|(key, value)| Some(format!("{}=>{}", key.key()?, value.key()?)))
            .collect::<Option<Vec<_>>>()?;
        Some(format!("map:[{}]", parts.join(",")))
    }
}

impl PathValue {
    pub fn new(path: impl AsRef<str>) -> Self {
        Self {
            raw: Arc::from(normalize_path(path.as_ref())),
        }
    }

    pub fn as_str(&self) -> &str {
        self.raw.as_ref()
    }

    pub fn join(&self, child: &str) -> Self {
        Self::new(join_paths(self.as_str(), child))
    }

    pub fn key(&self) -> String {
        format!("path:{}", self.as_str())
    }
}

impl ExecPathValue {
    fn collect_pending(&self, deps: &mut Vec<NodeId>) {
        match self {
            ExecPathValue::InTree { tree, .. } => tree.collect_pending(deps),
            ExecPathValue::InBlob { .. } => {}
        }
    }

    pub fn is_fully_resolved(&self) -> bool {
        match self {
            ExecPathValue::InTree { tree, .. } => tree.is_fully_resolved(),
            ExecPathValue::InBlob { .. } => true,
        }
    }

    pub fn key(&self) -> Option<String> {
        match self {
            ExecPathValue::InTree { tree, suffix } => {
                Some(format!("execpath:tree:{}:{}", tree.key()?, suffix.as_str()))
            }
            ExecPathValue::InBlob { blob, leaf } => {
                Some(format!("execpath:blob:{}:{}", blob.key()?, leaf.as_str()))
            }
        }
    }
}

impl BlobValue {
    pub fn from_string(value: impl AsRef<str>) -> Self {
        Self::Inline {
            bytes: Arc::from(value.as_ref().as_bytes()),
            executable: false,
        }
    }

    pub fn executable(&self) -> bool {
        match self {
            BlobValue::Inline { executable, .. }
            | BlobValue::FsFile { executable, .. }
            | BlobValue::Store { executable, .. } => *executable,
        }
    }

    // r[impl store.blob.utf8]
    pub fn to_utf8_string(&self) -> Result<Arc<str>, String> {
        let bytes = match self {
            BlobValue::Inline { bytes, .. } => bytes.to_vec(),
            BlobValue::FsFile { path, .. } => fs::read(path.as_str())
                .map_err(|error| format!("failed to read {}: {error}", path.as_str()))?,
            BlobValue::Store { blob_ref, .. } => {
                return Err(format!(
                    "store-backed blob {} requires an external store resolver",
                    blob_ref_key(blob_ref)
                ));
            }
        };
        let text =
            String::from_utf8(bytes).map_err(|error| format!("blob is not UTF-8: {error}"))?;
        Ok(Arc::from(text))
    }

    pub fn key(&self) -> Option<String> {
        match self {
            BlobValue::Inline { bytes, executable } => {
                Some(format!("blob:{executable}:{}", hex_bytes(bytes)))
            }
            BlobValue::FsFile { .. } => None,
            BlobValue::Store {
                blob_ref,
                executable,
            } => Some(format!("blob:{executable}:{}", blob_ref_key(blob_ref))),
        }
    }
}

impl TreeValue {
    fn collect_pending(&self, deps: &mut Vec<NodeId>) {
        match self {
            TreeValue::Inline(entries) => {
                for (_, value) in entries.iter() {
                    value.collect_pending(deps);
                }
            }
            TreeValue::FsDir(_) | TreeValue::Store(_) => {}
        }
    }

    pub fn is_fully_resolved(&self) -> bool {
        match self {
            TreeValue::Inline(entries) => {
                entries.iter().all(|(_, value)| value.is_fully_resolved())
            }
            TreeValue::FsDir(_) => true,
            TreeValue::Store(_) => true,
        }
    }

    pub fn key(&self) -> Option<String> {
        match self {
            TreeValue::Inline(entries) => {
                let parts = entries
                    .iter()
                    .map(|(name, value)| Some(format!("{name}:{}", value.key()?)))
                    .collect::<Option<Vec<_>>>()?;
                Some(format!("tree:{{{}}}", parts.join(",")))
            }
            TreeValue::FsDir(_) => None,
            TreeValue::Store(root) => Some(format!("tree:store:{root}")),
        }
    }
}

impl StructValue {
    pub fn key(&self) -> Option<String> {
        Value::Struct(self.clone()).key()
    }
}

impl FunctionValue {
    pub fn identity(&self) -> String {
        self.code_identity.to_string()
    }

    fn collect_pending(&self, deps: &mut Vec<NodeId>) {
        for (_, value) in self.captures.iter() {
            value.collect_pending(deps);
        }
    }

    pub fn key(&self) -> Option<String> {
        let captures = self
            .captures
            .iter()
            .map(|(name, value)| Some(format!("{name}:{}", value.key()?)))
            .collect::<Option<Vec<_>>>()?;
        Some(format!("fn:{}[{}]", self.identity(), captures.join(",")))
    }
}

impl BuiltinValue {
    fn collect_pending(&self, deps: &mut Vec<NodeId>) {
        match self {
            BuiltinValue::Namespace(_)
            | BuiltinValue::Function(BuiltinFunction::Acquire)
            | BuiltinValue::Function(BuiltinFunction::Stabilize)
            | BuiltinValue::Function(BuiltinFunction::Render)
            | BuiltinValue::Function(BuiltinFunction::Exec)
            | BuiltinValue::Function(BuiltinFunction::Discover)
            | BuiltinValue::Function(BuiltinFunction::ExecutorClass)
            | BuiltinValue::Function(BuiltinFunction::StructConstructor { .. })
            | BuiltinValue::Method(BuiltinMethod::BlobFromString)
            | BuiltinValue::Method(BuiltinMethod::BytesFromString)
            | BuiltinValue::Method(BuiltinMethod::TreeOf)
            | BuiltinValue::Method(BuiltinMethod::TreeEmpty)
            | BuiltinValue::Method(BuiltinMethod::TreeFromPath) => {}
            BuiltinValue::Method(BuiltinMethod::StringStartsWith { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringContains { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringSplit { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringLines { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringTrim { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringToLower { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayMap { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayToMap { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver })
            | BuiltinValue::Method(BuiltinMethod::MapMapValues { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobToString { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobExecutable { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobUtf8 { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { receiver })
            | BuiltinValue::Method(BuiltinMethod::BytesUtf8 { receiver })
            | BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { receiver })
            | BuiltinValue::Method(BuiltinMethod::BytesBlob { receiver })
            | BuiltinValue::Method(BuiltinMethod::StreamCollect { receiver })
            | BuiltinValue::Method(BuiltinMethod::TreeFile { receiver })
            | BuiltinValue::Method(BuiltinMethod::TreeMerge { receiver })
            | BuiltinValue::Method(BuiltinMethod::TreeSubtree { receiver })
            | BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver })
            | BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver })
            | BuiltinValue::Method(BuiltinMethod::HostRead { receiver })
            | BuiltinValue::Method(BuiltinMethod::HostHash { receiver })
            | BuiltinValue::Method(BuiltinMethod::HostWhich { receiver }) => {
                receiver.collect_pending(deps);
            }
        }
    }

    pub fn is_fully_resolved(&self) -> bool {
        match self {
            BuiltinValue::Namespace(_)
            | BuiltinValue::Function(BuiltinFunction::Acquire)
            | BuiltinValue::Function(BuiltinFunction::Stabilize)
            | BuiltinValue::Function(BuiltinFunction::Render)
            | BuiltinValue::Function(BuiltinFunction::Exec)
            | BuiltinValue::Function(BuiltinFunction::Discover)
            | BuiltinValue::Function(BuiltinFunction::ExecutorClass)
            | BuiltinValue::Function(BuiltinFunction::StructConstructor { .. })
            | BuiltinValue::Method(BuiltinMethod::BlobFromString)
            | BuiltinValue::Method(BuiltinMethod::BytesFromString)
            | BuiltinValue::Method(BuiltinMethod::TreeOf)
            | BuiltinValue::Method(BuiltinMethod::TreeEmpty)
            | BuiltinValue::Method(BuiltinMethod::TreeFromPath) => true,
            BuiltinValue::Method(BuiltinMethod::StringStartsWith { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringContains { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringSplit { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringLines { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringTrim { receiver })
            | BuiltinValue::Method(BuiltinMethod::StringToLower { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayMap { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayToMap { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver })
            | BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver })
            | BuiltinValue::Method(BuiltinMethod::MapMapValues { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobToString { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobExecutable { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobUtf8 { receiver })
            | BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { receiver })
            | BuiltinValue::Method(BuiltinMethod::BytesUtf8 { receiver })
            | BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { receiver })
            | BuiltinValue::Method(BuiltinMethod::BytesBlob { receiver })
            | BuiltinValue::Method(BuiltinMethod::StreamCollect { receiver })
            | BuiltinValue::Method(BuiltinMethod::TreeFile { receiver })
            | BuiltinValue::Method(BuiltinMethod::TreeMerge { receiver })
            | BuiltinValue::Method(BuiltinMethod::TreeSubtree { receiver })
            | BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver })
            | BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver })
            | BuiltinValue::Method(BuiltinMethod::HostRead { receiver })
            | BuiltinValue::Method(BuiltinMethod::HostHash { receiver })
            | BuiltinValue::Method(BuiltinMethod::HostWhich { receiver }) => {
                receiver.is_fully_resolved()
            }
        }
    }

    pub fn key(&self) -> Option<String> {
        match self {
            BuiltinValue::Namespace(BuiltinNamespace::Blob) => Some("builtin:Blob".into()),
            BuiltinValue::Namespace(BuiltinNamespace::Bytes) => Some("builtin:Bytes".into()),
            BuiltinValue::Namespace(BuiltinNamespace::Tree) => Some("builtin:Tree".into()),
            BuiltinValue::Function(BuiltinFunction::Acquire) => Some("builtin:acquire".into()),
            BuiltinValue::Function(BuiltinFunction::Stabilize) => Some("builtin:stabilize".into()),
            BuiltinValue::Function(BuiltinFunction::Render) => Some("builtin:render".into()),
            BuiltinValue::Function(BuiltinFunction::Exec) => Some("builtin:exec".into()),
            BuiltinValue::Function(BuiltinFunction::Discover) => Some("builtin:discover".into()),
            BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                Some("builtin:executor_class".into())
            }
            BuiltinValue::Function(BuiltinFunction::StructConstructor { type_name, fields }) => {
                Some(format!(
                    "builtin:struct_ctor:{}({})",
                    type_name,
                    fields
                        .iter()
                        .map(|field| field.as_ref())
                        .collect::<Vec<_>>()
                        .join(",")
                ))
            }
            BuiltinValue::Method(BuiltinMethod::StringStartsWith { receiver }) => {
                Some(format!("builtin:string.starts_with({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::StringContains { receiver }) => {
                Some(format!("builtin:string.contains({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::StringSplit { receiver }) => {
                Some(format!("builtin:string.split({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::StringLines { receiver }) => {
                Some(format!("builtin:string.lines({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::StringTrim { receiver }) => {
                Some(format!("builtin:string.trim({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::StringToLower { receiver }) => {
                Some(format!("builtin:string.to_lower({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BlobFromString) => {
                Some("builtin:Blob.from_string".into())
            }
            BuiltinValue::Method(BuiltinMethod::BytesFromString) => Some("builtin:Bytes".into()),
            BuiltinValue::Method(BuiltinMethod::TreeOf) => Some("builtin:Tree.of".into()),
            BuiltinValue::Method(BuiltinMethod::TreeEmpty) => Some("builtin:Tree.empty".into()),
            BuiltinValue::Method(BuiltinMethod::TreeFromPath) => {
                Some("builtin:Tree.from_path".into())
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMap { receiver }) => {
                Some(format!("builtin:array.map({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::ArrayToMap { receiver }) => {
                Some(format!("builtin:array.to_map({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver }) => {
                Some(format!("builtin:array.flatten({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver }) => {
                Some(format!("builtin:array.merge({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::MapMapValues { receiver }) => {
                Some(format!("builtin:map.map_values({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BlobToString { receiver }) => {
                Some(format!("builtin:blob.to_string({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BlobExecutable { receiver }) => {
                Some(format!("builtin:blob.executable({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8 { receiver }) => {
                Some(format!("builtin:blob.utf8({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { receiver }) => {
                Some(format!("builtin:blob.utf8_lossy({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8 { receiver }) => {
                Some(format!("builtin:bytes.utf8({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { receiver }) => {
                Some(format!("builtin:bytes.utf8_lossy({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::BytesBlob { receiver }) => {
                Some(format!("builtin:bytes.blob({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::StreamCollect { receiver }) => {
                Some(format!("builtin:stream.collect({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::TreeFile { receiver }) => {
                Some(format!("builtin:tree.file({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::TreeMerge { receiver }) => {
                Some(format!("builtin:tree.merge({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::TreeSubtree { receiver }) => {
                Some(format!("builtin:tree.subtree({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver }) => {
                Some(format!("builtin:option.unwrap_or({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver }) => {
                Some(format!("builtin:fallible({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::HostRead { receiver }) => {
                Some(format!("builtin:host.read({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::HostHash { receiver }) => {
                Some(format!("builtin:host.hash({})", receiver.key()?))
            }
            BuiltinValue::Method(BuiltinMethod::HostWhich { receiver }) => {
                Some(format!("builtin:host.which({})", receiver.key()?))
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Pending(node_id) => write!(f, "<pending {}>", node_id.0),
            Value::U64(n) => write!(f, "{n}"),
            Value::I64(n) => write!(f, "{n}"),
            Value::F64(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "{s}"),
            Value::Path(path) => write!(f, "{}", path.as_str()),
            Value::ExecPath(path) => write!(f, "{path}"),
            Value::Blob(blob) => write!(f, "{blob}"),
            Value::Bytes(bytes) => write!(f, "<bytes {} bytes>", bytes.len()),
            Value::Tree(tree) => write!(f, "{tree}"),
            Value::Executor(selector) => write!(f, "{}", selector.id),
            Value::Host(_) => write!(f, "<host>"),
            Value::Discovered(discovered) => write!(f, "{}", discovered.value),
            Value::Unit => write!(f, "()"),
            Value::Tuple(elems) => {
                write!(f, "(")?;
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{elem}")?;
                }
                if elems.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")
            }
            Value::Array(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Map(map) => {
                write!(f, "%{{")?;
                for (i, (key, value)) in map.entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{key} => {value}")?;
                }
                write!(f, "}}")
            }
            Value::Struct(s) => {
                write!(f, "{} {{ ", s.type_name)?;
                for (i, (name, val)) in s.fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{name}: {val}")?;
                }
                write!(f, " }}")
            }
            Value::Function(func) => write!(f, "<fn {}>", func.def.name),
            Value::Partial(_) => write!(f, "<partial>"),
            Value::Builtin(builtin) => write!(f, "{builtin}"),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Discovered(a), Value::Discovered(b)) => a.value == b.value,
            (Value::Discovered(a), other) => a.value.as_ref() == other,
            (other, Value::Discovered(b)) => other == b.value.as_ref(),
            (Value::Pending(a), Value::Pending(b)) => a == b,
            (Value::U64(a), Value::U64(b)) => a == b,
            (Value::I64(a), Value::I64(b)) => a == b,
            (Value::F64(a), Value::F64(b)) => float_eq(*a, *b),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Path(a), Value::Path(b)) => a == b,
            (Value::ExecPath(a), Value::ExecPath(b)) => a == b,
            (Value::Blob(a), Value::Blob(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::Tree(a), Value::Tree(b)) => a == b,
            (Value::Executor(a), Value::Executor(b)) => a == b,
            (Value::Host(a), Value::Host(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Struct(a), Value::Struct(b)) => {
                a.type_name == b.type_name && a.fields == b.fields
            }
            (Value::Function(a), Value::Function(b)) => {
                a.identity() == b.identity() && a.captures == b.captures
            }
            (Value::Partial(a), Value::Partial(b)) => {
                a.function == b.function
                    && a.args == b.args
                    && a.kwargs == b.kwargs
                    && a.kwargs_expr == b.kwargs_expr
            }
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl ExecutorBackendKind {
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "class" => Some(Self::Class),
            "local" => Some(Self::Local),
            "ssh_bootstrap" => Some(Self::SshBootstrap),
            "daemon" => Some(Self::Daemon),
            _ => None,
        }
    }

    pub fn as_tag(&self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Local => "local",
            Self::SshBootstrap => "ssh_bootstrap",
            Self::Daemon => "daemon",
        }
    }
}

impl ExecutorSelector {
    pub fn class(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            backend: ExecutorBackendKind::Class,
        }
    }
}

impl PartialEq for ExecPathValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                ExecPathValue::InTree {
                    tree: left_tree,
                    suffix: left_suffix,
                },
                ExecPathValue::InTree {
                    tree: right_tree,
                    suffix: right_suffix,
                },
            ) => left_tree == right_tree && left_suffix == right_suffix,
            (
                ExecPathValue::InBlob {
                    blob: left_blob,
                    leaf: left_leaf,
                },
                ExecPathValue::InBlob {
                    blob: right_blob,
                    leaf: right_leaf,
                },
            ) => left_blob == right_blob && left_leaf == right_leaf,
            _ => false,
        }
    }
}

impl Eq for ExecPathValue {}

impl PartialEq for MapValue {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for MapValue {}

impl PartialEq for BlobValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                BlobValue::Inline {
                    bytes: a_bytes,
                    executable: a_exec,
                },
                BlobValue::Inline {
                    bytes: b_bytes,
                    executable: b_exec,
                },
            ) => a_exec == b_exec && a_bytes == b_bytes,
            (
                BlobValue::FsFile {
                    path: a_path,
                    executable: a_exec,
                },
                BlobValue::FsFile {
                    path: b_path,
                    executable: b_exec,
                },
            ) => a_exec == b_exec && a_path == b_path,
            (
                BlobValue::Store {
                    blob_ref: a_ref,
                    executable: a_exec,
                },
                BlobValue::Store {
                    blob_ref: b_ref,
                    executable: b_exec,
                },
            ) => a_exec == b_exec && blob_ref_eq(a_ref, b_ref),
            _ => false,
        }
    }
}

impl Eq for BlobValue {}

impl PartialEq for TreeValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TreeValue::Inline(a), TreeValue::Inline(b)) => a == b,
            (TreeValue::FsDir(a), TreeValue::FsDir(b)) => a == b,
            (TreeValue::Store(a), TreeValue::Store(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for TreeValue {}

impl PartialEq for BuiltinMethod {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BuiltinMethod::BlobFromString, BuiltinMethod::BlobFromString)
            | (BuiltinMethod::BytesFromString, BuiltinMethod::BytesFromString)
            | (BuiltinMethod::TreeOf, BuiltinMethod::TreeOf)
            | (BuiltinMethod::TreeEmpty, BuiltinMethod::TreeEmpty)
            | (BuiltinMethod::TreeFromPath, BuiltinMethod::TreeFromPath) => true,
            (
                BuiltinMethod::StringStartsWith { receiver: left },
                BuiltinMethod::StringStartsWith { receiver: right },
            )
            | (
                BuiltinMethod::StringContains { receiver: left },
                BuiltinMethod::StringContains { receiver: right },
            )
            | (
                BuiltinMethod::StringSplit { receiver: left },
                BuiltinMethod::StringSplit { receiver: right },
            )
            | (
                BuiltinMethod::StringLines { receiver: left },
                BuiltinMethod::StringLines { receiver: right },
            )
            | (
                BuiltinMethod::StringTrim { receiver: left },
                BuiltinMethod::StringTrim { receiver: right },
            )
            | (
                BuiltinMethod::StringToLower { receiver: left },
                BuiltinMethod::StringToLower { receiver: right },
            )
            | (
                BuiltinMethod::ArrayMap { receiver: left },
                BuiltinMethod::ArrayMap { receiver: right },
            )
            | (
                BuiltinMethod::ArrayToMap { receiver: left },
                BuiltinMethod::ArrayToMap { receiver: right },
            )
            | (
                BuiltinMethod::ArrayFlatten { receiver: left },
                BuiltinMethod::ArrayFlatten { receiver: right },
            )
            | (
                BuiltinMethod::ArrayMerge { receiver: left },
                BuiltinMethod::ArrayMerge { receiver: right },
            )
            | (
                BuiltinMethod::MapMapValues { receiver: left },
                BuiltinMethod::MapMapValues { receiver: right },
            )
            | (
                BuiltinMethod::BlobToString { receiver: left },
                BuiltinMethod::BlobToString { receiver: right },
            )
            | (
                BuiltinMethod::BlobExecutable { receiver: left },
                BuiltinMethod::BlobExecutable { receiver: right },
            )
            | (
                BuiltinMethod::BlobUtf8 { receiver: left },
                BuiltinMethod::BlobUtf8 { receiver: right },
            )
            | (
                BuiltinMethod::BlobUtf8Lossy { receiver: left },
                BuiltinMethod::BlobUtf8Lossy { receiver: right },
            )
            | (
                BuiltinMethod::BytesUtf8 { receiver: left },
                BuiltinMethod::BytesUtf8 { receiver: right },
            )
            | (
                BuiltinMethod::BytesUtf8Lossy { receiver: left },
                BuiltinMethod::BytesUtf8Lossy { receiver: right },
            )
            | (
                BuiltinMethod::BytesBlob { receiver: left },
                BuiltinMethod::BytesBlob { receiver: right },
            )
            | (
                BuiltinMethod::StreamCollect { receiver: left },
                BuiltinMethod::StreamCollect { receiver: right },
            )
            | (
                BuiltinMethod::TreeFile { receiver: left },
                BuiltinMethod::TreeFile { receiver: right },
            )
            | (
                BuiltinMethod::TreeMerge { receiver: left },
                BuiltinMethod::TreeMerge { receiver: right },
            )
            | (
                BuiltinMethod::TreeSubtree { receiver: left },
                BuiltinMethod::TreeSubtree { receiver: right },
            )
            | (
                BuiltinMethod::OptionUnwrapOr { receiver: left },
                BuiltinMethod::OptionUnwrapOr { receiver: right },
            )
            | (
                BuiltinMethod::FallibleCall { receiver: left },
                BuiltinMethod::FallibleCall { receiver: right },
            )
            | (
                BuiltinMethod::HostRead { receiver: left },
                BuiltinMethod::HostRead { receiver: right },
            )
            | (
                BuiltinMethod::HostHash { receiver: left },
                BuiltinMethod::HostHash { receiver: right },
            )
            | (
                BuiltinMethod::HostWhich { receiver: left },
                BuiltinMethod::HostWhich { receiver: right },
            ) => left == right,
            _ => false,
        }
    }
}

impl Eq for BuiltinMethod {}

impl PartialEq for BuiltinValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BuiltinValue::Namespace(a), BuiltinValue::Namespace(b)) => a == b,
            (BuiltinValue::Function(a), BuiltinValue::Function(b)) => a == b,
            (BuiltinValue::Method(a), BuiltinValue::Method(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for BuiltinValue {}

impl fmt::Display for BlobValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlobValue::Inline { bytes, executable } => {
                let mode = if *executable { "x" } else { "-" };
                write!(f, "<blob {mode} {} bytes>", bytes.len())
            }
            BlobValue::FsFile { path, executable } => {
                let mode = if *executable { "x" } else { "-" };
                write!(f, "<blob {mode} {}>", path.as_str())
            }
            BlobValue::Store {
                blob_ref,
                executable,
            } => {
                let mode = if *executable { "x" } else { "-" };
                write!(f, "<blob {mode} {}>", blob_ref_key(blob_ref))
            }
        }
    }
}

impl fmt::Display for TreeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeValue::Inline(entries) => write!(f, "<tree {} entries>", entries.len()),
            TreeValue::FsDir(path) => write!(f, "<tree {}>", path.as_str()),
            TreeValue::Store(root) => write!(f, "<tree {root}>"),
        }
    }
}

impl fmt::Display for ExecPathValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecPathValue::InTree { suffix, .. } => write!(f, "{}", suffix.as_str()),
            ExecPathValue::InBlob { leaf, .. } => write!(f, "{}", leaf.as_str()),
        }
    }
}

impl fmt::Display for BuiltinValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuiltinValue::Namespace(BuiltinNamespace::Blob) => write!(f, "Blob"),
            BuiltinValue::Namespace(BuiltinNamespace::Bytes) => write!(f, "Bytes"),
            BuiltinValue::Namespace(BuiltinNamespace::Tree) => write!(f, "Tree"),
            BuiltinValue::Function(BuiltinFunction::Acquire) => write!(f, "acquire"),
            BuiltinValue::Function(BuiltinFunction::Stabilize) => write!(f, "stabilize"),
            BuiltinValue::Function(BuiltinFunction::Render) => write!(f, "render"),
            BuiltinValue::Function(BuiltinFunction::Exec) => write!(f, "exec"),
            BuiltinValue::Function(BuiltinFunction::Discover) => write!(f, "discover"),
            BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                write!(f, "executor_class")
            }
            BuiltinValue::Function(BuiltinFunction::StructConstructor { type_name, .. }) => {
                write!(f, "{type_name}")
            }
            BuiltinValue::Method(BuiltinMethod::StringStartsWith { .. }) => {
                write!(f, "<bound string.starts_with>")
            }
            BuiltinValue::Method(BuiltinMethod::StringContains { .. }) => {
                write!(f, "<bound string.contains>")
            }
            BuiltinValue::Method(BuiltinMethod::StringSplit { .. }) => {
                write!(f, "<bound string.split>")
            }
            BuiltinValue::Method(BuiltinMethod::StringLines { .. }) => {
                write!(f, "<bound string.lines>")
            }
            BuiltinValue::Method(BuiltinMethod::StringTrim { .. }) => {
                write!(f, "<bound string.trim>")
            }
            BuiltinValue::Method(BuiltinMethod::StringToLower { .. }) => {
                write!(f, "<bound string.to_lower>")
            }
            BuiltinValue::Method(BuiltinMethod::BlobFromString) => write!(f, "Blob.from_string"),
            BuiltinValue::Method(BuiltinMethod::BytesFromString) => write!(f, "Bytes"),
            BuiltinValue::Method(BuiltinMethod::TreeOf) => write!(f, "Tree.of"),
            BuiltinValue::Method(BuiltinMethod::TreeEmpty) => write!(f, "Tree.empty"),
            BuiltinValue::Method(BuiltinMethod::TreeFromPath) => write!(f, "Tree.from_path"),
            BuiltinValue::Method(BuiltinMethod::ArrayMap { .. }) => {
                write!(f, "<bound array.map>")
            }
            BuiltinValue::Method(BuiltinMethod::ArrayToMap { .. }) => {
                write!(f, "<bound array.to_map>")
            }
            BuiltinValue::Method(BuiltinMethod::ArrayFlatten { .. }) => {
                write!(f, "<bound array.flatten>")
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMerge { .. }) => {
                write!(f, "<bound array.merge>")
            }
            BuiltinValue::Method(BuiltinMethod::MapMapValues { .. }) => {
                write!(f, "<bound map.map_values>")
            }
            BuiltinValue::Method(BuiltinMethod::BlobToString { .. }) => {
                write!(f, "<bound blob.to_string>")
            }
            BuiltinValue::Method(BuiltinMethod::BlobExecutable { .. }) => {
                write!(f, "<bound blob.executable>")
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8 { .. }) => {
                write!(f, "<bound blob.utf8>")
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { .. }) => {
                write!(f, "<bound blob.utf8_lossy>")
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8 { .. }) => {
                write!(f, "<bound bytes.utf8>")
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { .. }) => {
                write!(f, "<bound bytes.utf8_lossy>")
            }
            BuiltinValue::Method(BuiltinMethod::BytesBlob { .. }) => {
                write!(f, "<bound bytes.blob>")
            }
            BuiltinValue::Method(BuiltinMethod::StreamCollect { .. }) => {
                write!(f, "<bound stream.collect>")
            }
            BuiltinValue::Method(BuiltinMethod::TreeFile { .. }) => {
                write!(f, "<bound tree.file>")
            }
            BuiltinValue::Method(BuiltinMethod::TreeMerge { .. }) => {
                write!(f, "<bound tree.merge>")
            }
            BuiltinValue::Method(BuiltinMethod::TreeSubtree { .. }) => {
                write!(f, "<bound tree.subtree>")
            }
            BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { .. }) => {
                write!(f, "<bound option.unwrap_or>")
            }
            BuiltinValue::Method(BuiltinMethod::FallibleCall { .. }) => {
                write!(f, "<bound fallible>")
            }
            BuiltinValue::Method(BuiltinMethod::HostRead { .. }) => {
                write!(f, "<bound host.read>")
            }
            BuiltinValue::Method(BuiltinMethod::HostHash { .. }) => {
                write!(f, "<bound host.hash>")
            }
            BuiltinValue::Method(BuiltinMethod::HostWhich { .. }) => {
                write!(f, "<bound host.which>")
            }
        }
    }
}

fn float_eq(left: f64, right: f64) -> bool {
    (left.is_nan() && right.is_nan()) || left == right
}

fn float_key(value: f64) -> String {
    if value.is_nan() {
        "nan".into()
    } else if value == 0.0 {
        "0".into()
    } else {
        value.to_bits().to_string()
    }
}

fn normalize_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let is_absolute = path.starts_with('/');
    let mut parts = Vec::new();

    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|segment: &&str| *segment != "..") {
                    parts.pop();
                } else if !is_absolute {
                    parts.push("..");
                }
            }
            _ => parts.push(part),
        }
    }

    let joined = parts.join("/");
    if is_absolute {
        if joined.is_empty() {
            "/".into()
        } else {
            format!("/{joined}")
        }
    } else if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

fn join_paths(base: &str, child: &str) -> String {
    if child.starts_with('/') {
        return normalize_path(child);
    }
    let base = normalize_path(base);
    if base == "/" {
        return normalize_path(&format!("/{child}"));
    }
    if base == "." {
        return normalize_path(child);
    }
    normalize_path(&format!("{base}/{child}"))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn blob_ref_key(blob_ref: &BlobRef) -> String {
    match blob_ref {
        BlobRef::Small { chunk, size } => format!("blob:store:small:{chunk}:{size}"),
        BlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => format!("blob:store:large:{blob_node}:{content_hash}:{size}"),
    }
}

fn blob_ref_eq(left: &BlobRef, right: &BlobRef) -> bool {
    match (left, right) {
        (
            BlobRef::Small {
                chunk: left_chunk,
                size: left_size,
            },
            BlobRef::Small {
                chunk: right_chunk,
                size: right_size,
            },
        ) => left_chunk == right_chunk && left_size == right_size,
        (
            BlobRef::Large {
                blob_node: left_blob_node,
                content_hash: left_content_hash,
                size: left_size,
            },
            BlobRef::Large {
                blob_node: right_blob_node,
                content_hash: right_content_hash,
                size: right_size,
            },
        ) => {
            left_blob_node == right_blob_node
                && left_content_hash == right_content_hash
                && left_size == right_size
        }
        _ => false,
    }
}
