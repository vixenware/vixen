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

/// Wrapper for CasVfs that implements Vfs by delegating to the inner CasVfs.
///
/// Also serves a synthetic "canary" file at the root for smoke-testing:
/// after mounting, vx-vfsd reads the canary through the real filesystem
/// to verify the mount is actually working end-to-end.
#[derive(Clone)]
struct SharedVfs {
    inner: Arc<CasVfs>,
    canary: Arc<Canary>,
}

struct Canary {
    /// Filename of the canary (e.g., ".vfs-smoke-ab3f...")
    name: String,
    /// Random content to verify round-trip
    content: Vec<u8>,
    /// Fake item ID for the canary file (u64::MAX — will never collide with real IDs)
    item_id: ItemId,
}

const CANARY_ITEM_ID: ItemId = u64::MAX;
const ROOT_ITEM_ID: ItemId = 1;

impl SharedVfs {
    fn new(inner: Arc<CasVfs>) -> Self {
        let name = format!(".vfs-smoke-{:016x}", fastrand::u64(..));
        let content: Vec<u8> = (0..32).map(|_| fastrand::u8(..)).collect();
        Self {
            inner,
            canary: Arc::new(Canary {
                name,
                content,
                item_id: CANARY_ITEM_ID,
            }),
        }
    }

    fn canary_attrs(&self) -> vixenfs_proto::ItemAttributes {
        vixenfs_proto::ItemAttributes {
            item_id: self.canary.item_id,
            item_type: vixenfs_proto::ItemType::File,
            size: self.canary.content.len() as u64,
            modified_time: 0,
            created_time: 0,
            mode: vixenfs_proto::mode::FILE_REGULAR,
        }
    }
}

impl Vfs for SharedVfs {
    async fn lookup(&self, cx: &roam::Context, parent_id: ItemId, name: String) -> LookupResult {
        if parent_id == ROOT_ITEM_ID && name == self.canary.name {
            return LookupResult {
                attrs: self.canary_attrs(),
                error: vixenfs_proto::errno::OK,
            };
        }
        self.inner.lookup(cx, parent_id, name).await
    }

    async fn get_attributes(&self, cx: &roam::Context, item_id: ItemId) -> GetAttributesResult {
        if item_id == self.canary.item_id {
            return GetAttributesResult {
                attrs: self.canary_attrs(),
                error: vixenfs_proto::errno::OK,
            };
        }
        self.inner.get_attributes(cx, item_id).await
    }

    async fn read_dir(&self, cx: &roam::Context, item_id: ItemId, cursor: u64) -> ReadDirResult {
        self.inner.read_dir(cx, item_id, cursor).await
    }

    async fn read(&self, cx: &roam::Context, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        if item_id == self.canary.item_id {
            let content = &self.canary.content;
            let start = (offset as usize).min(content.len());
            let end = (start + len as usize).min(content.len());
            return ReadResult {
                data: content[start..end].to_vec(),
                error: vixenfs_proto::errno::OK,
            };
        }
        self.inner.read(cx, item_id, offset, len).await
    }

    async fn write(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        offset: u64,
        data: Vec<u8>,
    ) -> WriteResult {
        self.inner.write(cx, item_id, offset, data).await
    }

    async fn create(
        &self,
        cx: &roam::Context,
        parent_id: ItemId,
        name: String,
        item_type: vixenfs_proto::ItemType,
    ) -> CreateResult {
        self.inner.create(cx, parent_id, name, item_type).await
    }

    async fn delete(&self, cx: &roam::Context, item_id: ItemId) -> VfsResult {
        self.inner.delete(cx, item_id).await
    }

    async fn rename(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        new_parent_id: ItemId,
        new_name: String,
    ) -> VfsResult {
        self.inner
            .rename(cx, item_id, new_parent_id, new_name)
            .await
    }

    async fn set_attributes(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        params: SetAttributesParams,
    ) -> VfsResult {
        self.inner.set_attributes(cx, item_id, params).await
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
    vx_tracing::init_service("vx-vfsd=debug");

    // Die when parent dies
    ur_taking_me_with_you::die_with_parent();

    // Initialize peeps instrumentation
    peeps::init();
    peeps::install_sigusr1("vx-vfsd");

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
    let shared_vfs = SharedVfs::new(vfs.clone());
    let canary = shared_vfs.canary.clone();
    match vx_vfs::mount(
        VfsConfig {
            mount_path: args.mount.clone(),
        },
        shared_vfs,
    )
    .await
    {
        Ok(VfsStart::Mounted {
            mount_path,
            mut vfs_dead,
            ..
        }) => {
            tracing::info!("VFS mounted at {}", mount_path);

            // Smoke test: read the canary file through the real filesystem.
            // This proves the FUSE/FSKit mount is actually serving files.
            let canary_fs_path = mount_path.join(&canary.name);
            let smoke_timeout = std::time::Duration::from_secs(5);
            let read_result =
                tokio::time::timeout(smoke_timeout, tokio::fs::read(canary_fs_path.as_std_path()))
                    .await;
            match read_result {
                Ok(Ok(actual)) if actual == canary.content => {
                    tracing::info!("VFS smoke test passed");
                }
                Ok(Ok(actual)) => {
                    tracing::error!(
                        expected_len = canary.content.len(),
                        actual_len = actual.len(),
                        "VFS smoke test: content mismatch"
                    );
                    platform::collect_smoke_test_diagnostics(&mount_path).await;
                    std::process::exit(1);
                }
                Ok(Err(e)) => {
                    tracing::error!(%e, path = %canary_fs_path, "VFS smoke test: read failed");
                    platform::collect_smoke_test_diagnostics(&mount_path).await;
                    std::process::exit(1);
                }
                Err(_) => {
                    tracing::error!(
                        path = %canary_fs_path,
                        timeout = ?smoke_timeout,
                        "VFS smoke test: timed out — mount is not responding"
                    );
                    platform::collect_smoke_test_diagnostics(&mount_path).await;
                    std::process::exit(1);
                }
            }

            // Set up platform signal handler for diagnostics
            platform::setup_diagnostics_signal(vfs.clone());

            // Spawn watcher for VFS death
            let vfs_mount_path = mount_path.clone();
            peeps::tasks::spawn_tracked("vfs_death_watcher", async move {
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
                peeps::tasks::spawn_tracked(format!("vfsd_connection_{}", peer_addr), async move {
                    tracing::debug!("vx-vfsd connection from {}", peer_addr);
                    let dispatcher = VfsdDispatcher::new(service);
                    let (_handle, _incoming, driver) = match accept(
                        stream,
                        HandshakeConfig {
                            max_payload_size: 256 * 1024 * 1024, // 256 MiB
                            name: Some("vx-vfsd".into()),
                            ..Default::default()
                        },
                        dispatcher,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to accept vx-vfsd connection from {}: {e}",
                                peer_addr
                            );
                            return;
                        }
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
    let caller = vx_io::net::reconnecting_client_named(
        endpoint.clone(),
        NoDispatcher,
        Some("vx-vfsd".into()),
    );

    // Wait for initial connection
    caller
        .handle()
        .await
        .map_err(|e| format!("failed to connect to vx-store at {}: {}", endpoint, e))?;

    Ok(StoreClient::new(caller))
}
