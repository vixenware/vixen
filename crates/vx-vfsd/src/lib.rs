//! vx-vfsd - VFS daemon library.
//!
//! Exposes [`VfsdService`] and [`VfsdAcceptor`] for use by the daemon (in-process).

pub(crate) mod platform;
pub mod protocol;
pub(crate) mod service;
pub(crate) mod vfs;

pub use crate::protocol::VfsdDispatcher;
pub use crate::service::VfsdService;

use camino::Utf8PathBuf;
use eyre::Result;
use moire::task::FutureExt;
use std::sync::Arc;
use vx_services::{
    Blake3Hash, CasNode, ContentHash, CreateResult, DirectoryEntry, DirectoryEntryKind,
    DirectoryNode, GetAttributesResult, ItemId, LookupResult, NodeHash, ReadDirResult, ReadResult,
    SetAttributesParams, Vfs, VfsDiskNotify, VfsResult, WriteResult,
    chunking::{put_chunk_if_missing, put_node_if_missing},
};
use vx_vfs::{VfsConfig, VfsStart};

use crate::vfs::CasVfs;

/// Canary item ID — must be high enough to never collide with real CAS item IDs.
const CANARY_ITEM_ID: ItemId = 0xFFFF_FFFF_FFFD_0001;
const ROOT_ITEM_ID: ItemId = 1;
/// The single global mount on macOS — the configured `vfsd.mount` is ignored there;
/// there is exactly one Vixen volume. Block era: a real `/Volumes` volume (must match
/// `vx_management::mount::MOUNT_POINT`).
const MACOS_GLOBAL_MOUNT_PATH: &str = "/Volumes/Vixen";

pub fn effective_mount_path(configured_mount: &Utf8PathBuf) -> Utf8PathBuf {
    if cfg!(target_os = "macos") {
        Utf8PathBuf::from(MACOS_GLOBAL_MOUNT_PATH)
    } else {
        configured_mount.clone()
    }
}

struct Canary {
    name: String,
    content: Vec<u8>,
    item_id: ItemId,
}

/// Wrapper for [`CasVfs`] that also serves a synthetic canary file at the root
/// for smoke-testing the mount end-to-end.
#[derive(Clone)]
pub struct SharedVfs {
    inner: Arc<CasVfs>,
    canary: Arc<Canary>,
}

impl SharedVfs {
    pub fn new(inner: Arc<CasVfs>) -> Self {
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

    fn canary_attrs(&self) -> vx_services::ItemAttributes {
        vx_services::ItemAttributes {
            item_id: self.canary.item_id,
            item_type: vx_services::ItemType::File,
            size: self.canary.content.len() as u64,
            modified_time: 0,
            created_time: 0,
            mode: vx_services::mode::FILE_REGULAR,
        }
    }
}

async fn store_directory_node(
    cas: &vx_services::StoreClient,
    entries: Vec<DirectoryEntry>,
    total_size: u64,
) -> Result<Blake3Hash> {
    let node = CasNode::Directory(DirectoryNode {
        entries,
        total_size,
    });
    let (bytes, hash) = node.to_bytes_and_hash();
    put_node_if_missing(cas, hash, bytes)
        .await
        .map_err(|e| eyre::eyre!("failed to store directory node: {e}"))?;
    Ok(hash.0)
}

async fn store_small_file_tree(
    cas: &vx_services::StoreClient,
    rel_path: &str,
    data: &[u8],
) -> Result<Blake3Hash> {
    let content_hash = Blake3Hash::from_bytes(data);
    put_chunk_if_missing(cas, vx_services::ChunkHash(content_hash), data.to_vec())
        .await
        .map_err(|e| eyre::eyre!("failed to store file chunk: {e}"))?;

    let leaf_name = rel_path
        .rsplit('/')
        .next()
        .ok_or_else(|| eyre::eyre!("relative path should contain a file name"))?;

    let mut current_hash = store_directory_node(
        cas,
        vec![DirectoryEntry {
            name: leaf_name.to_string(),
            kind: DirectoryEntryKind::SmallFile {
                content_hash: ContentHash(content_hash),
                size: data.len() as u64,
                executable: false,
            },
        }],
        data.len() as u64,
    )
    .await?;

    for component in rel_path.split('/').rev().skip(1) {
        current_hash = store_directory_node(
            cas,
            vec![DirectoryEntry {
                name: component.to_string(),
                kind: DirectoryEntryKind::Dir {
                    hash: NodeHash(current_hash),
                },
            }],
            data.len() as u64,
        )
        .await?;
    }

    Ok(current_hash)
}

async fn seed_demo_prefix(cas: Arc<vx_services::StoreClient>, vfs: Arc<CasVfs>) -> Result<()> {
    let root_hash = store_small_file_tree(&cas, "src/hello.txt", b"hello from vx-vfsd\n").await?;
    vfs.create_prefix("demo".to_string()).await;
    vfs.mount_tree_to_prefix("demo", "", NodeHash(root_hash))
        .await
        .ok_or_else(|| eyre::eyre!("failed to mount built-in demo tree"))?;
    Ok(())
}

impl SharedVfs {
    async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult {
        if parent_id == ROOT_ITEM_ID && name == self.canary.name {
            let result = LookupResult {
                attrs: self.canary_attrs(),
                error: vx_services::errno::OK,
            };
            tracing::info!(
                parent_id,
                %name,
                item_id = result.attrs.item_id,
                item_type = ?result.attrs.item_type,
                mode = result.attrs.mode,
                size = result.attrs.size,
                "canary lookup hit"
            );
            return result;
        }
        tracing::trace!(parent_id, %name, "lookup");
        let result = CasVfs::lookup(self.inner.as_ref(), parent_id, name.clone()).await;
        tracing::debug!(
            parent_id,
            %name,
            item_id = result.attrs.item_id,
            item_type = ?result.attrs.item_type,
            mode = result.attrs.mode,
            size = result.attrs.size,
            error = result.error,
            "lookup result"
        );
        result
    }

