//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Can serve the store service, the runner service, the vfs service

mod service;

use eyre::Result;
use fs4::fs_std::FileExt;
pub use service::DaemonService;
use std::fs::File;
use vox::{
    AcceptedConnection, Caller, ConnectionAcceptor, ConnectionId, ConnectionSettings, DriverCaller,
    Metadata, MetadataEntry, MetadataFlags, MetadataValue,
};
use vox_stream::{LocalLink, LocalLinkAcceptor};
use vx_services::DaemonDispatcher;

pub type UpstreamCaller = DriverCaller;

pub enum ListenAddr {
    /// e.g. `/tmp/vx-daemon.sock`
    UnixSocket(camino::Utf8PathBuf),

    /// e.g. `0.0.0.0:9977`
    Tcp(std::net::SocketAddr),

    /// TODO: windows named pipe
    NamedPipe(()),
}

pub struct DaemonConfig {
    /// All addresses to listen on
    pub listen: Vec<ListenAddr>,
}

/// Derive a lock file path from a listen address.
fn lock_path_for(addr: &ListenAddr) -> Option<String> {
    match addr {
        ListenAddr::UnixSocket(path) => Some(format!("{path}.lock")),
        _ => None,
    }
}

/// Serve the VXD service on all configured listen addresses.
pub async fn serve(config: DaemonConfig) -> Result<()> {
    if config.listen.is_empty() {
        return Err(eyre::eyre!("no listen addresses configured"));
    }

    for l in &config.listen {
        match l {
            ListenAddr::UnixSocket(path) => {
                tracing::info!("configured to listen on unix socket {path}");
            }
            ListenAddr::Tcp(socket_addr) => {
                tracing::info!("configured to listen on tcp {socket_addr}");
            }
            ListenAddr::NamedPipe(_) => {
                tracing::info!("configured to listen on named pipe");
            }
        }
    }

    // Acquire an exclusive flock on a lock file derived from the first Unix socket.
    // The lock file is held for the lifetime of the process.
    let lock_path = config
        .listen
        .iter()
        .find_map(lock_path_for)
        .ok_or_else(|| eyre::eyre!("no Unix socket in config to derive lock file from"))?;
    let lock_file = File::create(&lock_path)?;
    lock_file.try_lock_exclusive().map_err(|_| {
        eyre::eyre!("vx daemon already running (could not acquire lock on {lock_path})")
    })?;

    if config.listen.is_empty() {
        return Err(eyre::eyre!("no listen addresses configured"));
    }

    // Bind all addresses up front (fail-fast if any bind fails)
    let mut accept_tasks = Vec::new();
    for addr in &config.listen {
        match addr {
            ListenAddr::UnixSocket(path) => {
                // We hold the flock, so any existing socket is stale
                if std::path::Path::new(path.as_str()).exists() {
                    tracing::info!("removing stale socket at {path}");
                    std::fs::remove_file(path.as_str())?;
                }
                let acceptor = LocalLinkAcceptor::bind(path.as_str())?;
                tracing::info!("vx daemon listening at {path}");
                accept_tasks.push(moire::task::spawn(accept_loop(acceptor)));
            }
            ListenAddr::Tcp(_addr) => {
                todo!("TCP listen not yet implemented");
            }
            ListenAddr::NamedPipe(_) => {
                todo!("named pipe listen not yet implemented");
            }
        }
    }

    // Wait for any accept loop to finish (they shouldn't under normal operation)
    let (result, _index, _remaining) = futures_util::future::select_all(accept_tasks).await;
    result??;

    // lock_file dropped here (or on process exit), releasing the flock
    Ok(())
}

async fn accept_loop(acceptor: LocalLinkAcceptor) -> eyre::Result<()> {
    loop {
        match acceptor.accept().await {
            Ok(link) => {
                moire::task::spawn(handle_conn(link));
            }
            Err(error) => {
                tracing::debug!("vxd accept error: {error}");
            }
        }
    }
}

async fn handle_conn(link: LocalLink) -> eyre::Result<()> {
    tracing::info!("local vxd connection");
    let (client, session) = vox::acceptor_on(link)
        .on_connection(DaemonAcceptor {})
        .establish::<DriverCaller>(DaemonDispatcher::new(DaemonService::new()))
        .await?;

    let _session = session;
    client.closed().await;
    tracing::info!("vxd connection out");
    Ok(())
}

/// A connection acceptor that can serve multiple sub-services:
/// store, runner, vfs, etc.
struct DaemonAcceptor {}

impl ConnectionAcceptor for DaemonAcceptor {
    fn accept(
        &self,
        conn_id: ConnectionId,
        peer_settings: &ConnectionSettings,
        metadata: &[MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        tracing::info!(%conn_id, ?peer_settings, ?metadata, "Sub-connection attempt");

        return Err(vec![MetadataEntry {
            key: "error",
            value: MetadataValue::String("oh we have to leak to return metadata, neat"),
            flags: MetadataFlags::NONE,
        }]);
    }
}
