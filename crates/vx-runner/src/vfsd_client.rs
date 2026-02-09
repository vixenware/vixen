//! RPC client for vx-vfsd

use std::collections::HashMap;
use std::sync::Arc;
use vx_store_proto::{Blake3Hash, StoreClient};

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
    /// Implemented by creating a single-file tree manifest and mounting it.
    pub async fn add_file_to_prefix(
        &self,
        prefix_id: &str,
        path: &str,
        cas_hash: Blake3Hash,
        size: u64,
        executable: bool,
    ) -> Result<(), String> {
        use vx_store_proto::TreeManifest;

        // Create a tree manifest with just this one file
        let mut manifest = TreeManifest::new();
        manifest.add_file(path.to_string(), cas_hash, size, executable);
        manifest.finalize();

        // Serialize and hash the manifest
        let manifest_json = facet_json::to_string(&manifest)
            .map_err(|e| format!("failed to serialize tree manifest for {}: {}", path, e))?;
        let manifest_hash = Blake3Hash::from_bytes(manifest_json.as_bytes());

        // Store manifest in CAS
        self.cas
            .put_blob(manifest_json.into_bytes())
            .await
            .map_err(|e| format!("failed to store tree manifest for {}: {:?}", path, e))?;

        // Mount at prefix root (empty rel_path)
        self.mount_tree(prefix_id, "", manifest_hash)
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
        self.client.mount(path, tree_hash).await.map_err(|e| e.to_string())
    }

    /// Flush all written files in a prefix to CAS
    pub async fn flush_prefix_to_cas(&self, prefix_id: &str) -> HashMap<String, (Blake3Hash, u64)> {
        // Call vx-vfsd's flush to get tree manifest hash
        let tree_hash = match self.flush_prefix_manifest(prefix_id, "").await {
            Ok(hash) => hash,
            Err(e) => {
                tracing::error!("failed to flush prefix {}: {}", prefix_id, e);
                return HashMap::new();
            }
        };

        // Fetch the tree manifest from CAS
        let manifest = match self.cas.get_tree_manifest(tree_hash).await {
            Ok(Some(m)) => m,
            Ok(None) => {
                tracing::error!("tree manifest {} not found after flush", tree_hash);
                return HashMap::new();
            }
            Err(e) => {
                tracing::error!("failed to fetch tree manifest {}: {:?}", tree_hash, e);
                return HashMap::new();
            }
        };

        // Build HashMap of file paths to (hash, size)
        let mut result = HashMap::new();
        for entry in &manifest.entries {
            if let vx_store_proto::TreeEntryKind::File { blob, size, .. } = &entry.kind {
                result.insert(entry.path.clone(), (*blob, *size));
            }
        }

        result
    }

    /// Flush a prefix and return the tree manifest hash
    pub async fn flush_prefix_manifest(&self, prefix_id: &str, subpath: &str) -> Result<Blake3Hash, String> {
        let path = if subpath.is_empty() {
            format!("/{}", prefix_id)
        } else {
            format!("/{}/{}", prefix_id, subpath)
        };
        self.client.flush(path).await.map_err(|e| e.to_string())
    }

    /// Remove an action prefix
    pub async fn remove_prefix(&self, prefix_id: &str) {
        let path = format!("/{}", prefix_id);
        let _ = self.client.rm_rf(path).await;
    }
}
