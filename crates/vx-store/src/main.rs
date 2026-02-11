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
    tracing::debug!("tracing initialized");

    // Initialize peeps instrumentation
    peeps::init();
    #[cfg(unix)]
    peeps::install_sigusr1("vx-store");

    tracing::info!("hello from vixen-store");
    tracing::debug!("parsing args");

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

    tracing::debug!("args parsed, listen={}", args.listen);
    let bind = Endpoint::parse(&args.listen)?;

    // Build storage backend
    let storage = CasStore::new(FsStorage::new(args.root.clone()));

    // Initialize storage directories
    tracing::debug!("initializing storage at {}", args.root);
    storage.init().await?;
    tracing::debug!("storage initialized");

    // Initialize store service
    let store = StoreService::new(storage, args.vx_home);

    // SIGUSR1: dump diagnostics
    #[cfg(unix)]
    {
        let store = store.clone();
        peeps::tasks::spawn_tracked("sigusr1_handler", async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigusr1 =
                signal(SignalKind::user_defined1()).expect("failed to register SIGUSR1 handler");
            loop {
                sigusr1.recv().await;
                tracing::info!("SIGUSR1 received, dumping diagnostics");
                let sem = &store.download_semaphore;
                tracing::debug!("SIGUSR1 received");
                tracing::debug!("download_semaphore: available={}", sem.available_permits(),);
                eprint!("{}", roam_session::diagnostic::dump_all_diagnostics());

                let mut custom = std::collections::HashMap::new();
                custom.insert(
                    "download_semaphore_available".into(),
                    sem.available_permits().to_string(),
                );
                let dump = peeps::collect_dump("vx-store", custom);
                peeps::write_dump(&dump);
            }
        });
    }

    // Start server (TCP or Unix socket)
    let listener = Listener::bind(&bind).await?;
    tracing::info!("store listening on {}", bind);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        tracing::info!("accepted connection");
        let store = store.clone();

        peeps::tasks::spawn_tracked(format!("store_connection_{}", peer_addr), async move {
            tracing::debug!("New connection from {}", peer_addr);

            let dispatcher = StoreDispatcher::new(store);
            let (_handle, _incoming, driver) = match accept(
                stream,
                HandshakeConfig {
                    max_payload_size: 256 * 1024 * 1024, // 256 MiB
                    name: Some("vx-store".into()),
                    ..Default::default()
                },
                dispatcher,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("Handshake failed for {}: {e}", peer_addr);
                    return;
                }
            };
            if let Err(e) = driver.run().await {
                tracing::warn!("Connection error from {}: {}", peer_addr, e);
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
                rust_component_tarballs: peeps::locks::DiagnosticAsyncMutex::new(
                    "store/rust_component_tarballs",
                    std::collections::HashMap::new(),
                ),
                rust_component_trees: peeps::locks::DiagnosticAsyncMutex::new(
                    "store/rust_component_trees",
                    std::collections::HashMap::new(),
                ),
            }),
        }
    }
}
