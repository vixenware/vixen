//! vxd - Build orchestration daemon
//!
//! Minimal daemon shell during the evaluator purge.
//! It currently owns:
//! - booting or connecting to `vx-store`
//! - exposing the `vxd` RPC surface
//! - per-connection session/progress plumbing
//!
//! It does not currently execute builds.

mod service;

use ::moire::process::Child;
use ::moire::sync::SyncMutex;
use camino::Utf8PathBuf;
use eyre::Result;
use facet::Facet;
use figue as args;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver, DriverCaller};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use std::env::consts::{ARCH, OS};
use std::sync::Arc;
use std::time::Duration;
use vx_io::boot::{BootDispatcher, BootGate, BootTx};
use vx_io::net::{Endpoint, Listener};
use vx_io::spawn::{ServiceBuilder, ServiceHandle};
use vx_store_proto::StoreClient;
use vxd_proto::{ProgressListenerClient, VxdDispatcher};

use crate::service::{ActivityRegistry, VxdServiceDeps};
pub use service::VxdService;

pub type UpstreamCaller = DriverCaller;

/// vxd - Build orchestration daemon
#[derive(Facet, Debug)]
struct CliArgs {
    /// Listen endpoint (default: $VX_HOME/vxd.sock)
    #[facet(default, args::named)]
    listen: Option<String>,

    /// VX home directory (default: ~/.vx)
    #[facet(default, args::named)]
    vx_home: Option<String>,

    /// vx-store endpoint — if absent, spawns vx-store automatically
    #[facet(default, args::named)]
    store: Option<String>,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[derive(Clone, Debug)]
struct Args {
    vx_home: Utf8PathBuf,
    store_endpoint: Option<Endpoint>,
    bind: Endpoint,
}

fn resolve_args(cli: CliArgs) -> Result<Args> {
    let vx_home = cli
        .vx_home
        .or_else(|| std::env::var("VX_HOME").ok())
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").expect("HOME not set");
            Utf8PathBuf::from(format!("{home}/.vx"))
        });

    let store_endpoint = cli
        .store
        .or_else(|| std::env::var("VX_STORE").ok())
        .map(|value| Endpoint::parse(&value))
        .transpose()?;

    let bind = match cli.listen {
        Some(value) => Endpoint::parse(&value)?,
        None => vx_io::net::default_local_endpoint(&vx_home, "vxd"),
    };

    Ok(Args {
        vx_home,
        store_endpoint,
        bind,
    })
}

fn detect_host_triple() -> Result<String> {
    let arch = match ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "i686",
        other => eyre::bail!("unsupported host architecture {other:?}"),
    };
    let triple = match OS {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        other => eyre::bail!("unsupported host operating system {other:?}"),
    };
    Ok(triple)
}

#[derive(Default)]
pub struct SpawnTracker {
    store: Option<Child>,
}

impl SpawnTracker {
    pub fn kill_all(&mut self) {
        if let Some(ref mut child) = self.store {
            let pid = child.id().unwrap_or(0);
            tracing::info!("Killing spawned vx-store (pid: {pid})");
            if let Err(error) = child.start_kill() {
                tracing::warn!("Failed to kill vx-store (pid: {pid}): {error}");
            }
        }
        self.store.take();
    }
}

struct ServiceClients {
    store: Arc<StoreClient>,
    store_handle: Arc<ServiceHandle>,
    activity_registry: ActivityRegistry,
}

#[derive(Clone)]
struct VxdVirtualAcceptor {
    boot_service: vx_io::boot::BootService<Arc<VxdService>>,
}

