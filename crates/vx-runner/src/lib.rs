//! vx-runner - Worker daemon library.
//!
//! Exposes [`RunnerService`] and [`RunnerAcceptor`] for use by the daemon (in-process).

pub(crate) mod exec;
pub(crate) mod sandbox;
pub(crate) mod service;

use std::sync::Arc;
use vx_services::{RunnerDispatcher, StoreClient};
use vx_vfsd::VfsdService;

/// The runner service implementation.
#[derive(Clone, Default)]
pub struct RunnerService {
    store: Option<Arc<StoreClient>>,
    /// The runner's local CAS-VFS — what `exec` mounts actions into and runs against.
    /// `None` disables `exec` (it has no VFS). Held directly (not over RPC): the
    /// runner is the in-process executor, reached over vox but driving its own VFS.
    vfsd: Option<VfsdService>,
}

impl RunnerService {
    pub fn new(store: Option<Arc<StoreClient>>) -> Self {
        Self { store, vfsd: None }
    }

    /// Set the local CAS-VFS `exec` mounts actions into.
    pub fn with_vfsd(mut self, vfsd: VfsdService) -> Self {
        self.vfsd = Some(vfsd);
        self
    }

    pub fn store(&self) -> Option<Arc<StoreClient>> {
        self.store.clone()
    }

    pub(crate) fn vfsd(&self) -> Option<&VfsdService> {
        self.vfsd.as_ref()
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
