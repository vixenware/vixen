//! Tiny daemon host/container protocol definitions.
//!
//! The daemon service is intentionally limited to host-level introspection.
//! Orchestration and evaluation belong on the `Orchestrator` service.

use facet::Facet;

/// Default Unix socket path for the daemon host.
pub const DAEMON_SOCK: &str = "local:///tmp/vixen.vox";

/// Status response from a running daemon host.
#[derive(Debug, Clone, Facet)]
pub struct DaemonStatus {
    /// How long the daemon has been running, in milliseconds.
    pub uptime_ms: u64,
}

/// Tiny host/container service.
#[vox::service]
pub trait Daemon {
    async fn status(&self) -> DaemonStatus;
}
