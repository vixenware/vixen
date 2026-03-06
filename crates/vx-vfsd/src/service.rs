//! RPC service implementation for vx-vfsd.

use crate::protocol::Vfsd;
use crate::vfs::CasVfs;
use std::collections::HashSet;
use std::sync::Arc;
use vx_store_proto::{
    Blake3Hash, CasNode, ContentHash, DirEntry as CasDirEntry, DirEntryKind, DirectoryNode,
    NodeHash, StoreClient,
};

#[derive(Clone)]
pub struct VfsdService {
    vfs: Arc<CasVfs>,
}

impl vx_io::boot::GracefulShutdown for VfsdService {
    async fn graceful_shutdown(&self) {}
}

impl VfsdService {
    pub fn new(vfs: Arc<CasVfs>) -> Self {
        Self { vfs }
    }

    /// Parse an absolute VFS path into (prefix_id, relative_path)
    ///
    /// Examples:
    /// - "/build-xyz/src/main.rs" -> ("build-xyz", "src/main.rs")
    /// - "/build-xyz/" -> ("build-xyz", "")
    /// - "/build-xyz" -> ("build-xyz", "")
    fn parse_path(path: &str) -> Result<(String, String), String> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Err("path cannot be empty or just '/'".into());
        }

        match path.split_once('/') {
            Some((prefix, rest)) => Ok((prefix.to_string(), rest.to_string())),
            None => Ok((path.to_string(), String::new())),
        }
    }

    /// Ensure a prefix exists, creating it if necessary
    async fn ensure_prefix(&self, prefix_id: &str) -> Result<(), String> {
        if !self.vfs.has_prefix(prefix_id).await {
            tracing::debug!(prefix_id, "creating prefix");
            self.vfs.create_prefix(prefix_id.to_string()).await;
        }
        Ok(())
    }

    /// Recursively walk a DirectoryNode tree and mount all entries into the VFS.
    async fn mount_directory_node(
        &self,
        prefix_id: &str,
        base_path: &str,
        node_hash: NodeHash,
    ) -> Result<(), String> {
        let cas = &self.vfs.cas;

        let mut stack: Vec<(String, NodeHash)> = vec![(base_path.to_string(), node_hash)];
        let mut visited: HashSet<NodeHash> = HashSet::new();

        while let Some((dir_path, dir_hash)) = stack.pop() {
            if !visited.insert(dir_hash) {
                return Err(format!(
                    "detected DirectoryNode cycle while mounting: {}",
                    dir_hash.to_hex()
                ));
            }

            let node_data = cas
                .get_node(dir_hash)
                .await
                .map_err(|e| format!("failed to fetch DirectoryNode: {:?}", e))?
                .clone()
                .ok_or_else(|| format!("DirectoryNode {} not found in CAS", dir_hash.to_hex()))?;

            let node = CasNode::from_bytes(&node_data)
                .map_err(|e| format!("failed to deserialize DirectoryNode: {:?}", e))?;

            let dir = match node {
                CasNode::Directory(d) => d,
                _ => {
                    return Err("expected DirectoryNode, got different CasNode variant".to_string());
                }
            };

            for entry in &dir.entries {
                let entry_path = if dir_path.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", dir_path, entry.name)
                };

                match &entry.kind {
                    DirEntryKind::SmallFile {
                        content_hash,
                        size,
                        executable,
                    } => {
                        self.vfs
                            .add_file_to_prefix(
                                prefix_id,
                                &entry_path,
                                content_hash.0,
                                None,
                                *size,
                                *executable,
                            )
                            .await;
                    }
                    DirEntryKind::LargeFile {
                        content_hash,
                        blob_node,
                        size,
                        executable,
                    } => {
                        self.vfs
                            .add_file_to_prefix(
                                prefix_id,
                                &entry_path,
                                content_hash.0,
                                Some(*blob_node),
                                *size,
                                *executable,
                            )
                            .await;
                    }
                    DirEntryKind::Dir { hash, .. } => {
                        self.vfs.add_dir_to_prefix(prefix_id, &entry_path).await;
                        stack.push((entry_path, *hash));
                    }
                    DirEntryKind::Symlink { .. } => {
                        tracing::warn!(path = %entry_path, "skipping symlink (not yet supported)");
                    }
                }
            }
        }

        Ok(())
    }
}

