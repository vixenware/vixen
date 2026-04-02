//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Can serve the store service, the runner service, the vfs service

use eyre::Result;
use vox::{ConnectionAcceptor, DriverCaller, Metadata};
use vx_services::{DaemonClient, DaemonDispatcher, DaemonStatus};

mod service;
pub use service::DaemonService;

pub struct DaemonConfig {
    /// All addresses to listen on
    pub listen: Vec<String>,
}

/// Serve the VXD service on all configured listen addresses.
pub async fn serve(config: DaemonConfig) -> Result<()> {
    if config.listen.is_empty() {
        return Err(eyre::eyre!("no listen addresses configured"));
    }

    let service = DaemonService::new();
    let acceptor = DaemonAcceptor { service };

    let mut listeners = tokio::task::JoinSet::new();
    for l in config.listen {
        let acceptor = acceptor.clone();
        listeners.spawn(async move { vox::serve(l, acceptor).await });
    }

    while let Some(result) = listeners.join_next().await {
        result??;
    }

    Ok(())
}

/// A connection acceptor that can serve multiple sub-services:
/// store, runner, vfs, etc.
#[derive(Clone)]
struct DaemonAcceptor {
    service: DaemonService,
}

impl ConnectionAcceptor for DaemonAcceptor {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), Metadata<'static>> {
        tracing::info!("daemon got request for service: {}", request.service());
        connection.handle_with(DaemonDispatcher::new(self.service.clone()));

        Ok(())
    }
}
