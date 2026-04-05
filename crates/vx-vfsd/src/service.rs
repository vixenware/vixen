//! RPC service implementation for vx-vfsd.

use crate::protocol::{VfsPrefixStateSummary, VfsTrackedObservation, Vfsd};
use crate::vfs::CasVfs;
use camino::Utf8PathBuf;
use std::collections::HashSet;
use std::sync::Arc;
use vx_services::{
    Blake3Hash, CasNode, ContentHash, DirectoryEntry, DirectoryEntryKind, DirectoryNode, NodeHash,
    StoreClient,
};

#[derive(Clone)]
pub struct VfsdService {
    vfs: Arc<CasVfs>,
    access_path: Utf8PathBuf,
}

impl VfsdService {
    pub fn new(vfs: Arc<CasVfs>, access_path: Utf8PathBuf) -> Self {
        Self { vfs, access_path }
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
        tracing::info!(%path, %tree_hash, "mounting tree into VFS");
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

        self.vfs
            .mount_tree_to_prefix(&prefix_id, &rel_path, node_hash)
            .await
            .ok_or_else(|| format!("failed to register mount {path}"))?;

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

    async fn access_path(&self) -> Result<String, String> {
        Ok(self.access_path.as_str().to_string())
    }

    async fn describe_prefix_state(
        &self,
        prefix_id: String,
    ) -> Result<VfsPrefixStateSummary, String> {
        Ok(self.vfs.describe_prefix_state(&prefix_id).await)
    }

    async fn tracked_observations(
        &self,
        prefix_id: String,
        roots: Vec<String>,
    ) -> Result<Vec<VfsTrackedObservation>, String> {
        Ok(self.vfs.tracked_observations(&prefix_id, &roots).await)
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

    async fn access_path(&self) -> Result<String, String> {
        VfsdService::access_path(self).await
    }

    async fn describe_prefix_state(
        &self,
        prefix_id: String,
    ) -> Result<VfsPrefixStateSummary, String> {
        VfsdService::describe_prefix_state(self, prefix_id).await
    }

    async fn tracked_observations(
        &self,
        prefix_id: String,
        roots: Vec<String>,
    ) -> Result<Vec<VfsTrackedObservation>, String> {
        VfsdService::tracked_observations(self, prefix_id, roots).await
    }
}

impl VfsdService {
    /// Recursively build a DirectoryNode from VFS items, store it in CAS, return the NodeHash.
    /// Uses the VfsItems children index for O(children) per directory instead of O(N) scans.
    async fn build_directory_node(
        &self,
        cas: &Arc<StoreClient>,
        items: &crate::vfs::VfsItems,
        dir_item_id: vx_services::ItemId,
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
        dir_item_id: vx_services::ItemId,
        visited: &mut HashSet<vx_services::ItemId>,
    ) -> Result<(NodeHash, u64), String> {
        use vx_services::ItemType;

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
                        None => DirectoryEntryKind::SmallFile {
                            content_hash: ContentHash(hash),
                            size,
                            executable,
                        },
                        Some(blob_node) => DirectoryEntryKind::LargeFile {
                            content_hash: ContentHash(hash),
                            blob_node,
                            size,
                            executable,
                        },
                    };

                    entries.push(DirectoryEntry {
                        name: child_name.to_string(),
                        kind,
                    });
                }
                ItemType::Directory => {
                    let (child_hash, child_total) =
                        Box::pin(self.build_directory_node_inner(cas, items, child.id, visited))
                            .await?;

                    total_size += child_total;

                    entries.push(DirectoryEntry {
                        name: child_name.to_string(),
                        kind: DirectoryEntryKind::Dir { hash: child_hash },
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