impl VfsdService {
    async fn mkdir(&self, path: String) -> Result<(), String> {
        tracing::debug!(%path, "mkdir");

        let (prefix_id, rel_path) = Self::parse_path(&path)?;

        // Ensure prefix exists
        self.ensure_prefix(&prefix_id).await?;

        // Create the directory
        if rel_path.is_empty() {
            // Creating the prefix itself - already done
            Ok(())
        } else {
            // Create directory within the prefix
            self.vfs
                .add_dir_to_prefix(&prefix_id, &rel_path)
                .await
                .ok_or_else(|| format!("failed to create directory at {path}"))?;
            Ok(())
        }
    }

    async fn mount(&self, path: String, tree_hash: Blake3Hash) -> Result<(), String> {
        tracing::debug!(%path, %tree_hash, "mount");

        let (prefix_id, rel_path) = Self::parse_path(&path)?;

        // Ensure prefix exists
        self.ensure_prefix(&prefix_id).await?;

        // The tree_hash is a NodeHash pointing to a DirectoryNode
        let node_hash = NodeHash(tree_hash);

        tracing::debug!(
            prefix_id,
            rel_path,
            node_hash = %node_hash,
            "mounting DirectoryNode tree"
        );

        // Recursively walk the DirectoryNode tree and mount all entries
        self.mount_directory_node(&prefix_id, &rel_path, node_hash)
            .await?;

        Ok(())
    }

    async fn flush(&self, path: String) -> Result<Blake3Hash, String> {
        tracing::debug!(%path, "flush");

        let (prefix_id, rel_path) = Self::parse_path(&path)?;

        // Flush written files to CAS.
        // On FUSE this drains in-memory write buffers, updating cas_hash on each item.
        // On ProjFS (Windows) this reads files from the on-disk mount.
        let flushed = self.vfs.flush_prefix_to_cas(&prefix_id).await;

        tracing::debug!(
            prefix_id,
            rel_path,
            flushed_count = flushed.len(),
            "flushed written files"
        );

        let cas = &self.vfs.cas;

        // Take a cheap snapshot (no write buffers — they've been drained by flush above).
        // Resolve the target directory, then recursively build DirectoryNodes.
        let (items_snapshot, target_id) = {
            let state = self.vfs.state.lock();

            let prefix = state
                .prefixes
                .get(&prefix_id)
                .ok_or_else(|| format!("prefix {prefix_id} not found"))?;

            let mut current_id = prefix.root_item_id;

            if !rel_path.is_empty() {
                for component in rel_path.split(['/', '\\']).filter(|s| !s.is_empty()) {
                    let child = state
                        .items
                        .lookup_child(current_id, component)
                        .ok_or_else(|| format!("path {path} not found"))?;
                    current_id = child.id;
                }
            }

            (state.items.clone_without_buffers(), current_id)
        };

        let root_hash = self
            .build_directory_node(cas, &items_snapshot, target_id)
            .await?;

        tracing::debug!(
            root_hash = %root_hash,
            "created directory tree"
        );

        Ok(root_hash.0)
    }

    async fn rm_rf(&self, path: String) -> Result<(), String> {
        tracing::debug!(%path, "rm_rf");

        let (prefix_id, rel_path) = Self::parse_path(&path)?;

        if rel_path.is_empty() {
            // Remove entire prefix
            self.vfs.remove_prefix(&prefix_id).await;
            Ok(())
        } else {
            // Remove specific subtree within prefix
            let target_item_id = {
                let state = self.vfs.state.lock();

                let prefix = state
                    .prefixes
                    .get(&prefix_id)
                    .ok_or_else(|| format!("prefix {prefix_id} not found"))?;

                let mut current_id = prefix.root_item_id;

                // Navigate to the target path
                for component in rel_path.split(['/', '\\']).filter(|s| !s.is_empty()) {
                    let child = state
                        .items
                        .lookup_child(current_id, component)
                        .ok_or_else(|| format!("path {path} not found"))?;
                    current_id = child.id;
                }

                current_id
            };

            // Collect and remove all descendants under one write lock
            {
                let mut state = self.vfs.state.lock();
                let to_remove =
                    crate::vfs::CasVfs::collect_descendants_from(&state.items, target_item_id);
                // Remove in reverse (leaves first) to keep children index clean
                for &id in to_remove.iter().rev() {
                    state.items.remove(id);
                }
            }

            Ok(())
        }
    }

