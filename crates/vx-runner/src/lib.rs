//! vx-runner - Worker daemon library.
//!
//! Exposes [`RunnerService`] and [`RunnerAcceptor`] for use by the daemon (in-process).

pub(crate) mod exec;
pub(crate) mod sandbox;
pub(crate) mod service;

use std::sync::Arc;
use vx_runtime::observe::ObserveDispatcher;
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
            "Observe" => {
                connection.handle_with(ObserveDispatcher::new(self.service.clone()));
                Ok(())
            }
            other => {
                tracing::warn!("vx-runner: unknown service requested: {}", other);
                Err(vec![])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use vx_runtime::observe::{ExecCommand, ObserveClient, ObserveRequest};
    use vx_runtime::strand::{ObserveOutcome, Outcome, ResourceId, Scheduler, dispatch_observe};

    use super::{RunnerAcceptor, RunnerService};

    async fn observe_client_for(service: RunnerService) -> ObserveClient {
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(RunnerAcceptor::new(service))
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        vox::initiator_on(client_link, vox::TransportMode::Bare)
            .establish::<ObserveClient>()
            .await
            .expect("establish runner-backed Observe client")
    }

    #[test]
    fn strand_observe_reaches_runner_backed_service() {
        let scheduler = Scheduler::new(&[1]);
        let client = scheduler.block_on(observe_client_for(RunnerService::default()));
        let client = scheduler.register_observe_client(client);
        let strand = scheduler.spawn(ResourceId(0), move || {
            let observed = dispatch_observe(
                client,
                ObserveRequest {
                    observer: None,
                    command: ExecCommand {
                        program: "true".to_string(),
                        args: Vec::new(),
                    },
                },
            );
            match observed.as_ref() {
                ObserveOutcome::Poisoned(reason)
                    if reason.contains("runner-backed observe has no CAS-VFS configured") =>
                {
                    Outcome::Clean
                }
                other => Outcome::Poisoned(format!(
                    "expected runner-backed no-CAS-VFS poison, got {other:?}"
                )),
            }
        });
        scheduler.demand(strand);
        scheduler.run();
        assert_eq!(scheduler.outcome(strand), Some(Outcome::Clean));
    }
}
