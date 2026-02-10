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
use roam_stream::{HandshakeConfig, accept};
use std::sync::Arc;
use vx_io::net::{Endpoint, Listener};
use vx_store_proto::StoreDispatcher;

#[derive(Debug)]
struct Args {
    /// Storage root directory (typically .vx/store)
    root: Utf8PathBuf,

    /// VX home directory (typically .vx)
    vx_home: Utf8PathBuf,

    /// Bind endpoint (host:port or unix:/path/to/socket)
    bind: Endpoint,
}

impl Args {
    fn from_env() -> Result<Self> {
        let vx_home = std::env::var("VX_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("HOME not set");
            format!("{}/.vx", home)
        });
        let vx_home = Utf8PathBuf::from(&vx_home);
        let root = vx_home.join("store");

        // VX_STORE can be:
        // - host:port (TCP)
        // - unix:/path/to/socket
        // - /path/to/socket (Unix socket shorthand)
        // - unset: defaults to Unix socket at $VX_HOME/store.sock
        let bind = match std::env::var("VX_STORE") {
            Ok(v) => Endpoint::parse(&v)?,
            Err(_) => {
                // Default to Unix socket in VX_HOME
                #[cfg(unix)]
                {
                    vx_io::net::default_unix_endpoint(&vx_home, "store")
                }
                #[cfg(not(unix))]
                {
                    Endpoint::parse("127.0.0.1:9002")?
                }
            }
        };

        Ok(Args {
            root,
            vx_home,
            bind,
        })
    }
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // If spawned by parent, die when parent dies
    ur_taking_me_with_you::die_with_parent();

    // Initialize tracing
    vx_tracing::init_service("vx_store=info");

    tracing::info!("hello from vixen-store");

    let args = Args::from_env()?;

    // Build storage backend
    let storage = match std::env::var("VX_CAS_BASE") {
        Ok(base) => CasStore::with_base(
            FsStorage::new(args.root.clone()),
            FsStorage::new(Utf8PathBuf::from(base)),
        ),
        Err(_) => CasStore::new(FsStorage::new(args.root.clone())),
    };

    // Initialize storage directories
    tracing::info!("Initializing store at {}", args.root);
    storage.init().await?;

    // Initialize store service
    let store = StoreService::new(storage, args.vx_home);

    // Start server (TCP or Unix socket)
    let listener = Listener::bind(&args.bind).await?;
    tracing::info!("store listening on {}", args.bind);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let store = store.clone();

        tokio::spawn(async move {
            tracing::debug!("New connection from {}", peer_addr);

            let dispatcher = StoreDispatcher::new(store);
            let Ok((_handle, _incoming, driver)) = accept(
                stream,
                HandshakeConfig {
                    max_payload_size: 256 * 1024 * 1024, // 256 MiB
                    ..Default::default()
                },
                dispatcher,
            )
            .await
            else {
                tracing::debug!("Handshake failed for {}", peer_addr);
                return;
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
                rust_component_tarballs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
                rust_component_trees: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            }),
        }
    }
}