    async fn ping(&self) -> Result<(), String> {
        Ok(())
    }

    async fn dump_state(&self, prefix_id: String) -> Result<(), String> {
        tracing::info!("Dumping state for prefix: {prefix_id}");
        self.vfs.dump_state(&prefix_id).await;
        Ok(())
    }
}

impl Vfsd for VfsdService {
    async fn mkdir(&self, path: String) -> Result<(), String> {
        VfsdService::mkdir(self, path).await
    }

    async fn mount(&self, path: String, tree_hash: Blake3Hash) -> Result<(), String> {
        VfsdService::mount(self, path, tree_hash).await
    }

    async fn flush(&self, path: String) -> Result<Blake3Hash, String> {
        VfsdService::flush(self, path).await
    }

    async fn rm_rf(&self, path: String) -> Result<(), String> {
        VfsdService::rm_rf(self, path).await
    }

    async fn ping(&self) -> Result<(), String> {
        VfsdService::ping(self).await
    }

    async fn dump_state(&self, prefix_id: String) -> Result<(), String> {
        VfsdService::dump_state(self, prefix_id).await
    }
}

impl VfsdService {
    /// Recursively build a DirectoryNode from VFS items, store it in CAS, return the NodeHash.
    /// Uses the VfsItems children index for O(children) per directory instead of O(N) scans.
    async fn build_directory_node(
        &self,
        cas: &Arc<StoreClient>,
        items: &crate::vfs::VfsItems,
        dir_item_id: vixenfs_proto::ItemId,
    ) -> Result<NodeHash, String> {
        let mut visited = HashSet::new();
        let (hash, _total_size) = self
            .build_directory_node_inner(cas, items, dir_item_id, &mut visited)
            .await?;
        Ok(hash)
    }

