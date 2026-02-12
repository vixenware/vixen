//! Boot protocol for vixen daemons.
//!
//! Every daemon exposes a `Boot` service on the root connection.
//! Clients call `boot()` to initialize the daemon, then open a
//! virtual connection to access the real service.

use facet::Facet;

/// Result of a boot attempt.
#[derive(Debug, Clone, Facet)]
pub struct BootResult {
    /// Whether boot succeeded.
    pub success: bool,
    /// Error message if boot failed.
    pub error: Option<String>,
}

/// Boot service — exposed on the root connection of every daemon.
///
/// The protocol is:
/// 1. Client connects, gets a root connection with only `Boot`
/// 2. Client calls `boot()` — daemon initializes
/// 3. If boot succeeds, client opens a virtual connection for the real service
/// 4. If boot fails, client gets a structured error and can retry or report
#[roam::service]
pub trait Boot {
    /// Initialize the daemon. Returns success/failure.
    /// The daemon stays alive regardless of outcome.
    async fn boot(&self) -> BootResult;
}
