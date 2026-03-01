//! RPC client for vx-vfsd

use std::collections::HashMap;
use std::sync::Arc;
use vx_store_proto::{
    Blake3Hash, CasNode, ContentHash, DirEntry, DirEntryKind, DirectoryNode, NodeHash, StoreClient,
};

type RpcClient = vx_vfsd::protocol::VfsdClient<crate::VfsdCaller>;

/// Client wrapper for vx-vfsd RPC operations
#[derive(Clone)]
pub struct VfsdClient {
    client: Arc<RpcClient>,
    cas: Arc<StoreClient<crate::CasCaller>>,
}

impl VfsdClient {
    pub fn new(client: RpcClient, cas: Arc<StoreClient<crate::CasCaller>>) -> Self {
        Self {
            client: Arc::new(client),
            cas,
        }
    }

    /// Check if a prefix exists (by attempting to create it)
    pub async fn has_prefix(&self, prefix_id: &str) -> bool {
        // Try to mkdir - if it succeeds or fails with "exists", prefix is there
        // This is a bit of a hack but works for our use case
        let path = format!("/{}", prefix_id);
        self.client.mkdir(path).await.is_ok()
    }

    /// Create a new action prefix
    pub async fn create_prefix(&self, prefix_id: String) -> (String, u64) {
        let path = format!("/{}", prefix_id);
        self.client
            .mkdir(path)
            .await
            .expect("failed to create prefix");
        (prefix_id, 0) // Return dummy ItemId (not used by vx-runner)
    }

    /// Add a directory to a prefix
    pub async fn add_dir_to_prefix(&self, prefix_id: &str, rel_path: &str) {
        let path = format!("/{}/{}", prefix_id, rel_path);
        let _ = self.client.mkdir(path).await;
    }

    /// Add a file to a prefix from CAS
    ///
    /// Implemented by creating a DirectoryNode tree and mounting it.
    pub async fn add_file_to_prefix(
        &self,
        prefix_id: &str,
        path: &str,
        cas_hash: Blake3Hash,
        size: u64,
        executable: bool,
    ) -> Result<(), String> {
        // Split path into directory components and filename
        let components: Vec<&str> = path.split('/').collect();
        let filename = components
            .last()
            .ok_or_else(|| format!("empty path: {}", path))?;

        // Build leaf DirectoryNode containing just this file
        let file_entry = DirEntry {
            name: filename.to_string(),
            kind: DirEntryKind::SmallFile {
                content_hash: ContentHash(cas_hash),
                size,
                executable,
            },
        };

        let leaf_node = CasNode::Directory(DirectoryNode {
            entries: vec![file_entry],
            total_size: size,
        });
        let (leaf_bytes, leaf_hash) = leaf_node.to_bytes_and_hash();
        self.cas
            .put_node(leaf_hash, leaf_bytes)
            .await
            .map_err(|e| format!("failed to store DirectoryNode for {}: {:?}", path, e))?;

        // Build parent directories from inside out (skip the filename)
        let mut current_hash = leaf_hash;
        let current_size = size;
        for &dir_name in components[..components.len() - 1].iter().rev() {
            let dir_node = CasNode::Directory(DirectoryNode {
                entries: vec![DirEntry {
                    name: dir_name.to_string(),
                    kind: DirEntryKind::Dir {
                        hash: current_hash,
                        total_size: current_size,
                    },
                }],
                total_size: current_size,
            });
            let (dir_bytes, dir_hash) = dir_node.to_bytes_and_hash();
            self.cas.put_node(dir_hash, dir_bytes).await.map_err(|e| {
                format!("failed to store parent DirectoryNode for {}: {:?}", path, e)
            })?;
            current_hash = dir_hash;
        }

        // Mount at prefix root (empty rel_path)
        self.mount_tree(prefix_id, "", current_hash.0)
            .await
            .map_err(|e| format!("failed to mount file {}: {}", path, e))?;

        Ok(())
    }

    /// Mount a tree manifest at a path within a prefix
    pub async fn mount_tree(
        &self,
        prefix_id: &str,
        rel_path: &str,
        tree_hash: Blake3Hash,
    ) -> Result<(), String> {
        let path = if rel_path.is_empty() {
            format!("/{}", prefix_id)
        } else {
            format!("/{}/{}", prefix_id, rel_path)
        };
        self.client
            .mount(path, tree_hash)
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    /// Flush all written files in a prefix to CAS
    pub async fn flush_prefix_to_cas(&self, prefix_id: &str) -> HashMap<String, (Blake3Hash, u64)> {
        // Call vx-vfsd's flush to get the root DirectoryNode hash
        let tree_hash = match self.flush_prefix_manifest(prefix_id, "").await {
            Ok(hash) => hash,
            Err(e) => {
                tracing::error!("failed to flush prefix {}: {}", prefix_id, e);
                return HashMap::new();
            }
        };

        // Walk the DirectoryNode tree recursively to collect all files
        let mut result = HashMap::new();
        if let Err(e) = self
            .collect_files_from_directory_node(NodeHash(tree_hash), "", &mut result)
            .await
        {
            tracing::error!("failed to walk DirectoryNode tree {}: {}", tree_hash, e);
        }

        result
    }

    /// Recursively walk a DirectoryNode tree and collect file entries
    async fn collect_files_from_directory_node(
        &self,
        node_hash: NodeHash,
        prefix: &str,
        result: &mut HashMap<String, (Blake3Hash, u64)>,
    ) -> Result<(), String> {
        let node_data = self
            .cas
            .get_node(node_hash)
            .await
            .map_err(|e| format!("failed to fetch DirectoryNode: {:?}", e))?
            .ret
            .clone()
            .ok_or_else(|| format!("DirectoryNode {} not found in CAS", node_hash))?;

        let node = CasNode::from_bytes(&node_data)
            .map_err(|e| format!("failed to deserialize DirectoryNode: {}", e))?;

        let dir = match node {
            CasNode::Directory(d) => d,
            _ => {
                return Err(format!(
                    "expected DirectoryNode at {}, got different variant",
                    node_hash
                ));
            }
        };

        for entry in &dir.entries {
            let entry_path = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", prefix, entry.name)
            };

            match &entry.kind {
                DirEntryKind::SmallFile {
                    content_hash, size, ..
                } => {
                    result.insert(entry_path, (content_hash.0, *size));
                }
                DirEntryKind::LargeFile {
                    content_hash, size, ..
                } => {
                    result.insert(entry_path, (content_hash.0, *size));
                }
                DirEntryKind::Dir { hash, .. } => {
                    Box::pin(self.collect_files_from_directory_node(*hash, &entry_path, result))
                        .await?;
                }
                DirEntryKind::Symlink { .. } => {
                    // Skip symlinks
                }
            }
        }

        Ok(())
    }

    /// Flush a prefix and return the tree manifest hash
    pub async fn flush_prefix_manifest(
        &self,
        prefix_id: &str,
        subpath: &str,
    ) -> Result<Blake3Hash, String> {
        let path = if subpath.is_empty() {
            format!("/{}", prefix_id)
        } else {
            format!("/{}/{}", prefix_id, subpath)
        };
        self.client
            .flush(path)
            .await
            .map(|resp| resp.ret)
            .map_err(|e| format!("{e:?}"))
    }

    /// Remove an action prefix
    pub async fn remove_prefix(&self, prefix_id: &str) {
        let path = format!("/{}", prefix_id);
        let _ = self.client.rm_rf(path).await;
    }
}
