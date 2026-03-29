//! vx-runner - Worker daemon
//!
//! Minimal runner shell during the evaluator purge.
//! It exposes the Runner RPC surface but does not currently execute actions.

pub(crate) mod service;

use eyre::Result;
use facet::Facet;
use figue as args;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use std::sync::Arc;
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_services::RunnerDispatcher;

#[derive(Clone)]
struct RunnerVirtualAcceptor {
    boot_service: vx_io::boot::BootService<RunnerService>,
}

impl ConnectionAcceptor for RunnerVirtualAcceptor {
    fn accept(
        &self,
        _conn_id: roam_types::ConnectionId,
        peer_settings: &ConnectionSettings,
        _metadata: &[roam_types::MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        let Some(exec) = self.boot_service.gate().service().cloned() else {
            return Err(vec![]);
        };

        let parity = peer_settings.parity.other();
        Ok(AcceptedConnection {
            settings: ConnectionSettings {
                parity,
                max_concurrent_requests: peer_settings.max_concurrent_requests,
            },
            metadata: vec![],
            setup: Box::new(move |handle| {
                let dispatcher = RunnerDispatcher::new(exec);
                moire::task::spawn(
                    async move {
                        let mut driver = Driver::new(handle, dispatcher);
                        driver.run().await;
                    }
                    .named("runner_virtual_driver"),
                );
            }),
        })
    }
}

/// vx-runner - Worker daemon
#[derive(Facet, Debug)]
struct Args {
    /// Listen endpoint for Runner RPC service
    #[facet(args::named)]
    listen: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[derive(Clone, Default)]
pub struct RunnerService;

impl vx_io::boot::GracefulShutdown for RunnerService {
    async fn graceful_shutdown(&self) {}
}

impl RunnerService {
    pub fn new() -> Self {
        Self
    }
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    ur_taking_me_with_you::die_with_parent();
    vx_tracing::init_service("vx_runner=info");

    let args: Args = args::Driver::new(
        args::builder::<Args>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name("vx-runner")
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    let bind_endpoint = Endpoint::parse(&args.listen)?;
    let mut listener = Listener::bind(&bind_endpoint).await?;
    tracing::info!("Runner RPC bound on {}", bind_endpoint);

    let (gate, boot_tx) = BootGate::<RunnerService>::new();
    {
        let gate = Arc::clone(&gate);
        moire::task::spawn(
            async move {
                tracing::info!("Booting vx-runner");
                gate.set_service(RunnerService::new());
                boot_tx.ready();
            }
            .named("runner_boot"),
        );
    }

    let boot_service = gate.boot_service();
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let boot_service = boot_service.clone();
                moire::task::spawn(
                    async move {
                        tracing::debug!("Runner connection from {}", peer_addr);
                        let (reader, writer) = tokio::io::split(stream);
                        let conduit = BareConduit::<roam_types::MessageFamily, _>::new(
                            StreamLink::new(reader, writer),
                        );
                        let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                        let (caller, session_handle) = match acceptor(conduit)
                            .on_connection(RunnerVirtualAcceptor {
                                boot_service: boot_service.clone(),
                            })
                            .establish::<roam_core::NoopCaller>(boot_dispatcher)
                            .await
                        {
                            Ok(parts) => parts,
                            Err(error) => {
                                tracing::debug!(
                                    "Runner handshake error from {}: {error}",
                                    peer_addr
                                );
                                return;
                            }
                        };
                        vx_io::net::park_session(caller, session_handle).await;
                    }
                    .named("runner_connection"),
                );
            }
            Err(error) => {
                tracing::debug!("Runner accept error: {}", error);
            }
        }
    }
}
