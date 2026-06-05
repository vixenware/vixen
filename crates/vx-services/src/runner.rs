//! Runner service protocol definitions
//!
//! This crate defines the runner RPC surface only.
//!
//! During the evaluator purge, `vx-runner` is intentionally a minimal protocol
//! shell. The request and result payloads here are placeholders, not a stable
//! execution model.

use facet::Facet;

use crate::Blake3Hash;

/// Runner protocol version.
/// Bump this when making breaking changes to the Runner RPC interface.
pub const RUNNER_PROTOCOL_VERSION: u32 = 21;

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

/// One input mount: a content-addressed tree mounted at a prefix-relative path.
#[derive(Facet)]
pub struct ExecMount {
    /// Where to mount, relative to the action's VFS prefix (e.g. `"src"`).
    pub path: String,
    /// The tree manifest (in CAS) to mount there.
    pub tree: Blake3Hash,
}

/// One environment variable for the executed process.
#[derive(Facet)]
pub struct ExecEnv {
    pub key: String,
    pub value: String,
}

/// A flushed output root: the directory and the tree manifest of what was written.
#[derive(Facet)]
pub struct ExecOutput {
    /// The output directory, relative to the action's VFS prefix.
    pub path: String,
    /// The tree manifest (in CAS) of the files written there.
    pub tree: Blake3Hash,
}

/// A tool execution on an executor: the program to run, the content-addressed inputs
/// to mount under a fresh VFS prefix, and the writable output roots to flush as the
/// write-set. The process runs against the VFS prefix via the executor's access path.
/// (Sandbox confinement — the seatbelt profile derived from these mounts/outputs —
/// and caching join next; this is the mount → run → flush spine.)
#[derive(Facet)]
pub struct ExecRequest {
    /// The program — an absolute path, or a name resolved on the executor's `PATH`.
    pub program: String,
    /// The arguments after the program.
    pub args: Vec<String>,
    /// The environment for the process.
    pub env: Vec<ExecEnv>,
    /// Content-addressed inputs to mount under the action's VFS prefix.
    pub mounts: Vec<ExecMount>,
    /// Writable output directories (prefix-relative) to create before launch and
    /// flush as trees afterward.
    pub outputs: Vec<String>,
    /// Additional directories to create before launch. Each one must be equal to or
    /// nested under a writable output root.
    pub create_dirs: Vec<String>,
}

/// The result of an execution: the process outcome plus the captured write-set.
#[derive(Facet)]
pub struct ExecResult {
    /// Exit status — the process exit code, or `128 + signal` if killed by a signal.
    pub exit_code: i32,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
    /// The flushed write-set, one tree per requested output root.
    pub outputs: Vec<ExecOutput>,
    /// If the execution itself failed to run (couldn't mount, spawn, or flush) — as
    /// opposed to the process running and exiting nonzero — the reason. `None` on a
    /// clean run-to-completion (whatever the exit code).
    pub error: Option<String>,
}

/// Runner service trait
///
/// A remote-capable worker shell.
#[vox::service]
pub trait Runner {
    async fn exec(&self, request: ExecRequest) -> ExecResult;
    async fn discover_probe(&self, request: DiscoverProbeRequest) -> DiscoverResult;
}
