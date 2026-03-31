//! Runner service protocol definitions
//!
//! This crate defines the runner RPC surface only.
//!
//! During the evaluator purge, `vx-runner` is intentionally a minimal protocol
//! shell. The request and result payloads here are placeholders, not a stable
//! execution model.

use crate::store::{ServiceVersion, StoreProgress};
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

/// Placeholder run request while the eval3 runner protocol is rebuilt.
#[derive(Facet)]
pub struct RunRequest {}
/// Placeholder run result while the eval3 runner protocol is rebuilt.
#[derive(Facet)]
pub struct RunResult {}
/// Placeholder discover request while the eval3 runner protocol is rebuilt.
#[derive(Facet)]
pub struct DiscoverRequest {}
/// Placeholder discover result while the eval3 runner protocol is rebuilt.
#[derive(Facet)]
pub struct DiscoverResult {}

/// Runner service trait
///
/// A remote-capable worker shell.
#[vox::service]
pub trait Runner {
    /// Get service version information (for health checks and compatibility)
    async fn version(&self) -> ServiceVersion;

    /// Run work on the runner. The payload shape is intentionally unspecified
    /// until the eval3 execution protocol is defined.
    async fn run(&self, request: RunRequest, progress: vox::Tx<StoreProgress>) -> RunResult;

    /// Evaluate discovery work on the runner host. The payload shape is
    /// intentionally unspecified until the eval3 execution protocol is defined.
    async fn discover(&self, request: DiscoverRequest) -> DiscoverResult;
}
