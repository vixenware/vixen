//! vx-store - Content-Addressed Storage daemon
//!
//! Provides CAS, toolchain, and registry services over TCP or Unix sockets using Roam.

pub(crate) mod chunking;
pub(crate) mod hash_reader;
pub(crate) mod registry;
pub(crate) mod service;
pub(crate) mod storage;
pub(crate) mod toolchain;
pub(crate) mod types;

use crate::registry::RegistryManager;
use crate::storage::{CasStore, FsStorage};
use crate::toolchain::ToolchainManager;
use crate::types::{StoreService, StoreServiceInner};
use camino::Utf8PathBuf;
use eyre::Result;
use facet::Facet;
use figue as args;
use roam_stream::{HandshakeConfig, accept};
use std::sync::Arc;
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_store_proto::StoreDispatcher;

/// vx-store - Content-Addressed Storage daemon
#[derive(Facet, Debug)]
struct Args {
    /// Storage root directory
    #[facet(args::named)]
    root: Utf8PathBuf,

    /// VX home directory
    #[facet(args::named)]
    vx_home: Utf8PathBuf,

    /// Listen endpoint (host:port or unix:/path/to/socket)
    #[facet(args::named)]
    listen: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

fn main() -> Result<()> {
    peeps::threads::install_sigprof_handler();
    peeps::threads::register_thread("main");
    peeps::tasks::init_task_tracking();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .on_thread_start(|| {
            peeps::threads::register_thread(
                std::thread::current().name().unwrap_or("tokio-worker"),
            );
        })
        .on_thread_stop(|| {
            peeps::threads::deregister_thread();
        })
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // If spawned by parent, die when parent dies
    ur_taking_me_with_you::die_with_parent();

    // Initialize tracing
    vx_tracing::init_service("vx_store=info");

    // Initialize peeps instrumentation
    peeps::init_named("vx-store");

    tracing::info!("hello from vixen-store");

    let args: Args = args::Driver::new(
        args::builder::<Args>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name("vx-store")
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    let bind = Endpoint::parse(&args.listen)?;

    // Bind listener BEFORE booting — so parent can connect immediately.
    let mut listener = Listener::bind(&bind).await?;
    tracing::info!("store listening on {}", bind);

    // Set up boot gate — root connections get Boot service only.
    let (gate, boot_tx) = BootGate::<StoreService>::new();

    // Boot in background: initialize storage, create service.
    {
        let gate = Arc::clone(&gate);
        let root = args.root.clone();
        let vx_home = args.vx_home.clone();
        peeps::tasks::spawn_tracked("store_boot", async move {
            tracing::info!("Booting vx-store");
            match async {
                let storage = CasStore::new(FsStorage::new(root));
                storage.init().await?;
                let store = StoreService::new(storage, vx_home);
                Ok::<_, eyre::Report>(store)
            }
            .await
            {
                Ok(store) => {
                    tracing::info!("vx-store boot succeeded");
                    gate.set_service(store);
                    boot_tx.ready();
                }
                Err(e) => {
                    tracing::error!("vx-store boot failed: {e:#}");
                    boot_tx.failed(format!("{e:#}"));
                }
            }
        });
    }

    let boot_service = gate.boot_service();

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        tracing::info!("accepted connection from {peer_addr}");
        let boot_service = boot_service.clone();

        peeps::tasks::spawn_tracked(format!("store_connection_{}", peer_addr), async move {
            tracing::debug!("New connection from {}", peer_addr);

            // Root connection gets Boot service only.
            let boot_dispatcher = BootDispatcher::new(boot_service.clone());
            let (_handle, mut incoming, driver) = match accept(
                stream,
                HandshakeConfig {
                    max_payload_size: 256 * 1024 * 1024, // 256 MiB
                    name: Some("vx-store".into()),
                    ..Default::default()
                },
                boot_dispatcher,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("Handshake failed for {}: {e}", peer_addr);
                    return;
                }
            };

            // Drive the connection in the background.
            let driver_handle =
                peeps::tasks::spawn_tracked(format!("store_driver_{}", peer_addr), async move {
                    driver.run().await
                });

            // Accept virtual connections — each gets a StoreDispatcher.
            while let Some(conn) = incoming.recv().await {
                let Some(store) = boot_service.gate().service() else {
                    conn.reject("boot not complete".into(), vec![]);
                    continue;
                };
                let dispatcher = StoreDispatcher::new(store.clone());
                if let Err(e) = conn.accept(vec![], Some(Box::new(dispatcher))).await {
                    tracing::warn!(
                        "Failed to accept virtual connection from {}: {e}",
                        peer_addr
                    );
                }
            }

            if let Err(e) = driver_handle.await {
                tracing::warn!("Driver error from {}: {e:?}", peer_addr);
            }
            tracing::debug!("Connection from {} closed", peer_addr);
        });
    }
}

impl StoreService {
    fn new(storage: CasStore, vx_home: Utf8PathBuf) -> Self {
        Self {
            inner: Arc::new(StoreServiceInner {
                storage,
                vx_home,
                toolchain_manager: ToolchainManager::new(),
                registry_manager: RegistryManager::new(),
                download_semaphore: Arc::new(tokio::sync::Semaphore::new(32)),
                rust_component_tarballs: peeps::locks::DiagnosticMutex::new(
                    "store/rust_component_tarballs",
                    std::collections::HashMap::new(),
                ),
                rust_component_trees: peeps::locks::DiagnosticMutex::new(
                    "store/rust_component_trees",
                    std::collections::HashMap::new(),
                ),
            }),
        }
    }
}
