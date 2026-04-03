//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Can serve the store service, the runner service, the vfs service

use camino::Utf8PathBuf;
use eyre::Result;
use vox::{ConnectionAcceptor, Metadata};
use vx_runner::RunnerService;
use vx_services::{DaemonDispatcher, RunnerDispatcher, StoreDispatcher};
use vx_store::StoreService;

mod service;
pub use service::DaemonService;

pub struct DaemonConfig {
    /// All addresses to listen on
    pub listen: Vec<String>,
    /// Storage root directory
    pub store_root: Utf8PathBuf,
}

/// Serve the VXD service on all configured listen addresses.
pub async fn serve(config: DaemonConfig) -> Result<()> {
    if config.listen.is_empty() {
        return Err(eyre::eyre!("no listen addresses configured"));
    }

    let service = DaemonService::new();
    let store_service = vx_store::open(config.store_root).await?;
    let runner_service = RunnerService::new();
    let acceptor = DaemonAcceptor {
        service,
        store_service,
        runner_service,
    };

    let mut listeners = tokio::task::JoinSet::new();
    for l in config.listen {
        tracing::info!("listening on {}", l);
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
    store_service: StoreService,
    runner_service: RunnerService,
}

impl ConnectionAcceptor for DaemonAcceptor {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), Metadata<'static>> {
        tracing::info!("daemon got request for service: {}", request.service());
        match request.service() {
            "Daemon" => connection.handle_with(DaemonDispatcher::new(self.service.clone())),
            "Store" => connection.handle_with(StoreDispatcher::new(self.store_service.clone())),
            "Runner" => connection.handle_with(RunnerDispatcher::new(self.runner_service.clone())),
            other => {
                tracing::warn!("unknown service requested: {}", other);
                return Err(vec![]);
            }
        }
        Ok(())
    }
}