    async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult {
        if item_id == self.canary.item_id {
            let result = GetAttributesResult {
                attrs: self.canary_attrs(),
                error: vx_services::errno::OK,
            };
            tracing::info!(
                item_id,
                result_item_id = result.attrs.item_id,
                item_type = ?result.attrs.item_type,
                mode = result.attrs.mode,
                size = result.attrs.size,
                "canary get_attributes hit"
            );
            return result;
        }
        tracing::trace!(item_id, "get_attributes");
        let result = CasVfs::get_attributes(self.inner.as_ref(), item_id).await;
        tracing::debug!(
            item_id,
            result_item_id = result.attrs.item_id,
            item_type = ?result.attrs.item_type,
            mode = result.attrs.mode,
            size = result.attrs.size,
            error = result.error,
            "get_attributes result"
        );
        result
    }

    async fn read_dir(&self, item_id: ItemId, cookie: u64, verifier: u64) -> ReadDirResult {
        tracing::trace!(item_id, cookie, verifier, "read_dir");
        let mut result = CasVfs::read_dir(self.inner.as_ref(), item_id, cookie, verifier).await;

        if item_id != ROOT_ITEM_ID || result.error != vx_services::errno::OK {
            tracing::debug!(
                item_id,
                cookie,
                verifier,
                next_cookie = result.next_cookie,
                response_verifier = result.verifier,
                error = result.error,
                entries = ?result
                    .entries
                    .iter()
                    .map(|entry| format!(
                        "{}:{}:{:?}:{}:{:#o}",
                        entry.name, entry.item_id, entry.item_type, entry.size, entry.mode
                    ))
                    .collect::<Vec<_>>(),
                "read_dir result"
            );
            return result;
        }

        let canary = vx_services::DirEntry {
            name: self.canary.name.clone(),
            item_id: self.canary.item_id,
            item_type: vx_services::ItemType::File,
            size: self.canary.content.len() as u64,
            modified_time: 0,
            created_time: 0,
            mode: vx_services::mode::FILE_REGULAR,
        };

        if cookie == 0 {
            result.entries.insert(0, canary);
            if result.next_cookie != 0 {
                result.next_cookie += 1;
            }
        } else if cookie == 1 {
            result.entries = vec![canary];
            result.next_cookie = 0;
        }

        tracing::debug!(
            item_id,
            cookie,
            verifier,
            next_cookie = result.next_cookie,
            response_verifier = result.verifier,
            error = result.error,
            entries = ?result
                .entries
                .iter()
                .map(|entry| format!(
                    "{}:{}:{:?}:{}:{:#o}",
                    entry.name, entry.item_id, entry.item_type, entry.size, entry.mode
                ))
                .collect::<Vec<_>>(),
            "read_dir result with canary"
        );

        result
    }