    /// Inner recursive builder that returns (NodeHash, total_size).
    async fn build_directory_node_inner(
        &self,
        cas: &Arc<StoreClient>,
        items: &crate::vfs::VfsItems,
        dir_item_id: vixenfs_proto::ItemId,
        visited: &mut HashSet<vixenfs_proto::ItemId>,
    ) -> Result<(NodeHash, u64), String> {
        use vixenfs_proto::ItemType;

        if !visited.insert(dir_item_id) {
            return Err(format!(
                "detected directory cycle while flushing: item_id={dir_item_id:?}"
            ));
        }

        let children = items.sorted_children_of(dir_item_id);

        let mut entries = Vec::new();
        let mut total_size: u64 = 0;

        for (child_name, child) in &children {
            match child.item_type {
                ItemType::File => {
                    let Some(hash) = child.cas_hash else {
                        continue;
                    };

                    let executable = (child.mode & 0o111) != 0;
                    let size = child.size;
                    total_size += size;

                    let kind = match child.blob_node {
                        None => DirEntryKind::SmallFile {
                            content_hash: ContentHash(hash),
                            size,
                            executable,
                        },
                        Some(blob_node) => DirEntryKind::LargeFile {
                            content_hash: ContentHash(hash),
                            blob_node,
                            size,
                            executable,
                        },
                    };

                    entries.push(CasDirEntry {
                        name: child_name.to_string(),
                        kind,
                    });
                }
                ItemType::Directory => {
                    let (child_hash, child_total) =
                        Box::pin(self.build_directory_node_inner(cas, items, child.id, visited))
                            .await?;

                    total_size += child_total;

                    entries.push(CasDirEntry {
                        name: child_name.to_string(),
                        kind: DirEntryKind::Dir {
                            hash: child_hash,
                            total_size: child_total,
                        },
                    });
                }
                ItemType::Symlink => {
                    tracing::warn!(name = %child.name, "skipping symlink in flush");
                }
            }
        }

        let dir_node = CasNode::Directory(DirectoryNode {
            entries,
            total_size,
        });

        let (node_bytes, node_hash) = dir_node.to_bytes_and_hash();
        cas.put_node(node_hash, node_bytes.to_vec())
            .await
            .map_err(|e| format!("failed to store DirectoryNode: {:?}", e))?;

        visited.remove(&dir_item_id);

        Ok((node_hash, total_size))
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use vixenfs_proto::{ItemId, ItemType, SetAttributesParams, errno};
    use vx_io::spawn::{ServiceBuilder, ServiceHandle};
    use vx_store_proto::{Blake3Hash, CDC_MAX_CHUNK, CasNode, ChunkHash, StoreClient};

    use super::*;
    use crate::vfs::CasVfs;

    struct TestEnv {
        _store_root: TempDir,
        _vx_home: TempDir,
        _store_service: ServiceHandle,
        cas: Arc<StoreClient>,
        service: VfsdService,
    }

    impl TestEnv {
        async fn new() -> Self {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .try_init();

            let store_root = TempDir::new().expect("failed to create store root");
            let vx_home = TempDir::new().expect("failed to create vx_home");
            let mount_path = TempDir::new().expect("failed to create mount path");
            let endpoint = free_local_endpoint();

            let store_root_str = store_root.path().to_string_lossy().into_owned();
            let binary_path = resolve_vx_store_binary();

            let store_service = ServiceBuilder::new(binary_path, endpoint)
                .caller_name("vx-vfsd-flush-tests")
                .args(&["--root", &store_root_str])
                .env("VX_TEST", "1")
                .timeout(Duration::from_secs(10))
                .connect()
                .await
                .expect("failed to spawn/connect vx-store");

            let store_virtual = store_service
                .boot_and_connect()
                .await
                .expect("failed to boot/connect vx-store");
            let cas = Arc::new(StoreClient::new(store_virtual));

            let vfs = Arc::new(CasVfs::new(
                Arc::clone(&cas),
                None,
                mount_path.path().to_string_lossy().into_owned().into(),
            ));
            let service = VfsdService::new(vfs);

            Self {
                _store_root: store_root,
                _vx_home: vx_home,
                _store_service: store_service,
                cas,
                service,
            }
        }
    }

    fn free_local_endpoint() -> vx_io::net::Endpoint {
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
        let addr = listener.local_addr().expect("failed to read local addr");
        drop(listener);
        vx_io::net::Endpoint::parse(&addr.to_string()).expect("failed to parse endpoint")
    }

    fn resolve_vx_store_binary() -> String {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_vx-store")
            && !path.is_empty()
        {
            return path;
        }

        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/vx-store")
            .to_string_lossy()
            .into_owned()
    }

    async fn write_file(
        env: &TestEnv,
        prefix_id: &str,
        rel_dir: &str,
        file_name: &str,
        data: &[u8],
    ) -> (ItemId, ItemId) {
        env.service
            .mkdir(format!("/{prefix_id}/{rel_dir}"))
            .await
            .expect("mkdir failed");

        let dir_id = {
            let state = env.service.vfs.state.lock();
            let prefix_root = state
                .prefixes
                .get(prefix_id)
                .expect("prefix should exist")
                .root_item_id;
            state
                .items
                .lookup_child(prefix_root, rel_dir)
                .expect("dir should exist")
                .id
        };

        let create = env
            .service
            .vfs
            .create(dir_id, file_name.to_string(), ItemType::File)
            .await;
        assert_eq!(
            create.error,
            errno::OK,
            "create failed with errno {}",
            create.error
        );

        let write = env
            .service
            .vfs
            .write(create.item_id, 0, data.to_vec())
            .await;
        assert_eq!(
            write.error,
            errno::OK,
            "write failed with errno {}",
            write.error
        );
        (dir_id, create.item_id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_writes_buffered_file_to_cas_and_returns_directory_hash() {
        let env = TestEnv::new().await;
        write_file(&env, "pfx-a", "out", "hello.txt", b"hello flush").await;

        let root_hash = env
            .service
            .flush("/pfx-a/out".to_string())
            .await
            .expect("flush failed");

        let node_bytes = env
            .cas
            .get_node(vx_store_proto::NodeHash(root_hash))
            .await
            .expect("get_node RPC failed")
            .expect("flushed root node not found in CAS");
        let root_node =
            CasNode::from_bytes(&node_bytes).expect("failed to decode root DirectoryNode");
        let dir = match root_node {
            CasNode::Directory(d) => d,
            other => panic!("expected DirectoryNode, got {other:?}"),
        };
        assert_eq!(dir.entries.len(), 1);
        assert_eq!(dir.entries[0].name, "hello.txt");

        let expected_hash = Blake3Hash::from_bytes(b"hello flush");
        let expected_chunk = ChunkHash(expected_hash);
        let chunk = env
            .cas
            .get_chunk(expected_chunk)
            .await
            .expect("get_chunk RPC failed")
            .expect("flushed chunk not found in CAS");
        assert_eq!(chunk, b"hello flush");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_prefix_root_without_files_returns_empty_directory_node() {
        let env = TestEnv::new().await;
        env.service
            .mkdir("/pfx-empty".to_string())
            .await
            .expect("mkdir failed");

        let root_hash = env
            .service
            .flush("/pfx-empty".to_string())
            .await
            .expect("flush failed");

        let node_bytes = env
            .cas
            .get_node(vx_store_proto::NodeHash(root_hash))
            .await
            .expect("get_node RPC failed")
            .expect("flushed empty node not found in CAS");
        let root_node = CasNode::from_bytes(&node_bytes).expect("failed to decode empty root node");
        let dir = match root_node {
            CasNode::Directory(d) => d,
            other => panic!("expected DirectoryNode, got {other:?}"),
        };
        assert!(dir.entries.is_empty(), "expected no entries");
        assert_eq!(dir.total_size, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_missing_prefix_returns_error() {
        let env = TestEnv::new().await;
        let err = env
            .service
            .flush("/does-not-exist/out".to_string())
            .await
            .expect_err("flush should fail for missing prefix");
        assert!(err.contains("prefix"), "unexpected error: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_rejects_entry_name_containing_slash() {
        let env = TestEnv::new().await;
        let (dir_id, file_id) = write_file(&env, "pfx-badname", "out", "ok.txt", b"hello").await;

        let rename = env
            .service
            .vfs
            .rename(file_id, dir_id, "bad/name".to_string())
            .await;
        assert_eq!(
            rename.error,
            errno::OK,
            "rename should currently succeed in VFS"
        );

        let err = env
            .service
            .flush("/pfx-badname/out".to_string())
            .await
            .expect_err("flush should fail when entry name contains '/'");
        assert!(
            err.contains("contains '/'"),
            "unexpected flush error for invalid entry name: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_rejects_smallfile_size_over_cdc_max() {
        let env = TestEnv::new().await;
        let (_dir_id, file_id) = write_file(&env, "pfx-big", "out", "a.bin", b"tiny").await;

        env.service
            .flush("/pfx-big/out".to_string())
            .await
            .expect("first flush should succeed");

        let set = env
            .service
            .vfs
            .set_attributes(
                file_id,
                SetAttributesParams {
                    mode: None,
                    modified_time: None,
                    size: Some(CDC_MAX_CHUNK as u64 + 1),
                },
            )
            .await;
        assert_eq!(
            set.error,
            errno::OK,
            "set_attributes failed with errno {}",
            set.error
        );

        let err = env
            .service
            .flush("/pfx-big/out".to_string())
            .await
            .expect_err("flush should fail when SmallFile size exceeds CDC_MAX_CHUNK");
        assert!(
            err.contains("SmallFile"),
            "unexpected flush error for SmallFile size mismatch: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_dir_rejects_stale_cookie_verifier_pair_after_mutation() {
        let env = TestEnv::new().await;
        env.service
            .mkdir("/pfx-readdir/out".to_string())
            .await
            .expect("mkdir failed");

        let out_id = {
            let state = env.service.vfs.state.lock();
            let prefix_root = state
                .prefixes
                .get("pfx-readdir")
                .expect("prefix should exist")
                .root_item_id;
            state
                .items
                .lookup_child(prefix_root, "out")
                .expect("out dir should exist")
                .id
        };

        for i in 0..101 {
            let create = env
                .service
                .vfs
                .create(out_id, format!("f-{i:03}.txt"), ItemType::File)
                .await;
            assert_eq!(create.error, errno::OK, "create failed at {i}");
        }

        let first_page = env.service.vfs.read_dir(out_id, 0, 0).await;
        assert_eq!(first_page.error, errno::OK);
        assert_eq!(first_page.entries.len(), 100);
        assert!(first_page.next_cookie > 0);
        assert!(first_page.verifier > 0);

        let added = env
            .service
            .vfs
            .create(out_id, "later.txt".to_string(), ItemType::File)
            .await;
        assert_eq!(added.error, errno::OK);

        let stale = env
            .service
            .vfs
            .read_dir(out_id, first_page.next_cookie, first_page.verifier)
            .await;
        assert_eq!(stale.error, errno::ESTALE);
        assert_eq!(stale.entries.len(), 0);
        assert_eq!(stale.next_cookie, 0);
        assert!(stale.verifier != first_page.verifier);
    }
}
