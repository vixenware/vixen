//! Exec/observe Vox protocol shared by the orchestrator, runtime boundary
//! materializer, and runner.
//!
//! This crate is only the wire contract. The graph evaluator may construct an
//! `ObserveRequest` after demand reaches an exec boundary, and the runner may
//! answer it, but process execution, sandbox policy, CAS-VFS mounts, and observer
//! evaluation live in their owning crates.

use vx_ir::Bundle;

/// A command to run — minimal for now (program + args). The production payload
/// grows from here toward structured `Command`, env, mounts, sandbox obligations,
/// and declared output roots.
#[derive(facet::Facet, Debug, Clone)]
pub struct ExecCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// One environment variable for the executed process.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
pub struct ExecEnv {
    pub key: String,
    pub value: String,
}

/// A content-addressed tree manifest root, encoded as raw BLAKE3 bytes.
#[derive(facet::Facet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExecTreeHash {
    pub bytes: [u8; 32],
}

impl ExecTreeHash {
    pub fn to_hex(self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.bytes {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }
}

/// One content-addressed input tree mount for the executed process.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
pub struct ExecMount {
    pub path: String,
    pub tree: ExecTreeHash,
}

/// What the executor needs to observe: the command to run plus the observer to
/// evaluate against its `Run`. `observer` is `None` for the default observer
/// `run.ok.out`.
#[derive(facet::Facet, Debug, Clone)]
pub struct ObserveRequest {
    pub observer: Option<Bundle>,
    pub command: ExecCommand,
    pub env: Vec<ExecEnv>,
    pub mounts: Vec<ExecMount>,
    pub outputs: Vec<String>,
    pub create_dirs: Vec<String>,
}

/// A runtime value crossing back from the executor to the orchestrator over Vox.
/// It is the observer result, fully forced into data before crossing.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum WireValue {
    U64(u64),
    Bool(bool),
    Str(String),
    Unit,
    Tuple(Vec<WireValue>),
    Array(Vec<WireValue>),
    /// A producing tree returned by the observer. It crosses as a handle; paths
    /// are demanded later through the Producing virtual connection.
    Tree,
    Struct(Vec<(String, WireValue)>),
    Variant {
        tag: String,
        fields: Vec<(String, WireValue)>,
    },
}

impl WireValue {
    pub fn contains_tree(&self) -> bool {
        match self {
            Self::Tree => true,
            Self::Tuple(fields) | Self::Array(fields) => fields.iter().any(Self::contains_tree),
            Self::Struct(fields) => fields.iter().any(|(_, field)| field.contains_tree()),
            Self::Variant { fields, .. } => fields.iter().any(|(_, field)| field.contains_tree()),
            Self::U64(_) | Self::Bool(_) | Self::Str(_) | Self::Unit => false,
        }
    }
}

/// The result of observing: a crossed value or a poison message.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum Observed {
    /// A top-level producing tree returned by the observer. The tree's paths cross
    /// lazily through the `Producing` virtual connection opened on the same observe
    /// session; it is not ordinary eager data.
    Tree,
    /// Ordinary observed data containing one or more producing-tree leaves. Every
    /// `WireValue::Tree` leaf refers to this exec's output tree and crosses through
    /// the same `Producing` virtual connection.
    ValueWithTree(WireValue),
    Value(WireValue),
    Poisoned(String),
}

/// The runner-side observe service: ship a bundle plus command, get the
/// observer's result.
#[vox::service]
pub trait Observe {
    async fn observe(&self, request: ObserveRequest) -> Observed;
}
