use std::io::IsTerminal;

use eyre::Context;
use vx_io::net::Endpoint;

use crate::tui;

/// CLI Context, passed everywhere
pub(crate) struct CliContext {
    pub(crate) mode: CliMode,
    pub(crate) strat: DaemonStrategy,
}

impl CliContext {
    pub(crate) fn from_cli(cli: &crate::cli::Cli) -> eyre::Result<Self> {
        let mode = if cli.json {
            CliMode::Json
        } else if cli.no_tui || !std::io::stdout().is_terminal() {
            CliMode::Text
        } else {
            CliMode::Tui(tui::TuiHandle::new())
        };
        let strat = DaemonStrategy::from_env()?;
        Ok(Self { mode, strat })
    }
}

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
#[derive(Clone)]
pub(crate) enum DaemonStrategy {
    /// Connect to existing daemons, never bootstrap
    Connect(Endpoint),

    /// Spawn daemons if necessary
    Bootstrap(Endpoint),
}

impl DaemonStrategy {
    /// Convert Bootstrap to Connect (keep the endpoint, just don't spawn).
    pub(crate) fn connect_only(self) -> Self {
        match self {
            Self::Bootstrap(ep) => Self::Connect(ep),
            already @ Self::Connect(_) => already,
        }
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        match self {
            Self::Bootstrap(ep) | Self::Connect(ep) => ep,
        }
    }

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
