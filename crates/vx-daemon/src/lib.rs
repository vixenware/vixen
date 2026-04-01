//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Can serve the store service, the runner service, the vfs service

mod service;

use eyre::Result;
pub use service::DaemonService;
use vox::{
    AcceptedConnection, Caller, ConnectionAcceptor, ConnectionId, ConnectionSettings, DriverCaller,
    Metadata, MetadataEntry, MetadataFlags, MetadataValue,
};
use vox_stream::{LocalLink, LocalLinkAcceptor};
use vx_services::DaemonDispatcher;

pub type UpstreamCaller = DriverCaller;

pub const DAEMON_SOCK: &str = "/tmp/vx-daemon.sock";

/// Serve the VXD service
pub async fn serve() -> Result<()> {
    // Check for an existing socket
    if std::path::Path::new(DAEMON_SOCK).exists() {
        match LocalLink::connect(DAEMON_SOCK).await {
            Ok(_) => return Err(eyre::eyre!("vx daemon already running at {DAEMON_SOCK}")),
            Err(_) => {
                // Stale socket — remove it so we can bind
                std::fs::remove_file(DAEMON_SOCK)?;
            }
        }
    }

    let acceptor = LocalLinkAcceptor::bind(DAEMON_SOCK)?;

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
        .establish::<DriverCaller>(DaemonDispatcher::new(DaemonService {}))
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
