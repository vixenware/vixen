//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Can serve the store service, the runner service, the vfs service

mod service;

use eyre::Result;
use figue as args;
use moire::task::FutureExt;
pub use service::VxdService;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use vox_core::{AcceptedConnection, BareConduit, DriverCaller};
use vox_core::{ConnectionAcceptor, acceptor};
use vox_stream::StreamLink;
use vox_types::{
    ConnectionAccept, ConnectionId, ConnectionSettings, MessageFamily, Metadata, MetadataEntry,
    MetadataFlags, MetadataValue,
};
use vx_io::net::Listener;

pub type UpstreamCaller = DriverCaller;

/// Serve the VXD service
pub async fn serve(listener: Listener) -> Result<()> {
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                moire::task::spawn(handle_conn(stream, peer_addr));
            }
            Err(error) => {
                tracing::debug!("vxd accept error: {error}");
            }
        }
    }
}

async fn handle_conn(stream: Stream, peer_addr: String) -> eyre::Result<()> {
    tracing::info!("vxd connection from {peer_addr}");
    let (reader, writer) = tokio::io::split(stream);

    let (driver, sess_handle) = vox::acceptor_on(StreamLink::new(reader, writer))
        .on_connection(acceptor)
        .establish(VxdService)
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
