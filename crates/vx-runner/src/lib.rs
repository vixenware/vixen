//! vx-runner - Worker daemon library.
//!
//! Exposes [`RunnerService`] and [`RunnerAcceptor`] for use by the daemon (in-process).

pub(crate) mod service;

use std::sync::Arc;
use vx_services::{RunnerDispatcher, StoreClient};

/// The runner service implementation.
#[derive(Clone, Default)]
pub struct RunnerService {
    store: Option<Arc<StoreClient>>,
}

impl RunnerService {
    pub fn new(store: Option<Arc<StoreClient>>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> Option<Arc<StoreClient>> {
        self.store.clone()
    }
}

/// A [`vox::ConnectionAcceptor`] that routes all connections to the [`RunnerService`].
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
