//! VFS daemon for Vixen build system.
//!
//! Manages a FUSE-backed virtual filesystem that serves content from CAS
//! (vx-store) and collects build outputs.

mod platform;
mod protocol;
mod service;
mod vfs;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

use camino::Utf8PathBuf;
use facet::Facet;
use figue as args;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::sync::Arc;
use vixenfs_proto::{
    CreateResult, GetAttributesResult, ItemId, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, Vfs, VfsResult, WriteResult,
};
use vx_io::net::{Endpoint, Listener};
use vx_store_proto::StoreClient;
use vx_vfs::{VfsConfig, VfsStart};

use crate::protocol::VfsdDispatcher;
use crate::service::VfsdService;
use crate::vfs::CasVfs;

pub type CasCaller = roam_stream::Client<vx_io::net::EndpointConnector, NoDispatcher>;

/// Wrapper for CasVfs that implements Vfs by delegating to the inner CasVfs
#[derive(Clone)]
struct SharedVfs(Arc<CasVfs>);

impl Vfs for SharedVfs {
    async fn lookup(&self, cx: &roam::Context, parent_id: ItemId, name: String) -> LookupResult {
        self.0.lookup(cx, parent_id, name).await
    }

    async fn get_attributes(&self, cx: &roam::Context, item_id: ItemId) -> GetAttributesResult {
        self.0.get_attributes(cx, item_id).await
    }

    async fn read_dir(&self, cx: &roam::Context, item_id: ItemId, cursor: u64) -> ReadDirResult {
        self.0.read_dir(cx, item_id, cursor).await
    }

    async fn read(&self, cx: &roam::Context, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        self.0.read(cx, item_id, offset, len).await
    }

    async fn write(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        offset: u64,
        data: Vec<u8>,
    ) -> WriteResult {
        self.0.write(cx, item_id, offset, data).await
    }

    async fn create(
        &self,
        cx: &roam::Context,
        parent_id: ItemId,
        name: String,
        item_type: vixenfs_proto::ItemType,
    ) -> CreateResult {
        self.0.create(cx, parent_id, name, item_type).await
    }

    async fn delete(&self, cx: &roam::Context, item_id: ItemId) -> VfsResult {
        self.0.delete(cx, item_id).await
    }

    async fn rename(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        new_parent_id: ItemId,
        new_name: String,
    ) -> VfsResult {
        self.0.rename(cx, item_id, new_parent_id, new_name).await
    }

    async fn set_attributes(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        params: SetAttributesParams,
    ) -> VfsResult {
        self.0.set_attributes(cx, item_id, params).await
    }

    async fn ping(&self, _cx: &roam::Context) -> String {
        "vx-vfsd".to_string()
    }
}

/// vx-vfsd - VFS daemon for Vixen build system
#[derive(Facet, Debug)]
struct Args {
    /// Sentinel mode: clean up mount on parent death
    #[facet(args::named)]
    sentinel: Option<Utf8PathBuf>,

    /// RPC listen address (e.g., "127.0.0.1:0" for random port)
    #[facet(args::named, default = "127.0.0.1:0")]
    listen: String,

    /// FUSE mount point
    #[facet(args::named)]
    mount: Utf8PathBuf,

    /// vx-store RPC address
    #[facet(args::named)]
    store: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[tokio::main]
async fn main() -> Result<()> {
    vx_tracing::init_service("vx-vfsd=debug");

    // Die when parent dies
    ur_taking_me_with_you::die_with_parent();

    let args: Args = args::Driver::new(
        args::builder::<Args>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| h.program_name("vx-vfsd").version(env!("CARGO_PKG_VERSION")))
            .build(),
    )
    .run()
    .unwrap();

    if let Some(mount_path) = args.sentinel {
        platform::run_sentinel(&mount_path).await?;
        return Ok(());
    }

    tracing::info!(
        listen = %args.listen,
        mount = %args.mount,
        store = %args.store,
        "starting vx-vfsd"
    );

    // 1. Connect to vx-store
    tracing::info!("Connecting to vx-store at {}", args.store);
    let cas = connect_to_store(&args.store).await?;
    tracing::info!("Connected to vx-store");

    let cas = Arc::new(cas);

