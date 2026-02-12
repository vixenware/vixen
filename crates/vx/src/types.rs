use eyre::Context;
use vx_io::net::Endpoint;

use crate::tui;

#[derive(Clone)]
pub(crate) enum CliMode {
    /// Full-terminal TUI: tracing is redirected to events etc.
    Tui(tui::TuiHandle),
    /// NDJSON output
    Json,
    /// Text output
    Text,
}

/// VX daemon strategy
pub(crate) enum DaemonStrategy {
    /// Connect to existing daemons, never bootstrap
    Connect(Endpoint),

    /// Spawn daemons if necessary
    Bootstrap(Endpoint),
}

impl DaemonStrategy {
    pub(crate) fn from_env() -> eyre::Result<Self> {
        if let Ok(vxd) = std::env::var("VXD") {
            // VXD env var set: connect to existing daemon, never spawn
            let endpoint = Endpoint::parse(&vxd).wrap_err("parsing $VXD env var")?;
            Ok(Self::Connect(endpoint))
        } else {
            // No VXD env var: spawn daemon if necessary
            let vx_home = vx_io::vx_home();
            Ok(Self::Bootstrap(vx_io::net::default_local_endpoint(
                &vx_home, "vxd",
            )))
        }
    }
}