    async fn read(&self, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        if item_id == self.canary.item_id {
            tracing::trace!(item_id, offset, len, "canary read hit");
            let content = &self.canary.content;
            let start = (offset as usize).min(content.len());
            let end = (start + len as usize).min(content.len());
            return ReadResult {
                data: content[start..end].to_vec(),
                error: vx_services::errno::OK,
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
        item_type: vx_services::ItemType,
        mode: u32,
    ) -> CreateResult {
        tracing::trace!(parent_id, %name, ?item_type, mode, "create");
        CasVfs::create(self.inner.as_ref(), parent_id, name, item_type, mode).await
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
        item_type: vx_services::ItemType,
        mode: u32,
    ) -> CreateResult {
        SharedVfs::create(self, parent_id, name, item_type, mode).await
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

/// A single-service [`vox::ConnectionAcceptor`] that routes all connections
/// to the [`VfsdService`].
#[derive(Clone)]
pub struct VfsdAcceptor {
    service: VfsdService,
}

impl VfsdAcceptor {
    pub fn new(service: VfsdService) -> Self {
        Self { service }
    }
}

impl vox::ConnectionAcceptor for VfsdAcceptor {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), vox::Metadata<'static>> {
        match request.service() {
            "Vfsd" => {
                connection.handle_with(VfsdDispatcher::new(self.service.clone()));
                Ok(())
            }
            other => {
                tracing::warn!("vx-vfsd: unknown service requested: {}", other);
                Err(vec![])
            }
        }
    }
}

/// Boot the VFS daemon from an already-connected store client, mount the VFS
/// at `mount`, run the smoke test, and return a ready [`VfsdService`].
pub async fn boot_with_store(
    cas: Arc<vx_services::StoreClient>,
    mount: &Utf8PathBuf,
) -> Result<VfsdService> {
    // 2. Create VFS with optional L2 disk blob cache.
    let disk_cache_dir = std::env::var("VX_HOME")
        .ok()
        .map(|vx_home| Utf8PathBuf::from(vx_home).join("cache/blobs/blake3"));
    if let Some(ref dir) = disk_cache_dir {
        tracing::debug!(%dir, "L2 disk blob cache enabled");
    }
    let vfs = Arc::new(CasVfs::new(cas.clone(), disk_cache_dir, mount.clone()));
    seed_demo_prefix(cas.clone(), vfs.clone()).await?;

    // 3. Clean up, create mount dir, mount VFS, smoke test.
    let mount_result: Result<Utf8PathBuf> = async {
        platform::cleanup_stuck_mount(mount).await?;

        if platform::should_create_mount_dir(mount) {
            moire::fs::create_dir_all(mount.as_std_path())
                .await
                .map_err(|e| eyre::eyre!("failed to create mount directory {mount}: {e}"))?;
        }

        tracing::debug!("mounting VFS at {mount}");
        let shared_vfs = SharedVfs::new(vfs.clone());
        let canary = shared_vfs.canary.clone();
        match vx_vfs::mount(VfsConfig { mount_path: mount.clone() }, shared_vfs).await {
            Ok(VfsStart::Mounted {
                access_path,
                mount_path,
                mut vfs_dead,
                ..
            }) => {
                tracing::debug!("VFS mounted at {mount_path} (access path {access_path})");

                const MAX_ATTEMPTS: u32 = 10;
                const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
                let smoke_timeout = std::time::Duration::from_secs(5);

                let mut smoke_err: Option<String> = None;
                for attempt in 1..=MAX_ATTEMPTS {
                    let canary_fs_path = access_path.join(&canary.name);
                    tracing::debug!(attempt, "trying to read canary from {canary_fs_path}");

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
                                "timed out after {smoke_timeout:?} reading {} — mount not responding",
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
                    return Err(eyre::eyre!("VFS smoke test failed: {reason}"));
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

                Ok(access_path)
            }
            Ok(VfsStart::Unsupported { platform }) => {
                Err(eyre::eyre!("VFS not supported on platform {platform}"))
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
                            msg.push_str(&format!("\n  {}", line.dimmed()));
                        }
                    }
                }
                Err(eyre::eyre!("{msg}"))
            }
        }
    }
    .await;

    let access_path = match mount_result {
        Ok(p) => p,
        Err(error) => {
            use std::time::Instant;
            let before = Instant::now();
            let diagnostics = platform::collect_vfs_diagnostics(mount).await;
            let elapsed = before.elapsed();

            let temp_path = format!("/tmp/vfsd-diagnostics-{:016x}.txt", fastrand::u64(..));
            let diagnostic_content = format!("{error}\n{diagnostics}");
            let diagnostic_content = strip_ansi_escapes::strip_str(&diagnostic_content);
            if let Err(e) = moire::fs::write(&temp_path, &diagnostic_content).await {
                tracing::error!("failed to write VFS diagnostics to {}: {}", temp_path, e);
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
    };

    Ok(VfsdService::new(vfs, access_path))
}

/// Boot the VFS daemon: connect to the store at `store_addr`, mount the VFS
/// at `mount`, run the smoke test, and return a ready [`VfsdService`].
///
/// `store_addr` must be a `vox`-compatible address string (e.g. `local:///path/to/sock`)
/// pointing at a server that routes `"Store"` connections.
pub async fn boot(store_addr: &str, mount: &Utf8PathBuf) -> Result<VfsdService> {
    use vx_services::StoreClient;

    tracing::debug!(%store_addr, "connecting to store");
    let cas: StoreClient = vox::connect(store_addr)
        .await
        .map_err(|e| eyre::eyre!("failed to connect to store at {store_addr}: {e}"))?;
    tracing::debug!("connected to store");
    boot_with_store(Arc::new(cas), mount).await
}
