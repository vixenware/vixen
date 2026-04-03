//! vx-runner - Worker daemon library.
//!
//! Exposes [`RunnerService`] for use by the daemon (in-process),
//! or standalone via the `vx-runner` binary using [`serve`].

pub(crate) mod service;

use eyre::Result;
use vx_services::RunnerDispatcher;

/// The runner service implementation.
#[derive(Clone, Default)]
pub struct RunnerService;

impl RunnerService {
    pub fn new() -> Self {
        Self
    }
}

/// A single-service [`vox::ConnectionAcceptor`] that routes all connections
/// to the [`RunnerService`].
#[derive(Clone)]
pub struct RunnerAcceptor {
    service: RunnerService,
}

impl RunnerAcceptor {
    pub fn new(service: RunnerService) -> Self {
        Self { service }
    }
}

impl vox::ConnectionAcceptor for RunnerAcceptor {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), vox::Metadata<'static>> {
        match request.service() {
            "Runner" => {
                connection.handle_with(RunnerDispatcher::new(self.service.clone()));
                Ok(())
            }
            other => {
                tracing::warn!("vx-runner: unknown service requested: {}", other);
                Err(vec![])
            }
        }
    }
}

/// Serve the runner RPC service on the given endpoint, blocking until done.
///
/// Called by the standalone `vx-runner` binary. The daemon hosts
/// [`RunnerService`] in-process via [`RunnerAcceptor`] instead.
pub async fn serve(listen: &str) -> Result<()> {
    let service = RunnerService::new();
    let acceptor = RunnerAcceptor::new(service);
    vox::serve(listen, acceptor).await?;
    Ok(())
}
