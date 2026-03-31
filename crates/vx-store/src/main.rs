//! vx-store - Content-Addressed Storage daemon
//!
//! Provides CAS, toolchain, and registry services over TCP or Unix sockets using Roam.

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
use moire::task::FutureExt;
use std::sync::Arc;
use vox_core::acceptor;
use vox_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use vox_stream::StreamLink;
use vox_types::{ConnectionSettings, Metadata, MetadataEntry, MetadataValue};
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_services::StoreDispatcher;

/// vx-store - Content-Addressed Storage daemon
#[derive(Facet, Debug)]
struct Args {
    /// Storage root directory
    #[facet(args::named)]
    root: Utf8PathBuf,

    /// Listen endpoint (host:port or unix:/path/to/socket)
    #[facet(args::named)]
    listen: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[derive(Clone)]
struct StoreVirtualAcceptor {
    boot_service: vx_io::boot::BootService<StoreService>,
}

const UPSTREAM_STORE_METADATA_KEY: &str = "vx-upstream-store";

fn requests_upstream_store(metadata: &[MetadataEntry<'_>]) -> bool {
    metadata.iter().any(|entry| {
        entry.key == UPSTREAM_STORE_METADATA_KEY && matches!(entry.value, MetadataValue::U64(1))
    })
}

impl ConnectionAcceptor for StoreVirtualAcceptor {
    fn accept(
        &self,
        _conn_id: vox_types::ConnectionId,
        peer_settings: &ConnectionSettings,
        metadata: &[vox_types::MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        let Some(store) = self.boot_service.gate().service().cloned() else {
            return Err(vec![]);
        };
        let wants_upstream = requests_upstream_store(metadata);

        let parity = peer_settings.parity.other();
        Ok(AcceptedConnection {
            settings: ConnectionSettings {
                parity,
                max_concurrent_requests: peer_settings.max_concurrent_requests,
            },
            metadata: vec![],
            setup: Box::new(move |handle| {
                let connection_store = store.clone();
                let dispatcher = StoreDispatcher::new(connection_store.clone());
                moire::task::spawn(
                    async move {
                        let mut driver = Driver::new(handle, dispatcher);
                        if wants_upstream {
                            connection_store.set_upstream(Arc::new(vx_services::StoreClient::new(
                                driver.caller(),
                            )));
                        }
                        driver.run().await;
                    }
                    .named("store_virtual_driver"),
                );
            }),
        })
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
    // If spawned by parent, die when parent dies
    ur_taking_me_with_you::die_with_parent();

    // Initialize tracing
    vx_tracing::init_service("vx_store=info");

    // Initialize moire instrumentation
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
        moire::task::spawn(
            async move {
                tracing::info!("Booting vx-store");
                match async {
                    let storage = CasStore::new(FsStorage::new(root));
                    storage.init().await?;
                    let store = StoreService::new(storage);
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
            }
            .named("store_boot"),
        );
    }

    let boot_service = gate.boot_service();

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        tracing::info!("accepted connection from {peer_addr}");
        let boot_service = boot_service.clone();

        moire::task::spawn(
            async move {
                tracing::debug!("New connection from {}", peer_addr);

                // Root connection gets Boot service only.
                let (reader, writer) = tokio::io::split(stream);
                let conduit = BareConduit::<vox_types::MessageFamily, _>::new(StreamLink::new(
                    reader, writer,
                ));
                let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                let (caller, session_handle) = match acceptor(conduit)
                    .on_connection(StoreVirtualAcceptor {
                        boot_service: boot_service.clone(),
                    })
                    .establish::<vox_core::NoopCaller>(boot_dispatcher)
                    .await
                {
                    Ok(parts) => parts,
                    Err(e) => {
                        tracing::debug!("Handshake failed for {}: {e}", peer_addr);
                        return;
                    }
                };

                tracing::debug!("Connection from {} established", peer_addr);
                vx_io::net::park_session(caller, session_handle).await;
            }
            .named("store_connection"),
        );
    }
}

impl StoreService {
    fn new(storage: CasStore) -> Self {
        Self {
            inner: Arc::new(StoreServiceInner {
                storage,
                toolchain_manager: ToolchainManager::new(),
                registry_manager: RegistryManager::new(),
                download_semaphore: Arc::new(tokio::sync::Semaphore::new(32)),
                rust_component_tarballs: moire::sync::SyncMutex::new(
                    "store/rust_component_tarballs",
                    std::collections::HashMap::new(),
                ),
                rust_component_tarball_flights: moire::sync::SyncMutex::new(
                    "store/rust_component_tarball_flights",
                    std::collections::HashMap::new(),
                ),
                rust_component_trees: moire::sync::SyncMutex::new(
                    "store/rust_component_trees",
                    std::collections::HashMap::new(),
                ),
                rust_component_tree_flights: moire::sync::SyncMutex::new(
                    "store/rust_component_tree_flights",
                    std::collections::HashMap::new(),
                ),
                present_chunks: moire::sync::SyncMutex::new(
                    "store/present_chunks",
                    std::collections::HashSet::new(),
                ),
                present_nodes: moire::sync::SyncMutex::new(
                    "store/present_nodes",
                    std::collections::HashSet::new(),
                ),
            }),
            upstream: Arc::new(moire::sync::SyncMutex::new("store/upstream", None)),
        }
    }
}
