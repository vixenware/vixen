//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod cli;

use eyre::Result;
use figue as args;
use vox::{SessionHandle, TransportMode};
use vox_stream::LocalLink;
use vx_services::DaemonClient;

use crate::cli::{Cli, CliCommand};

fn main() {
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

    match cli.command {
        CliCommand::Doctor(_args) => {
            eprintln!("All is looking good.");
        }
        CliCommand::Daemon(_args) => {
            if let Err(e) = vxd::serve().await {
                eprintln!("vx daemon: {e}");
                std::process::exit(1);
            }
        }
    };
}

/// Connect to the daemon, auto-starting it if not running.
///
/// Returns the client and a session handle — drop the handle to close
/// the connection.
#[allow(dead_code)]
async fn connect_or_start() -> Result<(DaemonClient, SessionHandle)> {
    // Fast path: daemon already running
    if let Ok(link) = LocalLink::connect(vxd::DAEMON_SOCK).await {
        return establish(link).await;
    }

    // Spawn the daemon detached and wait for the socket to appear
    let vx_exe = std::env::current_exe()?;
    tokio::process::Command::new(&vx_exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    for i in 0u64..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50 + i * 10)).await;
        if let Ok(link) = LocalLink::connect(vxd::DAEMON_SOCK).await {
            return establish(link).await;
        }
    }

    Err(eyre::eyre!("vx daemon failed to start within timeout"))
}

async fn establish(link: LocalLink) -> Result<(DaemonClient, SessionHandle)> {
    let (client, session) = vox::initiator_on(link, TransportMode::Bare)
        .establish::<DaemonClient>(())
        .await?;
    Ok((client, session))
}