impl ConnectionAcceptor for VxdVirtualAcceptor {
    fn accept(
        &self,
        _conn_id: roam_types::ConnectionId,
        peer_settings: &ConnectionSettings,
        _metadata: &[roam_types::MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        let Some(daemon) = self.boot_service.gate().service().cloned() else {
            return Err(vec![]);
        };

        let parity = peer_settings.parity.other();
        let session = daemon.new_session();
        Ok(AcceptedConnection {
            settings: ConnectionSettings {
                parity,
                max_concurrent_requests: peer_settings.max_concurrent_requests,
            },
            metadata: vec![],
            setup: Box::new(move |handle| {
                let session = session.clone();
                moire::task::spawn(
                    async move {
                        let dispatcher = VxdDispatcher::new(session.clone());
                        let mut driver = Driver::new(handle, dispatcher);
                        let progress_listener =
                            Arc::new(ProgressListenerClient::new(driver.caller()));
                        session.set_progress_listener(Some(progress_listener)).await;
                        driver.run().await;
                        session.cancel_active_build().await;
                    }
                    .named("vxd_virtual_driver"),
                );
            }),
        })
    }
}

async fn ensure_services(
    args: &Args,
    spawn_tracker: &Arc<SyncMutex<SpawnTracker>>,
    boot_tx: &BootTx,
) -> Result<ServiceClients> {
    let store_endpoint = args
        .store_endpoint
        .clone()
        .unwrap_or_else(|| vx_io::net::default_local_endpoint(&args.vx_home, "store"));
    let store_root = args.vx_home.join("store");

    let mut store_builder = ServiceBuilder::new("vx-store", store_endpoint.clone())
        .caller_name("vxd")
        .args(&["--root", store_root.as_str()])
        .log_path(args.vx_home.join("store.log"))
        .timeout(Duration::from_secs(30));
    if args.store_endpoint.is_some() {
        store_builder = store_builder.connect_only();
    }

    boot_tx.phase("connecting to vx-store");
    let mut store_handle = store_builder.connect().await?;
    {
        let mut tracker = spawn_tracker.lock();
        tracker.store = store_handle.child.take();
    }

    boot_tx.phase("waiting for vx-store boot");
    let store_virtual = store_handle.boot_and_connect().await?;
    Ok(ServiceClients {
        store: Arc::new(StoreClient::new(store_virtual)),
        store_handle: Arc::new(store_handle),
        activity_registry: ActivityRegistry::new(),
    })
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    vx_tracing::init_service("vxd=info");

    let cli: CliArgs = args::Driver::new(
        args::builder::<CliArgs>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| h.program_name("vxd").version(env!("CARGO_PKG_VERSION")))
            .build(),
    )
    .run()
    .unwrap();
    let args = resolve_args(cli)?;

    let spawn_tracker = Arc::new(SyncMutex::new("vxd/spawn_tracker", SpawnTracker::default()));
    let exec_host_triple = detect_host_triple()?;

    let mut listener = Listener::bind(&args.bind).await?;
    tracing::info!("Daemon listening on {}", args.bind);

    let (gate, boot_tx) = BootGate::<Arc<VxdService>>::new();
    {
        let args_for_boot = args.clone();
        let spawn_tracker = Arc::clone(&spawn_tracker);
        let gate = Arc::clone(&gate);
        moire::task::spawn(
            async move {
                tracing::info!("Booting vxd services");
                match ensure_services(&args_for_boot, &spawn_tracker, &boot_tx).await {
                    Ok(clients) => {
                        let daemon = Arc::new(
                            VxdService::new(
                                args_for_boot.vx_home,
                                exec_host_triple,
                                VxdServiceDeps {
                                    spawn_tracker,
                                    store: clients.store,
                                    store_handle: clients.store_handle,
                                    activity_registry: clients.activity_registry,
                                },
                            )
                            .await,
                        );
                        gate.set_service(daemon);
                        boot_tx.ready();
                    }
                    Err(error) => {
                        tracing::error!(error = %format!("{error:#}"), "vxd boot failed");
                        boot_tx.failed(format!("{error:#}"));
                    }
                }
            }
            .named("vxd_boot"),
        );
    }

    let boot_service = gate.boot_service();
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let boot_service = boot_service.clone();
                moire::task::spawn(
                    async move {
                        tracing::debug!("vxd connection from {peer_addr}");
                        let (reader, writer) = tokio::io::split(stream);
                        let conduit = BareConduit::<roam_types::MessageFamily, _>::new(
                            StreamLink::new(reader, writer),
                        );
                        let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                        let (caller, session_handle) = match acceptor(conduit)
                            .on_connection(VxdVirtualAcceptor {
                                boot_service: boot_service.clone(),
                            })
                            .establish::<roam_core::NoopCaller>(boot_dispatcher)
                            .await
                        {
                            Ok(parts) => parts,
                            Err(error) => {
                                tracing::debug!("vxd handshake error from {peer_addr}: {error}");
                                return;
                            }
                        };
                        vx_io::net::park_session(caller, session_handle).await;
                    }
                    .named("vxd_connection"),
                );
            }
            Err(error) => {
                tracing::debug!("vxd accept error: {error}");
            }
        }
    }
}
