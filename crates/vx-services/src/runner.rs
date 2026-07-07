//! Runner service protocol definitions
//!
//! This crate defines the runner RPC surface only. Implementations live in
//! private runtime crates so the protocol can stay publishable.

use std::collections::BTreeMap;

use crate::{ContentHash, NodeHash};
use facet::Facet;

/// Runner protocol version.
/// Bump this when making breaking changes to the Runner RPC interface.
pub const RUNNER_PROTOCOL_VERSION: u32 = 23;

// =============================================================================
// Generic Sandboxed Tool Execution
// =============================================================================

// =============================================================================
// Runner Service Trait
// =============================================================================

#[derive(Facet)]
pub struct DiscoverProbeRequest {
    // TODO: a closure?
}

#[derive(Facet)]
pub struct DiscoverResult {
    // TODO: design
}

/// A mounted or flushed tree. Text entries keep fixtures readable; blob entries
/// carry arbitrary bytes without lossy UTF-8 conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Facet)]
pub struct ExecTree {
    pub entries: BTreeMap<String, String>,
    pub blobs: BTreeMap<String, Vec<u8>>,
}

impl ExecTree {
    pub fn insert_bytes(&mut self, path: impl Into<String>, contents: Vec<u8>) {
        let path = path.into();
        match String::from_utf8(contents) {
            Ok(text) => {
                self.entries.insert(path, text);
            }
            Err(err) => {
                self.blobs.insert(path, err.into_bytes());
            }
        }
    }

    pub fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.entries
            .get(path)
            .map(|contents| contents.as_bytes().to_vec())
            .or_else(|| self.blobs.get(path).cloned())
    }
}

/// One declared mount: a tree mounted at an absolute logical path.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecMount {
    pub at: String,
    pub tree: ExecTree,
}

/// What a command grammar says an argv element is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ExecRole {
    /// A staged executable produced by a prior command.
    Executable,
    /// A path the tool will read.
    Input,
    /// An argv element that contains a path the tool will read.
    InputFlag,
    /// A path the tool will write.
    Output,
    /// An argv element that contains one or more paths the tool will write.
    OutputFlag,
    /// A directory the tool may write with unknown contents.
    OutputDir,
    /// Captured process stdout, stored as an output artifact at this path.
    Stdout,
    /// An environment binding in KEY=VALUE form.
    Env,
    /// A directory the tool will probe.
    SearchDir,
    /// An argv element that contains a directory the tool will probe.
    SearchDirFlag,
    /// Behavior-changing but not path-shaped.
    Flag,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecArg {
    pub value: String,
    pub role: ExecRole,
}

/// A resolved command after command-grammar role assignment.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecPlan {
    pub argv: Vec<ExecArg>,
}

/// A tool execution on a runner. The runner stages declared input mounts under
/// a fresh prefix, runs the process through the daemon's sandbox launch
/// capability, then flushes declared outputs.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecRequest {
    /// The program to execute: an absolute path, or a name resolved on PATH
    /// before entering the sandbox.
    pub program: String,
    /// Role-annotated argv.
    pub plan: ExecPlan,
    /// Declared input mounts.
    pub mounts: Vec<ExecMount>,
    /// Declared roots that may be executed or read as ambient toolchain state.
    pub toolchain_roots: Vec<String>,
}

/// One observed interaction with the mounted world.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ExecReadObservation {
    /// The path existed as a file with this content identity.
    File {
        content_hash: ContentHash,
        blob_node: Option<NodeHash>,
        size: u64,
    },
    /// The path existed as a directory with this Merkle directory identity.
    Directory { directory_node: NodeHash },
    /// A lookup under an observed directory missed.
    LookupMiss {
        parent_path: String,
        directory_node: NodeHash,
    },
}

/// Per-exec VFS prefix used to scope read observations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
#[repr(transparent)]
pub struct ExecPrefixId(pub String);

/// A tracked observation scope granted to one memoized exec.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecObservationScope {
    pub prefix_id: ExecPrefixId,
    pub roots: Vec<String>,
}

/// Exactly what the run was allowed and expected to read in stage 1. Stage 2
/// replaces this declared set with VFS-observed reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Facet)]
pub struct ExecReadSet {
    pub entries: BTreeMap<String, ExecReadObservation>,
}

