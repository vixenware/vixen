//! VFS daemon for Vixen build system.
//!
//! Manages a FUSE-backed virtual filesystem that serves content from CAS
//! (vx-store) and collects build outputs.

mod platform;
mod protocol;
mod service;
mod vfs;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

use camino::Utf8PathBuf;
use facet::Facet;
use figue as args;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::sync::Arc;
use vixenfs_proto::{
    CreateResult, GetAttributesResult, ItemId, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, Vfs, VfsResult, WriteResult,
};
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_store_proto::StoreClient;
use vx_vfs::{VfsConfig, VfsStart};

use crate::protocol::VfsdDispatcher;
use crate::service::VfsdService;
use crate::vfs::CasVfs;

pub type StoreCaller = roam_stream::ConnectionHandle;

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
    /// Fake item ID for the canary file (high value that won't collide with real IDs)
    item_id: ItemId,
}

/// Canary item ID — must be high enough to never collide with real CAS item IDs
/// (which start at 2) or the Swift-side virtual items (0xFFFF_FFFF_FFFE_xxxx),
/// but NOT u64::MAX because FSItem.Identifier(rawValue:) returns nil for that value.
const CANARY_ITEM_ID: ItemId = 0xFFFF_FFFF_FFFD_0001;
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
            tracing::trace!(parent_id, %name, item_id = self.canary.item_id, "canary lookup hit");
            return LookupResult {
                attrs: self.canary_attrs(),
                error: vixenfs_proto::errno::OK,
            };
        }
        tracing::trace!(parent_id, %name, "lookup");
        self.inner.lookup(cx, parent_id, name).await
    }

    async fn get_attributes(&self, cx: &roam::Context, item_id: ItemId) -> GetAttributesResult {
        if item_id == self.canary.item_id {
            tracing::trace!(item_id, "canary get_attributes hit");
            return GetAttributesResult {
                attrs: self.canary_attrs(),
                error: vixenfs_proto::errno::OK,
            };
        }
        tracing::trace!(item_id, "get_attributes");
        self.inner.get_attributes(cx, item_id).await
    }

    async fn read_dir(&self, cx: &roam::Context, item_id: ItemId, cursor: u64) -> ReadDirResult {
        tracing::trace!(item_id, cursor, "read_dir");
        self.inner.read_dir(cx, item_id, cursor).await
    }

    async fn read(&self, cx: &roam::Context, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        if item_id == self.canary.item_id {
            tracing::trace!(item_id, offset, len, "canary read hit");
            let content = &self.canary.content;
            let start = (offset as usize).min(content.len());
            let end = (start + len as usize).min(content.len());
            return ReadResult {
                data: content[start..end].to_vec(),
                error: vixenfs_proto::errno::OK,
            };
        }
        tracing::trace!(item_id, offset, len, "read");
        self.inner.read(cx, item_id, offset, len).await
    }

    async fn write(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        offset: u64,
        data: Vec<u8>,
    ) -> WriteResult {
        tracing::trace!(item_id, offset, len = data.len(), "write");
        self.inner.write(cx, item_id, offset, data).await
    }

    async fn create(
        &self,
        cx: &roam::Context,
        parent_id: ItemId,
        name: String,
        item_type: vixenfs_proto::ItemType,
    ) -> CreateResult {
        tracing::trace!(parent_id, %name, ?item_type, "create");
        self.inner.create(cx, parent_id, name, item_type).await
    }

    async fn delete(&self, cx: &roam::Context, item_id: ItemId) -> VfsResult {
        tracing::trace!(item_id, "delete");
        self.inner.delete(cx, item_id).await
    }

    async fn rename(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        new_parent_id: ItemId,
        new_name: String,
    ) -> VfsResult {
        tracing::trace!(item_id, new_parent_id, %new_name, "rename");
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
        tracing::trace!(item_id, "set_attributes");
        self.inner.set_attributes(cx, item_id, params).await
    }

    async fn ping(&self, _cx: &roam::Context) -> String {
        tracing::trace!("ping");
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
    vx_tracing::init_service("vx-vfsd=info");

    // Die when parent dies
    ur_taking_me_with_you::die_with_parent();

    // Initialize peeps instrumentation
    peeps::init();
    #[cfg(unix)]
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

    tracing::debug!(
        listen = %args.listen,
        mount = %args.mount,
        store = %args.store,
        "starting vx-vfsd"
    );

    // Bind RPC listener BEFORE booting — so parent can connect immediately.
    let listen_endpoint = Endpoint::parse(&args.listen)?;
    let vfsd_listener = Listener::bind(&listen_endpoint).await.map_err(|e| {
        format!(
            "Failed to bind vx-vfsd RPC socket at {}: {}",
            listen_endpoint, e
        )
    })?;
    let actual_addr = vfsd_listener.local_addr()?;
    tracing::debug!("vx-vfsd RPC bound on {}", actual_addr);

    // Print the actual address to stdout so parent can parse it.
    println!("{actual_addr}");

    // Set up boot gate — root connections get Boot service only.
    let (gate, boot_tx) = BootGate::<VfsdService>::new();

    // Boot in background: connect to store, create VFS, mount, smoke test.
    {
        let gate = Arc::clone(&gate);
        let store_addr = args.store.clone();
        let mount = args.mount.clone();
        peeps::tasks::spawn_tracked("vfsd_boot", async move {
            tracing::info!("Booting vx-vfsd");
            match boot_vfsd(&store_addr, &mount).await {
                Ok(service) => {
                    tracing::info!("vx-vfsd boot succeeded");
                    gate.set_service(service);
                    boot_tx.ready();
                }
                Err(e) => {
                    tracing::error!("vx-vfsd boot failed: {e:#}");
                    boot_tx.failed(format!("{e:#}"));
                }
            }
        });
    }

    let boot_service = gate.boot_service();

    loop {
        match vfsd_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let boot_service = boot_service.clone();
                peeps::tasks::spawn_tracked(format!("vfsd_connection_{}", peer_addr), async move {
                    tracing::debug!("vx-vfsd connection from {}", peer_addr);

                    // Root connection gets Boot service only.
                    let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                    let (_handle, mut incoming, driver) = match accept(
                        stream,
                        HandshakeConfig {
                            max_payload_size: 256 * 1024 * 1024, // 256 MiB
                            name: Some("vx-vfsd".into()),
                            ..Default::default()
                        },
                        boot_dispatcher,
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

                    // Drive the connection in the background.
                    let driver_handle = peeps::tasks::spawn_tracked(
                        format!("vfsd_driver_{}", peer_addr),
                        async move { driver.run().await },
                    );

                    // Accept virtual connections — each gets a VfsdDispatcher.
                    while let Some(conn) = incoming.recv().await {
                        let Some(service) = boot_service.gate().service() else {
                            conn.reject("boot not complete".into(), vec![]);
                            continue;
                        };
                        let dispatcher = VfsdDispatcher::new(service.clone());
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
                    tracing::debug!("vx-vfsd session closed with {}", peer_addr);
                });
            }
            Err(e) => {
                tracing::error!("vx-vfsd accept error: {}", e);
            }
        }
    }
}

