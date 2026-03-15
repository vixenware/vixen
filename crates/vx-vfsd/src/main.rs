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
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use std::sync::Arc;
use vixenfs_proto::{
    CreateResult, GetAttributesResult, ItemId, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, Vfs, VfsDiskNotify, VfsResult, WriteResult,
};
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_store_proto::StoreClient;
use vx_vfs::{VfsConfig, VfsStart};

use crate::protocol::VfsdDispatcher;
use crate::service::VfsdService;
use crate::vfs::CasVfs;

pub type StoreCaller = roam_core::DriverCaller;

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
const MACOS_GLOBAL_MOUNT_PATH: &str = "/tmp/vx/vfs";

fn effective_mount_path(configured_mount: &Utf8PathBuf) -> Utf8PathBuf {
    if cfg!(target_os = "macos") {
        Utf8PathBuf::from(MACOS_GLOBAL_MOUNT_PATH)
    } else {
        configured_mount.clone()
    }
}

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

impl SharedVfs {
    async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult {
        if parent_id == ROOT_ITEM_ID && name == self.canary.name {
            tracing::trace!(parent_id, %name, item_id = self.canary.item_id, "canary lookup hit");
            return LookupResult {
                attrs: self.canary_attrs(),
                error: vixenfs_proto::errno::OK,
            };
        }
        tracing::trace!(parent_id, %name, "lookup");
        CasVfs::lookup(self.inner.as_ref(), parent_id, name).await
    }

    async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult {
        if item_id == self.canary.item_id {
            tracing::trace!(item_id, "canary get_attributes hit");
            return GetAttributesResult {
                attrs: self.canary_attrs(),
                error: vixenfs_proto::errno::OK,
            };
        }
        tracing::trace!(item_id, "get_attributes");
        CasVfs::get_attributes(self.inner.as_ref(), item_id).await
    }

    async fn read_dir(&self, item_id: ItemId, cookie: u64, verifier: u64) -> ReadDirResult {
        tracing::trace!(item_id, cookie, verifier, "read_dir");
        CasVfs::read_dir(self.inner.as_ref(), item_id, cookie, verifier).await
    }

    async fn read(&self, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
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
        CasVfs::read(self.inner.as_ref(), item_id, offset, len).await
    }

    async fn write(&self, item_id: ItemId, offset: u64, data: Vec<u8>) -> WriteResult {
        tracing::trace!(item_id, offset, len = data.len(), "write");
        CasVfs::write(self.inner.as_ref(), item_id, offset, data).await
    }

    async fn create(
        &self,
        parent_id: ItemId,
        name: String,
        item_type: vixenfs_proto::ItemType,
    ) -> CreateResult {
        tracing::trace!(parent_id, %name, ?item_type, "create");
        CasVfs::create(self.inner.as_ref(), parent_id, name, item_type).await
    }

    async fn delete(&self, item_id: ItemId) -> VfsResult {
        tracing::trace!(item_id, "delete");
        CasVfs::delete(self.inner.as_ref(), item_id).await
    }

    async fn rename(&self, item_id: ItemId, new_parent_id: ItemId, new_name: String) -> VfsResult {
        tracing::trace!(item_id, new_parent_id, %new_name, "rename");
        CasVfs::rename(self.inner.as_ref(), item_id, new_parent_id, new_name).await
    }

    async fn set_attributes(&self, item_id: ItemId, params: SetAttributesParams) -> VfsResult {
        tracing::trace!(item_id, "set_attributes");
        CasVfs::set_attributes(self.inner.as_ref(), item_id, params).await
    }

    async fn ping(&self) -> String {
        tracing::trace!("ping");
        "vx-vfsd".to_string()
    }
}

impl Vfs for SharedVfs {
    async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult {
        SharedVfs::lookup(self, parent_id, name).await
    }

    async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult {
        SharedVfs::get_attributes(self, item_id).await
    }

    async fn read_dir(&self, item_id: ItemId, cookie: u64, verifier: u64) -> ReadDirResult {
        SharedVfs::read_dir(self, item_id, cookie, verifier).await
    }

    async fn read(&self, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        SharedVfs::read(self, item_id, offset, len).await
    }

    async fn write(&self, item_id: ItemId, offset: u64, data: Vec<u8>) -> WriteResult {
        SharedVfs::write(self, item_id, offset, data).await
    }

    async fn create(
        &self,
        parent_id: ItemId,
        name: String,
        item_type: vixenfs_proto::ItemType,
    ) -> CreateResult {
        SharedVfs::create(self, parent_id, name, item_type).await
    }

    async fn delete(&self, item_id: ItemId) -> VfsResult {
        SharedVfs::delete(self, item_id).await
    }

    async fn rename(&self, item_id: ItemId, new_parent_id: ItemId, new_name: String) -> VfsResult {
        SharedVfs::rename(self, item_id, new_parent_id, new_name).await
    }

    async fn set_attributes(&self, item_id: ItemId, params: SetAttributesParams) -> VfsResult {
        SharedVfs::set_attributes(self, item_id, params).await
    }

    async fn ping(&self) -> String {
        SharedVfs::ping(self).await
    }
}

impl VfsDiskNotify for SharedVfs {
    fn on_disk_file_created(&self, path: &str, is_directory: bool) {
        self.inner.on_disk_file_created(path, is_directory)
    }

    fn on_disk_file_deleted(&self, path: &str) {
        self.inner.on_disk_file_deleted(path)
    }

    fn on_disk_file_renamed(&self, source: &str, destination: &str) {
        self.inner.on_disk_file_renamed(source, destination)
    }
}

#[derive(Clone)]
struct VfsdVirtualAcceptor {
    boot_service: vx_io::boot::BootService<VfsdService>,
}