/// One output subfile that is ready for subfile-granular demand release.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecSubfileCompletion {
    pub path: String,
    pub content_hash: ContentHash,
    pub size: u64,
}

/// Final output-tree receipt after process exit.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecTreeFinalization {
    pub files: Vec<ExecSubfileCompletion>,
}

/// Progressive output-tree events visible to callers.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ExecTreeEvent {
    SubfileCompleted(ExecSubfileCompletion),
    TreeFinalized(ExecTreeFinalization),
}

/// Typed control hook for future kill-when-satisfied demand accounting.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ExecControlSignal {
    /// PIN(progressive-exec-trees): vix-side demand accounting will send this
    /// to a running exec once all demanded subfiles have completed.
    DemandsSatisfied(ExecDemandsSatisfied),
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecDemandsSatisfied {
    pub completed: Vec<String>,
}

/// A completed run: declared outputs plus the read-set observation slot.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecOutcome {
    pub outputs: ExecTree,
    pub read_set: ExecReadSet,
    pub observation_scopes: Vec<ExecObservationScope>,
    pub tree_events: Vec<ExecTreeEvent>,
}

/// The result of an execution: the process outcome plus the captured write-set.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecResult {
    /// Exit status — the process exit code, or `128 + signal` if killed by a signal.
    pub exit_code: i32,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
    /// Outputs and read-set when the process ran successfully and declared
    /// outputs were harvested.
    pub outcome: Option<ExecOutcome>,
    /// Machine-visible failure reason: spawn/sandbox/harvest failures, or a
    /// nonzero process exit.
    pub error: Option<String>,
}

pub fn exec_role_input_paths(arg: &str, role: ExecRole) -> Vec<String> {
    match role {
        ExecRole::Executable | ExecRole::Input => vec![arg.to_string()],
        ExecRole::InputFlag => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_output_paths(arg: &str, role: ExecRole) -> Vec<String> {
    match role {
        ExecRole::Output => vec![arg.to_string()],
        ExecRole::OutputFlag => rustc_emit_output_paths(arg),
        _ => Vec::new(),
    }
}

pub fn exec_role_output_dir_paths(arg: &str, role: ExecRole) -> Vec<String> {
    match role {
        ExecRole::OutputDir => vec![arg.to_string()],
        _ => Vec::new(),
    }
}

pub fn exec_role_stdout_paths(arg: &str, role: ExecRole) -> Vec<String> {
    match role {
        ExecRole::Stdout => vec![arg.to_string()],
        _ => Vec::new(),
    }
}

pub fn exec_role_env_paths(arg: &str, role: ExecRole) -> Vec<String> {
    match role {
        ExecRole::Env => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_search_dir_paths(arg: &str, role: ExecRole) -> Vec<String> {
    match role {
        ExecRole::SearchDir => vec![arg.to_string()],
        ExecRole::SearchDirFlag => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_embedded_paths(arg: &str, role: ExecRole) -> Vec<String> {
    let mut paths = exec_role_input_paths(arg, role);
    paths.extend(exec_role_output_paths(arg, role));
    paths.extend(exec_role_output_dir_paths(arg, role));
    paths.extend(exec_role_stdout_paths(arg, role));
    paths.extend(exec_role_env_paths(arg, role));
    paths.extend(exec_role_search_dir_paths(arg, role));
    paths
}

fn embedded_path_after_equals(arg: &str) -> Option<String> {
    arg.rsplit_once('=')
        .map(|(_, path)| path)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

fn rustc_emit_output_paths(arg: &str) -> Vec<String> {
    let Some(spec) = arg.strip_prefix("--emit=") else {
        return Vec::new();
    };
    spec.split(',')
        .filter_map(|entry| {
            let mut parts = entry.splitn(2, '=');
            let _kind = parts.next()?;
            parts
                .next()
                .filter(|path| !path.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// Runner service trait
///
/// A remote-capable worker shell.
#[vox::service]
#[allow(async_fn_in_trait)]
pub trait Runner {
    async fn exec(&self, request: ExecRequest) -> ExecResult;
    async fn discover_probe(&self, request: DiscoverProbeRequest) -> DiscoverResult;
}
