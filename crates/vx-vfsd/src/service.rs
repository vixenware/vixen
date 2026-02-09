//! RPC service implementation for vx-vfsd.

use crate::protocol::Vfsd;
use crate::vfs::CasVfs;
use std::sync::Arc;
use vx_store_proto::{Blake3Hash, TreeManifest};

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
                .ok_or_else(|| format!("failed to create directory at {}", path))?;
            Ok(())
        }
    }

    async fn mount(&self, _cx: &roam::Context, path: String, tree_hash: Blake3Hash) -> Result<(), String> {
        tracing::debug!(%path, %tree_hash, "mount");

        let (prefix_id, rel_path) = Self::parse_path(&path)?;

        // Ensure prefix exists
        self.ensure_prefix(&prefix_id).await?;

        // Fetch tree manifest from CAS
        let cas = &self.vfs.cas;
        let manifest: TreeManifest = cas
            .get_tree_manifest(tree_hash)
            .await
            .map_err(|e| format!("failed to fetch tree manifest: {}", e))?
            .ok_or_else(|| format!("tree manifest {} not found in CAS", tree_hash))?;

        tracing::debug!(
            prefix_id,
            rel_path,
            entries = manifest.entries.len(),
            "mounting tree manifest"
        );

        // Add each entry from the manifest to the VFS
        for entry in &manifest.entries {
            let entry_path = if rel_path.is_empty() {
                entry.path.clone()
            } else {
                format!("{}/{}", rel_path.trim_end_matches('/'), entry.path)
            };

            match &entry.kind {
                vx_store_proto::TreeEntryKind::File { blob, size, executable } => {
                    self.vfs
                        .add_file_to_prefix(&prefix_id, &entry_path, *blob, *size, *executable)
                        .await;
                }
                vx_store_proto::TreeEntryKind::Directory => {
                    self.vfs.add_dir_to_prefix(&prefix_id, &entry_path).await;
                }
                vx_store_proto::TreeEntryKind::Symlink { .. } => {
                    // TODO: Support symlinks
                    tracing::warn!(path = %entry_path, "skipping symlink (not yet supported)");
                }
            }
        }

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

        // Now collect ALL files under the relative path to create a tree manifest
        // We need to read the VFS and collect all items under this subtree
        let cas = &self.vfs.cas;

        // Get prefix root and find the target directory
        let (target_item_id, items_snapshot) = {
            let prefixes = self.vfs.prefixes.read();
            let items = self.vfs.items.read();

            let prefix = prefixes
                .get(&prefix_id)
                .ok_or_else(|| format!("prefix {} not found", prefix_id))?;

            let mut current_id = prefix.root_item_id;

            // Navigate to the target path
            if !rel_path.is_empty() {
                for component in rel_path.split('/').filter(|s| !s.is_empty()) {
                    let children: Vec<_> = items
                        .values()
                        .filter(|item| item.parent_id == current_id && item.name == component)
                        .collect();

                    current_id = children
                        .first()
                        .ok_or_else(|| format!("path {} not found", path))?
                        .id;
                }
            }

            (current_id, items.clone())
        };

        // Collect all descendants
        let descendants = self.vfs.collect_descendants(&items_snapshot, target_item_id);

        // Build tree manifest
        let mut manifest = TreeManifest::new();

        for item_id in descendants {
            let item = items_snapshot.get(&item_id).unwrap();

            // Build relative path from target root
            let item_path = self.vfs.build_path(&items_snapshot, item_id, target_item_id);

            match item.item_type {
                vixenfs_proto::ItemType::File => {
                    if let Some(hash) = item.cas_hash {
                        let executable = (item.mode & 0o111) != 0;
                        manifest.add_file(item_path, hash, item.size, executable);
                    }
                }
                vixenfs_proto::ItemType::Directory => {
                    if item_id != target_item_id {
                        // Don't add the root directory itself
                        manifest.add_directory(item_path);
                    }
                }
                vixenfs_proto::ItemType::Symlink => {
                    // TODO: Support symlinks in flush
                    tracing::warn!(path = %item_path, "skipping symlink in flush (not yet supported)");
                }
            }
        }

        // Finalize and upload manifest
        manifest.finalize();

        tracing::debug!(
            entries = manifest.entries.len(),
            total_size = manifest.total_size_bytes,
            "created tree manifest"
        );

        // Serialize manifest to JSON and store as blob
        let manifest_json = facet_json::to_string(&manifest)
            .map_err(|e| format!("failed to serialize tree manifest: {}", e))?;
        let manifest_hash = Blake3Hash::from_bytes(manifest_json.as_bytes());

        // Store the manifest JSON as a blob
        cas.put_blob(manifest_json.into_bytes())
            .await
            .map_err(|e| format!("failed to store tree manifest blob: {}", e))?;

        Ok(manifest_hash)
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
                    .ok_or_else(|| format!("prefix {} not found", prefix_id))?;

                let mut current_id = prefix.root_item_id;

                // Navigate to the target path
                for component in rel_path.split('/').filter(|s| !s.is_empty()) {
                    let children: Vec<_> = items
                        .values()
                        .filter(|item| item.parent_id == current_id && item.name == component)
                        .collect();

                    current_id = children
                        .first()
                        .ok_or_else(|| format!("path {} not found", path))?
                        .id;
                }

                current_id
            };

            // Collect all descendants including the target itself
            let items_to_remove = {
                let items = self.vfs.items.read();
                let mut all = self.vfs.collect_descendants(&items, target_item_id);
                all.push(target_item_id); // Include the target itself
                all
            };

            // Remove all items
            {
                let mut items = self.vfs.items.write();
                for id in items_to_remove {
                    items.remove(&id);
                }
            }

            Ok(())
        }
    }

    async fn ping(&self, _cx: &roam::Context) -> Result<(), String> {
        Ok(())
    }

    async fn dump_state(&self, _cx: &roam::Context, prefix_id: String) -> Result<(), String> {
        tracing::info!("Dumping state for prefix: {}", prefix_id);
        self.vfs.dump_state(&prefix_id).await;
        Ok(())
    }
}