    // 2. Create VFS with optional L2 disk blob cache
    let disk_cache_dir = std::env::var("VX_HOME")
        .ok()
        .map(|vx_home| Utf8PathBuf::from(vx_home).join("cache/blobs/blake3"));
    if let Some(ref dir) = disk_cache_dir {
        tracing::info!(%dir, "L2 disk blob cache enabled");
    }
    let vfs = Arc::new(CasVfs::new(cas.clone(), disk_cache_dir));

    // 3. Bind RPC listener before FUSE mount
    let listen_endpoint = Endpoint::parse(&args.listen)?;
    let vfsd_listener = Listener::bind(&listen_endpoint).await.map_err(|e| {
        format!(
            "Failed to bind vx-vfsd RPC socket at {}: {}",
            listen_endpoint, e
        )
    })?;
    let actual_addr = vfsd_listener.local_addr()?;
    tracing::info!("vx-vfsd RPC bound on {}", actual_addr);

    // Print the actual address to stdout so vxd can parse it
    println!("{}", actual_addr);

    // 4. Clean up stuck mounts BEFORE creating directory
    platform::cleanup_stuck_mount(&args.mount).await?;

    // 5. Ensure mount directory exists (skip for system-managed paths)
    if platform::should_create_mount_dir(&args.mount) {
        tokio::fs::create_dir_all(args.mount.as_std_path())
            .await
            .map_err(|e| format!("failed to create mount directory {}: {}", args.mount, e))?;
    }

    // 6. Mount VFS
    tracing::info!("Mounting VFS at {}", args.mount);
    match vx_vfs::mount(
        VfsConfig {
            mount_path: args.mount.clone(),
        },
        SharedVfs(vfs.clone()),
    )
    .await
    {
        Ok(VfsStart::Mounted {
            mount_path,
            mut vfs_dead,
            ..
        }) => {
            tracing::info!("VFS mounted at {}", mount_path);

            // Set up platform signal handler for diagnostics
            platform::setup_diagnostics_signal(vfs.clone());

            // Spawn watcher for VFS death
            let vfs_mount_path = mount_path.clone();
            tokio::spawn(async move {
                // Wait for VFS to report death
                loop {
                    if vfs_dead.changed().await.is_err() || *vfs_dead.borrow() {
                        break;
                    }
                }
                tracing::error!("VFS session died unexpectedly at {}", vfs_mount_path);
                std::process::exit(1);
            });
        }
        Ok(VfsStart::Unsupported { platform }) => {
            return Err(format!("VFS not supported on platform {}", platform).into());
        }
        Err(error) => {
            eprintln!("VFS mount failed: {}", error);
            for diagnostic in error.diagnostics() {
                eprintln!("  {}: {}", diagnostic.code, diagnostic.summary);
                if let Some(ref detail) = diagnostic.detail {
                    eprintln!("    {}", detail);
                }
            }
            return Err(format!("VFS mount failed: {}", error).into());
        }
    }

    // 7. Start accepting RPC connections
    tracing::info!("Starting vx-vfsd RPC server");
    let vfsd_service = VfsdService::new(vfs);

    loop {
        match vfsd_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let service = vfsd_service.clone();
                tokio::spawn(async move {
                    tracing::debug!("vx-vfsd connection from {}", peer_addr);
                    let dispatcher = VfsdDispatcher::new(service);
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
                        tracing::warn!("Failed to accept vx-vfsd connection from {}", peer_addr);
                        return;
                    };
                    tracing::debug!("vx-vfsd session established with {}", peer_addr);
                    if let Err(e) = driver.run().await {
                        tracing::debug!("vx-vfsd session error with {}: {}", peer_addr, e);
                    }
                    tracing::debug!("vx-vfsd session closed with {}", peer_addr);
                });
            }
            Err(e) => {
                tracing::error!("vx-vfsd accept error: {}", e);
            }
        }
    }
}

async fn connect_to_store(endpoint_str: &str) -> Result<StoreClient<CasCaller>> {
    let endpoint = Endpoint::parse(endpoint_str)?;
    let caller = vx_io::net::reconnecting_client(endpoint.clone(), NoDispatcher);

    // Wait for initial connection
    caller
        .handle()
        .await
        .map_err(|e| format!("failed to connect to vx-store at {}: {}", endpoint, e))?;

    Ok(StoreClient::new(caller))
}

