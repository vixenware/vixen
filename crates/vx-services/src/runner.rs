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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
#[repr(transparent)]
pub struct RunnerProtocolVersion(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
#[repr(transparent)]
pub struct ExecPath(pub String);

impl ExecPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ExecPath {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for ExecPath {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::borrow::Borrow<str> for ExecPath {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExecPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
#[repr(transparent)]
pub struct ExecProgram(pub String);

impl ExecProgram {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ExecProgram {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for ExecProgram {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for ExecProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
#[repr(transparent)]
pub struct ExecArgValue(pub String);

impl ExecArgValue {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ExecArgValue {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for ExecArgValue {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for ExecArgValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
#[repr(transparent)]
pub struct ByteLen(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(transparent)]
pub struct ExecExitCode(pub i32);

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(transparent)]
pub struct ExecDiagnostic(pub String);

// =============================================================================
// Runner Service Trait
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(transparent)]
pub struct PlatformName(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(transparent)]
pub struct PlatformArch(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RunnerPlatform {
    pub os: PlatformName,
    pub arch: PlatformArch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum RunnerTransport {
    VoxWebsocket,
    VoxLocal,
    VoxTcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum SandboxCapability {
    Seatbelt,
    Landlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ToolchainKind {
    Shell,
    CCompiler,
    Rustc,
    SandboxExec,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ToolchainCapability {
    pub kind: ToolchainKind,
    pub executable: ExecPath,
    pub content_hash: Option<ContentHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RunnerCapabilities {
    pub protocol_version: RunnerProtocolVersion,
    pub platform: RunnerPlatform,
    pub transports: Vec<RunnerTransport>,
    pub sandboxes: Vec<SandboxCapability>,
    pub toolchains: Vec<ToolchainCapability>,
}

#[derive(Facet)]
pub struct CapabilityProbeRequest {}

#[derive(Facet)]
pub struct CapabilityProbeResult {
    pub capabilities: RunnerCapabilities,
}

/// A mounted or flushed tree. Text entries keep fixtures readable; blob entries
/// carry arbitrary bytes without lossy UTF-8 conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Facet)]
pub struct ExecTree {
    pub entries: BTreeMap<ExecPath, String>,
    pub blobs: BTreeMap<ExecPath, Vec<u8>>,
}

impl ExecTree {
    pub fn insert_bytes(&mut self, path: impl Into<String>, contents: Vec<u8>) {
        let path = ExecPath(path.into());
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
    pub at: ExecPath,
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
    pub value: ExecArgValue,
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
    pub program: ExecProgram,
    /// Role-annotated argv.
    pub plan: ExecPlan,
    /// Declared input mounts.
    pub mounts: Vec<ExecMount>,
    /// Declared roots that may be executed or read as ambient toolchain state.
    pub toolchain_roots: Vec<ExecPath>,
}

/// One observed interaction with the mounted world.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ExecReadObservation {
    /// The path existed as a file with this content identity.
    File {
        content_hash: ContentHash,
        blob_node: Option<NodeHash>,
        size: ByteLen,
    },
    /// The path existed as a directory with this Merkle directory identity.
    Directory { directory_node: NodeHash },
    /// A lookup under an observed directory missed.
    LookupMiss {
        parent_path: ExecPath,
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
    pub roots: Vec<ExecPath>,
}

/// Exactly what the run was allowed and expected to read in stage 1. Stage 2
/// replaces this declared set with VFS-observed reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Facet)]
pub struct ExecReadSet {
    pub entries: BTreeMap<ExecPath, ExecReadObservation>,
}

/// One output subfile that is ready for subfile-granular demand release.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecSubfileCompletion {
    pub path: ExecPath,
    pub content_hash: ContentHash,
    pub size: ByteLen,
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

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ExecErrorKind {
    Staging,
    Spawn,
    UnsupportedPlatform,
    ProcessExit,
    Harvest,
    Verification,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecError {
    pub kind: ExecErrorKind,
    pub diagnostic: ExecDiagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ExecCompletion {
    Succeeded(ExecOutcome),
    Failed(ExecError),
}

/// The result of an execution: process bytes plus a typed completion.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecResult {
    /// Exit status — the process exit code, or `128 + signal` if killed by a signal.
    pub exit_code: ExecExitCode,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
    pub completion: ExecCompletion,
}

pub fn exec_role_input_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Executable | ExecRole::Input => vec![ExecPath::from(arg)],
        ExecRole::InputFlag => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_output_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Output => vec![ExecPath::from(arg)],
        ExecRole::OutputFlag => rustc_emit_output_paths(arg),
        _ => Vec::new(),
    }
}

pub fn exec_role_output_dir_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::OutputDir => vec![ExecPath::from(arg)],
        _ => Vec::new(),
    }
}

pub fn exec_role_stdout_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Stdout => vec![ExecPath::from(arg)],
        _ => Vec::new(),
    }
}

pub fn exec_role_env_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::Env => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_search_dir_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    match role {
        ExecRole::SearchDir => vec![ExecPath::from(arg)],
        ExecRole::SearchDirFlag => embedded_path_after_equals(arg).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn exec_role_embedded_paths(arg: &str, role: ExecRole) -> Vec<ExecPath> {
    let mut paths = exec_role_input_paths(arg, role);
    paths.extend(exec_role_output_paths(arg, role));
    paths.extend(exec_role_output_dir_paths(arg, role));
    paths.extend(exec_role_stdout_paths(arg, role));
    paths.extend(exec_role_env_paths(arg, role));
    paths.extend(exec_role_search_dir_paths(arg, role));
    paths
}

fn embedded_path_after_equals(arg: &str) -> Option<ExecPath> {
    arg.rsplit_once('=')
        .map(|(_, path)| path)
        .filter(|path| !path.is_empty())
        .map(ExecPath::from)
}

fn rustc_emit_output_paths(arg: &str) -> Vec<ExecPath> {
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
                .map(ExecPath::from)
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
    async fn capabilities(&self, request: CapabilityProbeRequest) -> CapabilityProbeResult;
}
