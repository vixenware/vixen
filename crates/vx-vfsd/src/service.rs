//! RPC service implementation for vx-vfsd.

use crate::protocol::Vfsd;
use crate::vfs::CasVfs;
use std::sync::Arc;
use vx_store_proto::{
    Blake3Hash, CasNode, ContentHash, DirEntry as CasDirEntry, DirEntryKind, DirectoryNode,
    NodeHash,
};

#[derive(Clone)]
pub struct VfsdService {
    vfs: Arc<CasVfs>,
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

        let node_data = cas
            .get_node(node_hash)
            .await
            .map_err(|e| format!("failed to fetch DirectoryNode: {e}"))?
            .ok_or_else(|| format!("DirectoryNode {} not found in CAS", node_hash.to_hex()))?;

        let node = CasNode::from_bytes(&node_data)
            .map_err(|e| format!("failed to deserialize DirectoryNode: {e}"))?;

        let dir = match node {
            CasNode::Directory(d) => d,
            _ => return Err("expected DirectoryNode, got different CasNode variant".to_string()),
        };

        for entry in &dir.entries {
            let entry_path = if base_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", base_path, entry.name)
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
                    // Recursively mount subdirectory
                    Box::pin(self.mount_directory_node(prefix_id, &entry_path, *hash)).await?;
                }
                DirEntryKind::Symlink { .. } => {
                    tracing::warn!(path = %entry_path, "skipping symlink (not yet supported)");
                }
            }
        }

        Ok(())
    }
}

impl Vfsd for VfsdService {
    async fn mkdir(&self, _cx: &roam::Context, path: String) -> Result<(), String> {
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

    async fn mount(
        &self,
        _cx: &roam::Context,
        path: String,
        tree_hash: Blake3Hash,
    ) -> Result<(), String> {
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

    async fn flush(&self, _cx: &roam::Context, path: String) -> Result<Blake3Hash, String> {
        tracing::debug!(%path, "flush");

        let (prefix_id, rel_path) = Self::parse_path(&path)?;

        // First flush any written files to CAS
        let flushed = self.vfs.flush_prefix_to_cas(&prefix_id).await;

        tracing::debug!(
            prefix_id,
            rel_path,
            flushed_count = flushed.len(),
            "flushed written files"
        );

        let cas = &self.vfs.cas;

        // Get prefix root and find the target directory
        let (target_item_id, snapshot) = {
            let prefixes = self.vfs.prefixes.read();
            let items = self.vfs.items.read();

            let prefix = prefixes
                .get(&prefix_id)
                .ok_or_else(|| format!("prefix {prefix_id} not found"))?;

            let mut current_id = prefix.root_item_id;

            // Navigate to the target path (O(1) per component via children index)
            if !rel_path.is_empty() {
                for component in rel_path.split('/').filter(|s| !s.is_empty()) {
                    let child = items
                        .lookup_child(current_id, component)
                        .ok_or_else(|| format!("path {path} not found"))?;
                    current_id = child.id;
                }
            }

            // Build minimal snapshot: no write_buffers, O(N) single pass, includes children index
            (current_id, items.snapshot_for_flush())
        };

        // Build a DirectoryNode tree from the VFS state, store it in CAS
        let root_hash = self
            .build_directory_node(cas, &snapshot, target_item_id)
            .await?;

        tracing::debug!(
            root_hash = %root_hash,
            "created DirectoryNode tree"
        );

        // Return the root hash as Blake3Hash (the wire type for the Vfsd protocol)
        Ok(root_hash.0)
    }

    async fn rm_rf(&self, _cx: &roam::Context, path: String) -> Result<(), String> {
        tracing::debug!(%path, "rm_rf");

        let (prefix_id, rel_path) = Self::parse_path(&path)?;

        if rel_path.is_empty() {
            // Remove entire prefix
            self.vfs.remove_prefix(&prefix_id).await;
            Ok(())
        } else {
            // Remove specific subtree within prefix
            let target_item_id = {
                let prefixes = self.vfs.prefixes.read();
                let items = self.vfs.items.read();

                let prefix = prefixes
                    .get(&prefix_id)
                    .ok_or_else(|| format!("prefix {prefix_id} not found"))?;

                let mut current_id = prefix.root_item_id;

                for component in rel_path.split('/').filter(|s| !s.is_empty()) {
                    let child = items
                        .lookup_child(current_id, component)
                        .ok_or_else(|| format!("path {path} not found"))?;
                    current_id = child.id;
                }

                current_id
            };

            // Collect and remove all descendants under one write lock
            {
                let mut items = self.vfs.items.write();
                let to_remove =
                    crate::vfs::CasVfs::collect_descendants_from(&items, target_item_id);
                // Remove in reverse (leaves first) to keep children index clean
                for &id in to_remove.iter().rev() {
                    items.remove(id);
                }
            }

            Ok(())
        }
    }

    async fn ping(&self, _cx: &roam::Context) -> Result<(), String> {
        Ok(())
    }

    async fn dump_state(&self, _cx: &roam::Context, prefix_id: String) -> Result<(), String> {
        tracing::info!("Dumping state for prefix: {prefix_id}");
        self.vfs.dump_state(&prefix_id).await;
        Ok(())
    }
}

impl VfsdService {
    /// Recursively build a DirectoryNode from a flush snapshot, store it in CAS, return the NodeHash.
    /// Uses the snapshot's children index for O(children) per directory instead of O(N) scans.
    /// Returns (NodeHash, total_size) so the parent can record total_size without re-scanning.
    async fn build_directory_node(
        &self,
        cas: &Arc<vx_store_proto::StoreClient<crate::CasCaller>>,
        snapshot: &crate::vfs::FlushSnapshot,
        dir_item_id: vixenfs_proto::ItemId,
    ) -> Result<NodeHash, String> {
        let (hash, _total_size) = self
            .build_directory_node_inner(cas, snapshot, dir_item_id)
            .await?;
        Ok(hash)
    }

    /// Inner recursive builder that returns (NodeHash, total_size).
    async fn build_directory_node_inner(
        &self,
        cas: &Arc<vx_store_proto::StoreClient<crate::CasCaller>>,
        snapshot: &crate::vfs::FlushSnapshot,
        dir_item_id: vixenfs_proto::ItemId,
    ) -> Result<(NodeHash, u64), String> {
        use vixenfs_proto::ItemType;

        // Children are already sorted by name in the snapshot
        let empty_children = Vec::new();
        let children = snapshot
            .children
            .get(&dir_item_id)
            .unwrap_or(&empty_children);

        let mut entries = Vec::new();
        let mut total_size: u64 = 0;

        for (child_name, child_id) in children {
            let Some(child) = snapshot.items.get(child_id) else {
                continue;
            };

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
                        name: child_name.clone(),
                        kind,
                    });
                }
                ItemType::Directory => {
                    let (child_hash, child_total) =
                        Box::pin(self.build_directory_node_inner(cas, snapshot, child.id)).await?;

                    total_size += child_total;

                    entries.push(CasDirEntry {
                        name: child_name.clone(),
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
            .map_err(|e| format!("failed to store DirectoryNode: {e}"))?;

        Ok((node_hash, total_size))
    }
}
