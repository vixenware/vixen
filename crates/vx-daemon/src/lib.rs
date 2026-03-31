//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Can serve the store service, the runner service, the vfs service

mod service;

use eyre::Result;
pub use service::DaemonService;
use std::time::Duration;
use vox::ConnectionAcceptor;
use vox::{AcceptedConnection, DriverCaller};
use vox::{
    ConnectionId, ConnectionSettings, Metadata, MetadataEntry, MetadataFlags, MetadataValue,
};
use vox_stream::{LocalLink, LocalLinkAcceptor};

pub type UpstreamCaller = DriverCaller;

/// Serve the VXD service
pub async fn serve() -> Result<()> {
    let acceptor = LocalLinkAcceptor::bind("/tmp/vx-daemon.sock")?;

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
    let (driver, sess_handle) = vox::acceptor_on(link)
        .on_connection(DaemonAcceptor {})
        .establish::<DriverCaller>(DaemonService {})
        .await?;

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await
    }
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
