//! RPC protocol between vx-runner and vx-vfsd.

use facet::Facet;
use vx_services::{Blake3Hash, NodeHash};

#[derive(Debug, Clone, Facet)]
pub struct VfsPrefixStateSummary {
    pub prefix_id: String,
    pub prefix_found: bool,
    pub total_items: u64,
    pub total_prefixes: u64,
    pub path_mappings_count: u64,
    pub root_children: Vec<String>,
    pub prefix_children: Vec<String>,
    pub src_children: Vec<String>,
    pub toolchain_children: Vec<String>,
    pub toolchain_bin_children: Vec<String>,
    pub path_mappings_sample: Vec<String>,
    pub src_path_mappings_sample: Vec<String>,
}

#[derive(Debug, Clone, Facet, PartialEq, Eq)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum VfsTrackedObservation {
    File {
        path: String,
        content_hash: Blake3Hash,
        blob_node: Option<NodeHash>,
        size: u64,
    },
    Directory {
        path: String,
        directory_node: NodeHash,
    },
    LookupMiss {
        path: String,
        parent_path: String,
        directory_node: NodeHash,
    },
}

/// VFS daemon RPC service.
///
/// Manages a FUSE-backed virtual filesystem for build sandboxes.
/// Serves content from CAS (vx-store) and collects build outputs.
#[vox::service]
#[allow(async_fn_in_trait)]
pub trait Vfsd {
    /// Create a directory at the given path.
    ///
    /// Parent directory must exist (standard mkdir semantics).
    async fn mkdir(&self, path: String) -> Result<(), String>;

    /// Mount a tree manifest at the given path.
    ///
    /// Fetches the manifest from CAS and registers all entries in the VFS.
    /// Parent directory must exist.
    ///
    /// # Arguments
    /// - `path`: Mount point (e.g., "/build-xyz/src/")
    /// - `tree_hash`: Blake3 hash of the tree manifest in CAS
    async fn mount(&self, path: String, tree_hash: Blake3Hash) -> Result<(), String>;

    /// Flush written files to CAS and return the tree manifest hash.
    ///
    /// Collects all files written under `path`, uploads them to CAS as blobs,
    /// creates a tree manifest, uploads the manifest, and returns its hash.
    ///
    /// # Arguments
    /// - `path`: Directory to flush (e.g., "/build-xyz/out/")
    ///
    /// # Returns
    /// Blake3 hash of the generated tree manifest
    async fn flush(&self, path: String) -> Result<Blake3Hash, String>;

    /// Recursively remove a path from the VFS.
    ///
    /// Cleans up the path and all its descendants from VFS memory.
    /// Does not affect CAS — blobs remain stored.
    ///
    /// # Arguments
    /// - `path`: Root path to remove (e.g., "/build-xyz/")
    async fn rm_rf(&self, path: String) -> Result<(), String>;

    /// Health check / readiness probe.
    ///
    /// Returns immediately if the daemon is ready to serve requests.
    async fn ping(&self) -> Result<(), String>;

    /// Dump VFS state for a specific prefix to stderr (for diagnostics).
    ///
    /// # Arguments
    /// - `prefix_id`: The prefix to dump (e.g., "build-xyz")
    async fn dump_state(&self, prefix_id: String) -> Result<(), String>;

    /// Return the real filesystem access path for this VFS session.
    async fn access_path(&self) -> Result<String, String>;

    /// Return a structured summary of VFS state for a specific prefix.
    ///
    /// This is useful for durable product diagnostics, since callers can log
    /// the returned summary in their own process rather than scraping `vx-vfsd`
    /// logs separately.
    async fn describe_prefix_state(
        &self,
        prefix_id: String,
    ) -> Result<VfsPrefixStateSummary, String>;

    /// Return tracked observations under the requested tracked roots.
    ///
    /// Paths are relative to the prefix root. Only observations rooted under one
    /// of `roots` are returned.
    async fn tracked_observations(
        &self,
        prefix_id: String,
        roots: Vec<String>,
    ) -> Result<Vec<VfsTrackedObservation>, String>;
}
