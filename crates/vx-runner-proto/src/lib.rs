//! Runner service protocol definitions
//!
//! The runner service is a remote-capable worker that compiles code.
//! All file inputs and outputs go through CAS/VFS, and shipped evaluator
//! payloads go over RPC. No filesystem paths are shared between daemon and runner.
//!
//! Runner responsibilities:
//! - Materialize toolchains from CAS (cached locally)
//! - Materialize source trees from CAS
//! - Materialize dependency artifacts from CAS
//! - Run rustc/zig
//! - Ingest outputs to CAS
//! - Return output manifest hashes
//!
//! Daemon responsibilities:
//! - Ingest source files to CAS
//! - Orchestrate build order
//! - Compute cache keys
//! - Check cache hits
//! - Materialize final outputs locally (if needed)

use facet::Facet;
use vx_store_proto::{ServiceVersion, StoreProgress};

/// Runner protocol version.
/// Bump this when making breaking changes to the Runner RPC interface.
pub const RUNNER_PROTOCOL_VERSION: u32 = 20;

// =============================================================================
// Generic Sandboxed Tool Execution
// =============================================================================

// =============================================================================
// Runner Service Trait
// =============================================================================

/// TODO: design for eval3
#[derive(Facet)]
pub struct RunRequest {}
#[derive(Facet)]
pub struct RunResult {}
#[derive(Facet)]
pub struct DiscoverRequest {}
#[derive(Facet)]
pub struct DiscoverResult {}

/// Runner service trait
///
/// A remote-capable compilation service. All inputs/outputs go through CAS.
/// No filesystem paths are shared between the daemon and the runner.
#[roam::service]
pub trait Runner {
    /// Get service version information (for health checks and compatibility)
    async fn version(&self) -> ServiceVersion;

    /// Compile a Rust crate (lib or bin)
    ///
    /// Runner will:
    /// 1. Materialize the toolchain from CAS (cached locally)
    /// 2. Materialize the source tree from CAS
    /// 3. Materialize dependency rlibs from CAS
    /// 4. Run rustc
    /// 5. Ingest output to CAS
    /// 6. Return the output manifest hash
    ///
    /// Progress channel receives merged stdout/stderr lines during compilation.
    /// Run a sandboxed command with hermetic inputs/outputs via CAS/VFS.
    async fn run(&self, request: RunRequest, progress: roam::Tx<StoreProgress>) -> RunResult;

    /// Evaluate a shipped discovery probe on the executor host.
    async fn discover(&self, request: DiscoverRequest) -> DiscoverResult;
}
