//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod cli;

use figue as args;
use vox::TransportMode;
use vx_services::{DAEMON_SOCK, DaemonClient};

use crate::cli::{Cli, CliCommand};

fn main() {
    init_cli_stderr();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(async_main());
}

async fn async_main() {
    let cli: Cli = args::Driver::new(
        args::builder::<Cli>()
            .expect("failed to build CLI")
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name(env!("CARGO_PKG_NAME"))
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    let res = match cli.command {
        CliCommand::Health(_args) => health().await,
        CliCommand::Daemon(_args) => {
            let config = vxd::DaemonConfig {
                listen: vec![vx_services::DAEMON_SOCK.to_string()],
            };
            vxd::serve(config).await
        }
    };

    if let Err(e) = res {
        eprintln!("fatal error: {e}");
        std::process::exit(1);
    }
}

async fn health() -> eyre::Result<()> {
    let client: DaemonClient = vox::connect(DAEMON_SOCK).await?;
    let status = client.status().await?;
    tracing::info!("Daemon status: {status:?}");
    Ok(())
}

/// Build the default EnvFilter for CLI processes that talk to multiple vx crates.
pub fn cli_env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "warn,vx=info,vxd=info,vx_store=info,vx_toolchain=info,vx_runner=info",
        )
    })
}

/// Initialize tracing for CLI with plain stderr output (non-TUI mode).
pub fn init_cli_stderr() {
    tracing_subscriber::fmt()
        .with_timer(tracing_subscriber::fmt::time::SystemTime)
        .with_env_filter(cli_env_filter())
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_ids(false)
        .init();
}