impl ConnectionAcceptor for VfsdVirtualAcceptor {
    fn accept(
        &self,
        _conn_id: roam_types::ConnectionId,
        peer_settings: &ConnectionSettings,
        _metadata: &[roam_types::MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        let Some(service) = self.boot_service.gate().service().cloned() else {
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
                let dispatcher = VfsdDispatcher::new(service);
                moire::task::spawn(
                    async move {
                        let mut driver = Driver::new(handle, dispatcher);
                        driver.run().await;
                    }
                    .named("vfsd_virtual_driver"),
                );
            }),
        })
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

    /// VFS mount point (defaults to stable global path)
    #[facet(args::named, default = "/tmp/vx/vfs")]
    mount: Utf8PathBuf,

    /// vx-store RPC address
    #[facet(args::named)]
    store: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    vx_tracing::init_service("vx-vfsd=info,vx_vfs=info");

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

    let mount = effective_mount_path(&args.mount);
    if cfg!(target_os = "macos") && mount != args.mount {
        tracing::info!(
            configured_mount = %args.mount,
            mount = %mount,
            "ignoring configured mount path on macOS; using stable global mount path",
        );
    }

    tracing::debug!(
        listen = %args.listen,
        mount = %mount,
        store = %args.store,
        "starting vx-vfsd"
    );

    // Bind RPC listener BEFORE booting — so parent can connect immediately.
    let listen_endpoint = Endpoint::parse(&args.listen)?;
    let mut vfsd_listener = Listener::bind(&listen_endpoint).await.map_err(|e| {
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
        let mount = mount.clone();
        moire::task::spawn(
            async move {
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
            }
            .named("vfsd_boot"),
        );
    }

    let boot_service = gate.boot_service();

    loop {
        match vfsd_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let boot_service = boot_service.clone();
                moire::task::spawn(
                    async move {
                        tracing::debug!("vx-vfsd connection from {}", peer_addr);

                        // Root connection gets Boot service only.
                        let (reader, writer) = tokio::io::split(stream);
                        let conduit = BareConduit::<roam_types::MessageFamily, _>::new(
                            StreamLink::new(reader, writer),
                        );
                        let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                        let (caller, session_handle) = match acceptor(conduit)
                            .on_connection(VfsdVirtualAcceptor {
                                boot_service: boot_service.clone(),
                            })
                            .establish::<roam_core::NoopCaller>(boot_dispatcher)
                            .await
                        {
                            Ok(parts) => parts,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to accept vx-vfsd connection from {}: {e}",
                                    peer_addr
                                );
                                return;
                            }
                        };

                        tracing::debug!("vx-vfsd session established with {}", peer_addr);
                        vx_io::net::park_session(caller, session_handle).await;
                    }
                    .named("vfsd_connection"),
                );
            }
            Err(e) => {
                tracing::error!("vx-vfsd accept error: {}", e);
            }
        }
    }
}

/// Connect to vx-store using the boot protocol.
async fn connect_to_store(
    endpoint_str: &str,
) -> Result<(
    StoreClient,
    roam_core::DriverCaller,
    roam_core::SessionHandle,
)> {
    let endpoint = Endpoint::parse(endpoint_str)?;
    let (root_caller, session_handle) = vx_io::net::connect_session(&endpoint)
        .await
        .map_err(|e| format!("failed to connect to vx-store at {}: {}", endpoint, e))?;
    let virtual_caller = vx_io::boot::boot_and_connect(&root_caller, &session_handle)
        .await
        .map_err(|e| format!("vx-store boot failed: {e:#}"))?;

    Ok((
        StoreClient::new(virtual_caller),
        root_caller,
        session_handle,
    ))
}

/// Full boot sequence: connect to store, create VFS, mount, smoke test.
async fn boot_vfsd(store_addr: &str, mount: &Utf8PathBuf) -> Result<VfsdService> {
    // 1. Connect to vx-store (with boot protocol)
    tracing::debug!("Connecting to vx-store at {store_addr}");
    let (cas, store_root_caller, store_session_handle) = connect_to_store(store_addr).await?;
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
            moire::fs::create_dir_all(mount.as_std_path())
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
                        moire::fs::read(canary_fs_path.as_std_path()),
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

                let vfs_mount_path = mount_path.clone();
                moire::task::spawn(async move {
                    loop {
                        if vfs_dead.changed().await.is_err() || *vfs_dead.borrow() {
                            break;
                        }
                    }
                    tracing::error!("VFS session died unexpectedly at {}", vfs_mount_path);
                    std::process::exit(1);
                }.named("vfs_death_watcher"));
            }
            Ok(VfsStart::Unsupported { platform }) => {
                return Err(format!("VFS not supported on platform {platform}").into());
            }
            Err(error) => {
                use owo_colors::OwoColorize;

                let mut msg = format!("{}", format!("{error}").blue());
                for diagnostic in error.diagnostics() {
                    for action in &diagnostic.actions {
                        if let vx_vfs::VfsAction::RunCommand(cmd) = action {
                            msg.push_str(&format!("\n\nTry: \n  $ {}", cmd.yellow()));
                        }
                    }
                    if let Some(ref detail) = diagnostic.detail {
                        msg.push_str(&"\n\nDetails:".dimmed().to_string());
                        for line in detail.lines() {
                            msg.push_str(&format!(
                                "\n  {}",
                                line.dimmed()
                            ));
                        }
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
        if let Err(e) = moire::fs::write(&temp_path, &diagnostic_content).await {
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

    Ok(VfsdService::new_with_store_connection(
        vfs,
        Arc::new(store_root_caller),
        Arc::new(store_session_handle),
    ))
}
