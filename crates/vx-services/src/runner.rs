//! Runner service protocol definitions
//!
//! This crate defines the runner RPC surface only.
//!
//! During the evaluator purge, `vx-runner` is intentionally a minimal protocol
//! shell. The request and result payloads here are placeholders, not a stable
//! execution model.

use facet::Facet;

/// Runner protocol version.
/// Bump this when making breaking changes to the Runner RPC interface.
pub const RUNNER_PROTOCOL_VERSION: u32 = 20;

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

#[derive(Facet)]
pub struct ExecRequest {
    // TODO: argv, envp, mounts, etc.
}

#[derive(Facet)]
pub struct ExecResult {
    // TODO: design
}

/// Runner service trait
///
/// A remote-capable worker shell.
#[vox::service]
pub trait Runner {
    async fn exec(&self, request: ExecRequest) -> ExecResult;
    async fn discover_probe(&self, request: DiscoverProbeRequest) -> DiscoverResult;
}