/// Connect to vx-store using the boot protocol.
async fn connect_to_store(endpoint_str: &str) -> Result<StoreClient<StoreCaller>> {
    let endpoint = Endpoint::parse(endpoint_str)?;
    let caller = vx_io::net::reconnecting_client_named(
        endpoint.clone(),
        NoDispatcher,
        Some("vx-vfsd".into()),
    );

    let root_handle = caller
        .handle()
        .await
        .map_err(|e| format!("failed to connect to vx-store at {}: {}", endpoint, e))?;

    let virtual_handle = vx_io::boot::boot_and_connect(&root_handle, None)
        .await
        .map_err(|e| format!("vx-store boot failed: {e:#}"))?;

    Ok(StoreClient::new(virtual_handle))
}

/// Full boot sequence: connect to store, create VFS, mount, smoke test.
async fn boot_vfsd(store_addr: &str, mount: &Utf8PathBuf) -> Result<VfsdService> {
    // 1. Connect to vx-store (with boot protocol)
    tracing::debug!("Connecting to vx-store at {store_addr}");
    let cas = connect_to_store(store_addr).await?;
    tracing::debug!("Connected to vx-store");
    let cas = Arc::new(cas);

    // 2. Create VFS with optional L2 disk blob cache
    let disk_cache_dir = std::env::var("VX_HOME")
        .ok()
        .map(|vx_home| Utf8PathBuf::from(vx_home).join("cache/blobs/blake3"));
    if let Some(ref dir) = disk_cache_dir {
        tracing::debug!(%dir, "L2 disk blob cache enabled");
    }
    let vfs = Arc::new(CasVfs::new(cas.clone(), disk_cache_dir, mount.clone()));

    // 3. Clean up, create mount dir, mount VFS, smoke test.
    let mount_result: Result<()> = async {
        platform::cleanup_stuck_mount(mount).await?;

        if platform::should_create_mount_dir(mount) {
            tokio::fs::create_dir_all(mount.as_std_path())
                .await
                .map_err(|e| format!("failed to create mount directory {mount}: {e}"))?;
        }

        tracing::debug!("Mounting VFS at {mount}");
        let shared_vfs = SharedVfs::new(vfs.clone());
        let canary = shared_vfs.canary.clone();
        match vx_vfs::mount(
            VfsConfig {
                mount_path: mount.clone(),
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
                tracing::debug!("VFS mounted at {mount_path}");

                // Smoke test: read the canary file through the real filesystem.
                let canary_fs_path = mount_path.join(&canary.name);
                tracing::debug!("Trying to read canary from {canary_fs_path}");

                const MAX_ATTEMPTS: u32 = 10;
                const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
                let smoke_timeout = std::time::Duration::from_secs(5);

                let mut smoke_err = None;
                for attempt in 1..=MAX_ATTEMPTS {
                    let read_result = tokio::time::timeout(
                        smoke_timeout,
                        tokio::fs::read(canary_fs_path.as_std_path()),
                    )
                    .await;

                    match read_result {
                        Ok(Ok(actual)) if actual == canary.content => {
                            tracing::debug!(attempt, "VFS smoke test passed");
                            smoke_err = None;
                            break;
                        }
                        Ok(Ok(actual)) => {
                            smoke_err = Some(format!(
                                "content mismatch at {} (expected {} bytes, got {} bytes)",
                                canary_fs_path,
                                canary.content.len(),
                                actual.len(),
                            ));
                        }
                        Ok(Err(e)) => {
                            smoke_err = Some(format!("failed to read {}: {e}", canary_fs_path));
                        }
                        Err(_) => {
                            smoke_err = Some(format!(
                                "timed out after {smoke_timeout:?} reading {} — mount is not responding",
                                canary_fs_path,
                            ));
                        }
                    }

                    if attempt < MAX_ATTEMPTS {
                        tracing::debug!(
                            attempt,
                            error = smoke_err.as_deref().unwrap_or("unknown"),
                            "canary read failed, retrying"
                        );
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }

                if let Some(reason) = smoke_err {
                    return Err(format!("VFS smoke test failed: {reason}").into());
                }

                platform::setup_diagnostics_signal(vfs.clone(), mount_path.clone());

                let vfs_mount_path = mount_path.clone();
                peeps::tasks::spawn_tracked("vfs_death_watcher", async move {
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
                return Err(format!("VFS not supported on platform {platform}").into());
            }
            Err(error) => {
                let mut msg = format!("VFS mount failed: {error}");
                for diagnostic in error.diagnostics() {
                    msg.push_str(&format!("\n  {}: {}", diagnostic.code, diagnostic.summary));
                    if let Some(ref detail) = diagnostic.detail {
                        msg.push_str(&format!("\n    {detail}"));
                    }
                }
                return Err(msg.into());
            }
        }
        Ok(())
    }
    .await;

    if let Err(error) = mount_result {
        use std::time::Instant;
        let before = Instant::now();
        let diagnostics = platform::collect_vfs_diagnostics(mount).await;
        let elapsed = before.elapsed();

        let temp_path = format!("/tmp/vfsd-diagnostics-{:016x}.txt", fastrand::u64(..));
        let diagnostic_content = format!("{error}\n{diagnostics}");
        let diagnostic_content = strip_ansi_escapes::strip_str(&diagnostic_content);
        if let Err(e) = tokio::fs::write(&temp_path, &diagnostic_content).await {
            tracing::error!("Failed to write VFS diagnostics to {}: {}", temp_path, e);
        } else {
            tracing::error!("==================================================");
            tracing::error!(
                "VFS diagnostics (gathered in {elapsed:?}) written to: {}",
                temp_path
            );
            tracing::error!("==================================================");
        }

        return Err(error);
    }

    Ok(VfsdService::new(vfs))
}
