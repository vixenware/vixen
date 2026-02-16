//! VFS implementation backed by CAS
//!
//! This module implements the Vixen VFS trait, providing a virtual filesystem
//! that serves content from the Content-Addressed Storage (CAS).
//!
//! # Architecture
//!
//! The VFS is mounted once at a configured mount base (see `VX_RUNNER_MOUNT`)
//! and serves multiple action prefixes:
//!
//! ```text
//! <mount-base>/                   <- single persistent mount
//!   ├── build-01JFX.../           <- action prefix (sandboxed)
//!   │   ├── toolchain/            <- Zig/Rust toolchain from CAS
//!   │   │   └── bin/cc
//!   │   ├── src/                  <- source tree from CAS
//!   │   │   └── main.c
//!   │   └── out/                  <- outputs (written back to CAS)
//!   │       └── main.o
//!   └── build-01JFY.../           <- another action
//! ```
//!
//! Each action gets its own prefix directory. The sandbox (seatbelt on macOS,
//! seccomp/landlock on Linux) restricts the subprocess to only access its prefix.
//!
//! # Item ID Allocation
//!
//! - ID 1: Root directory (`/`)
//! - ID 2+: Dynamically allocated for prefixes and their contents

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use peeps::Mutex;
use vixenfs_proto::{
    CreateResult, DirEntry, GetAttributesResult, ItemAttributes, ItemId, ItemType, LookupResult,
    ReadDirResult, ReadResult, SetAttributesParams, Vfs, VfsResult, WriteResult, errno, mode,
};
use vx_store_proto::{Blake3Hash, BlobNode, CasNode, ChunkHash, NodeHash, StoreClient};

/// A virtual filesystem item
#[derive(Debug, Clone)]
pub struct VfsItem {
    pub id: ItemId,
    pub parent_id: ItemId,
    pub name: String,
    pub item_type: ItemType,
    pub mode: u32,
    /// For files: CAS content hash (None means empty/new file).
    /// For SmallFile: this is also the ChunkHash. For LargeFile: see blob_node.
    pub cas_hash: Option<Blake3Hash>,
    /// For LargeFile entries: the NodeHash of the BlobNode in CAS.
    /// None for SmallFile entries (single chunk, fetched via cas_hash).
    pub blob_node: Option<NodeHash>,
    /// For files: size in bytes
    pub size: u64,
}

/// An action prefix - represents a build directory like `/build-01JFX.../`
#[derive(Debug)]
pub struct ActionPrefix {
    /// Unique ID for this prefix (e.g., ULID)
    pub id: String,
    /// The ItemId of this prefix's root directory
    pub root_item_id: ItemId,
    /// Mapping of paths within prefix to CAS hashes
    /// e.g., "toolchain/bin/cc" -> Blake3Hash
    pub path_mappings: HashMap<String, PrefixEntry>,
}

/// An entry in a prefix - either a file or directory
#[derive(Debug, Clone)]
pub struct PrefixEntry {
    pub item_type: ItemType,
    /// For files: the CAS content hash
    pub cas_hash: Option<Blake3Hash>,
    /// For LargeFile entries: the NodeHash of the BlobNode
    pub blob_node: Option<NodeHash>,
    /// For files: size in bytes
    pub size: u64,
    /// File mode (permissions)
    pub mode: u32,
}

const ROOT_ITEM_ID: ItemId = 1;

/// VFS item storage with parent→children adjacency index.
///
/// All mutations must go through [`VfsItems::insert`] and [`VfsItems::remove`]
/// to keep the children index consistent.
#[derive(Clone)]
pub(crate) struct VfsItems {
    /// All items keyed by ItemId.
    items: HashMap<ItemId, VfsItem>,
    /// parent_id → { child_name → child_id } (BTreeMap for deterministic iteration order)
    children: HashMap<ItemId, BTreeMap<String, ItemId>>,
    /// In-memory write buffers for files being written (FUSE only).
    /// Stored separately from VfsItem so items can be cloned cheaply for snapshots.
    write_buffers: HashMap<ItemId, Vec<u8>>,
}

impl VfsItems {
    fn new() -> Self {
        Self {
            items: HashMap::new(),
            children: HashMap::new(),
            write_buffers: HashMap::new(),
        }
    }

    /// Insert an item, updating the children index.
    fn insert(&mut self, item: VfsItem) {
        let id = item.id;
        let parent_id = item.parent_id;
        let name = item.name.clone();
        self.items.insert(id, item);
        if parent_id != 0 {
            self.children.entry(parent_id).or_default().insert(name, id);
        }
    }

    /// Remove an item by id, updating the children index.
    pub(crate) fn remove(&mut self, id: ItemId) -> Option<VfsItem> {
        let item = self.items.remove(&id)?;
        self.write_buffers.remove(&id);
        // Remove from parent's children map
        if item.parent_id != 0
            && let Some(siblings) = self.children.get_mut(&item.parent_id)
        {
            siblings.remove(&item.name);
            if siblings.is_empty() {
                self.children.remove(&item.parent_id);
            }
        }
        // Remove this item's children entry (even if non-empty — caller is
        // responsible for removing descendants first)
        self.children.remove(&id);
        Some(item)
    }

    /// Look up a child by parent_id + name. O(1).
    pub(crate) fn lookup_child(&self, parent_id: ItemId, name: &str) -> Option<&VfsItem> {
        let child_id = self.children.get(&parent_id)?.get(name)?;
        self.items.get(child_id)
    }

    /// Get an item by id.
    pub(crate) fn get(&self, id: &ItemId) -> Option<&VfsItem> {
        self.items.get(id)
    }

    /// Get a mutable item by id.
    fn get_mut(&mut self, id: &ItemId) -> Option<&mut VfsItem> {
        self.items.get_mut(id)
    }

    /// Iterate over children of a parent. O(children count).
    fn children_of(&self, parent_id: ItemId) -> impl Iterator<Item = &VfsItem> {
        self.children
            .get(&parent_id)
            .into_iter()
            .flat_map(|kids| kids.values())
            .filter_map(|id| self.items.get(id))
    }

    /// Check if a parent has any children.
    fn has_children(&self, parent_id: ItemId) -> bool {
        self.children
            .get(&parent_id)
            .is_some_and(|kids| !kids.is_empty())
    }

    /// Check if a child name exists under parent.
    fn child_exists(&self, parent_id: ItemId, name: &str) -> bool {
        self.children
            .get(&parent_id)
            .is_some_and(|kids| kids.contains_key(name))
    }

    /// Rename an item: update the children index for old and new parent.
    fn rename(&mut self, item_id: ItemId, new_parent_id: ItemId, new_name: String) {
        if let Some(item) = self.items.get(&item_id) {
            let old_parent = item.parent_id;
            let old_name = item.name.clone();
            // Remove from old parent
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.remove(&old_name);
            }
        }
        // Update the item
        if let Some(item) = self.items.get_mut(&item_id) {
            item.parent_id = new_parent_id;
            item.name = new_name.clone();
        }
        // Add to new parent
        self.children
            .entry(new_parent_id)
            .or_default()
            .insert(new_name, item_id);
    }

    /// Total number of items.
    fn len(&self) -> usize {
        self.items.len()
    }

    /// Check if the map contains an item.
    fn contains_key(&self, id: &ItemId) -> bool {
        self.items.contains_key(id)
    }

    /// Clone items and children index without write buffers (cheap snapshot for flush).
    pub(crate) fn clone_without_buffers(&self) -> Self {
        Self {
            items: self.items.clone(),
            children: self.children.clone(),
            write_buffers: HashMap::new(),
        }
    }

    /// Iterate over children of a parent, sorted by name.
    /// BTreeMap guarantees lexicographic order, which matches the byte ordering
    /// required for deterministic DirectoryNode hashing.
    pub(crate) fn sorted_children_of(&self, parent_id: ItemId) -> Vec<(&str, &VfsItem)> {
        let Some(kids) = self.children.get(&parent_id) else {
            return Vec::new();
        };
        kids.iter()
            .filter_map(|(name, id)| Some((name.as_str(), self.items.get(id)?)))
            .collect()
    }
}

/// CAS-backed VFS implementation
pub(crate) struct VfsState {
    pub(crate) items: VfsItems,
    pub(crate) prefixes: HashMap<String, ActionPrefix>,
}

struct ChunkL1Cache {
    entries: HashMap<Blake3Hash, Arc<[u8]>>,
    recency: VecDeque<Blake3Hash>,
    current_bytes: usize,
    max_bytes: usize,
}

impl ChunkL1Cache {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: VecDeque::new(),
            current_bytes: 0,
            max_bytes,
        }
    }

    fn get(&mut self, hash: &Blake3Hash) -> Option<Arc<[u8]>> {
        let data = self.entries.get(hash).cloned()?;
        self.touch(hash);
        Some(data)
    }

    fn insert(&mut self, hash: Blake3Hash, data: Arc<[u8]>) {
        let data_len = data.len();
        if data_len > self.max_bytes {
            return;
        }

        if let Some(prev) = self.entries.remove(&hash) {
            self.current_bytes = self.current_bytes.saturating_sub(prev.len());
            self.remove_from_recency(&hash);
        }

        while self.current_bytes + data_len > self.max_bytes {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.current_bytes = self.current_bytes.saturating_sub(evicted.len());
            }
        }

        self.current_bytes += data_len;
        self.recency.push_back(hash);
        self.entries.insert(hash, data);
    }

    fn touch(&mut self, hash: &Blake3Hash) {
        self.remove_from_recency(hash);
        self.recency.push_back(*hash);
    }

    fn remove_from_recency(&mut self, hash: &Blake3Hash) {
        if let Some(pos) = self.recency.iter().position(|h| h == hash) {
            self.recency.remove(pos);
        }
    }
}

pub struct CasVfs {
    /// CAS client for fetching blobs
    pub(crate) cas: Arc<StoreClient<crate::StoreCaller>>,
    /// Unified VFS state lock: items + prefixes
    pub(crate) state: Mutex<VfsState>,
    /// Next available ItemId
    next_id: AtomicU64,
    /// L2 chunk cache (disk): persistent across vx-vfsd restarts.
    /// Layout: `{cache_dir}/{hex[..2]}/{hex}`.
    /// None if VX_HOME is not set.
    disk_cache_dir: Option<Utf8PathBuf>,
    /// BlobNode metadata cache: NodeHash → parsed BlobNode.
    /// BlobNodes are immutable (content-addressed) so caching is unconditionally safe.
    /// Small: each entry is ~100 bytes × MAX_CHUNKS_PER_BLOB chunk entries.
    blobnode_cache: Mutex<HashMap<NodeHash, Arc<BlobNode>>>,
    /// L1 chunk cache (memory): ChunkHash → bytes, bounded by total bytes.
    chunk_l1_cache: Mutex<ChunkL1Cache>,
    /// Diagnostic tracker for CAS-backed file identity stability.
    observed_identities: Mutex<HashMap<ItemId, ObservedIdentity>>,
    /// Mount path for reading back files written directly to disk (ProjFS on Windows).
    mount_path: Utf8PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedIdentity {
    cas_hash: Blake3Hash,
    blob_node: Option<NodeHash>,
    size: u64,
}

impl CasVfs {
    /// Create a new CAS-backed VFS
    ///
    /// `disk_cache_dir` enables persistent L2 blob cache across restarts.
    /// Pass `None` to disable (L1 memory-only).
    pub fn new(
        cas: Arc<StoreClient<crate::StoreCaller>>,
        disk_cache_dir: Option<Utf8PathBuf>,
        mount_path: Utf8PathBuf,
    ) -> Self {
        let mut items = VfsItems::new();

        // Create root directory (ID 1)
        items.insert(VfsItem {
            id: ROOT_ITEM_ID,
            parent_id: 0, // root has no parent
            name: String::new(),
            item_type: ItemType::Directory,
            mode: mode::DIRECTORY,
            cas_hash: None,
            blob_node: None,
            size: 0,
        });

        Self {
            cas,
            state: Mutex::new(
                "CasVfs::state",
                VfsState {
                    items,
                    prefixes: HashMap::new(),
                },
            ),
            next_id: AtomicU64::new(2), // 1=root
            disk_cache_dir,
            blobnode_cache: Mutex::new("CasVfs::blobnode_cache", HashMap::new()),
            chunk_l1_cache: Mutex::new(
                "CasVfs::chunk_l1_cache",
                ChunkL1Cache::new(Self::chunk_l1_cache_max_bytes()),
            ),
            observed_identities: Mutex::new("CasVfs::observed_identities", HashMap::new()),
            mount_path,
        }
    }

    /// Allocate a new ItemId
    fn alloc_id(&self) -> ItemId {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn chunk_l1_cache_max_bytes() -> usize {
        static MAX_BYTES: OnceLock<usize> = OnceLock::new();
        *MAX_BYTES.get_or_init(|| {
            std::env::var("VX_VFSD_L1_CHUNK_CACHE_BYTES")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(256 * 1024 * 1024)
        })
    }

    /// Compute the disk cache path for a chunk.
    /// Layout: `{cache_dir}/{hex[..2]}/{hex}`
    fn chunk_cache_path(&self, hash: &Blake3Hash) -> Option<Utf8PathBuf> {
        let dir = self.disk_cache_dir.as_ref()?;
        let hex = hash.to_hex();
        Some(dir.join(&hex[..2]).join(hex))
    }

    /// Fetch a chunk with L1 memory + L2 disk cache. On miss, fetches from CAS and writes through.
    async fn read_chunk_cached(&self, chunk_hash: ChunkHash) -> Result<Arc<[u8]>, String> {
        async fn fetch_from_cas(
            cas: &StoreClient<crate::StoreCaller>,
            chunk_hash: ChunkHash,
        ) -> Result<Arc<[u8]>, String> {
            let data = cas
                .get_chunk(chunk_hash)
                .await
                .map_err(|e| format!("CAS chunk read error: {e}"))?
                .ok_or_else(|| format!("chunk {} not found", chunk_hash.to_hex()))?;

            let actual_hash = Blake3Hash::from_bytes(&data);
            if actual_hash != chunk_hash.0 {
                return Err(format!(
                    "CAS returned wrong chunk bytes for {} (got {})",
                    chunk_hash.to_hex(),
                    actual_hash.to_hex()
                ));
            }

            Ok(data.into())
        }

        // Try L1 memory cache first
        if let Some(data) = self.chunk_l1_cache.lock().get(&chunk_hash.0) {
            return Ok(data);
        }

        // Try L2 disk cache first
        if let Some(path) = self.chunk_cache_path(&chunk_hash.0) {
            match peeps::fs::read(path.as_std_path()).await {
                Ok(data) => {
                    let actual_hash = Blake3Hash::from_bytes(&data);
                    if actual_hash == chunk_hash.0 {
                        let data: Arc<[u8]> = data.into();
                        self.chunk_l1_cache
                            .lock()
                            .insert(chunk_hash.0, Arc::clone(&data));
                        return Ok(data);
                    }

                    tracing::error!(
                        expected = %chunk_hash.to_hex(),
                        actual = %actual_hash.to_hex(),
                        bytes = data.len(),
                        path = %path,
                        "L2 cache hash mismatch on hit, evicting corrupted entry"
                    );
                    if let Err(e) = peeps::fs::remove_file(path.as_std_path()).await
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %path, error = %e, "failed to remove bad L2 cache file");
                    }
                    // Fall through to CAS refetch.
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Cache miss — fall through to RPC
                }
                Err(e) => {
                    tracing::warn!(hash = %chunk_hash.0, error = %e, "L2 cache read error");
                    // Fall through to RPC
                }
            }
        }

        // Fetch from CAS
        let data = fetch_from_cas(&self.cas, chunk_hash).await?;

        // Write-through to L2
        if let Some(path) = self.chunk_cache_path(&chunk_hash.0)
            && let Err(e) = vx_io::atomic_write(&path, &data[..], false).await
        {
            tracing::warn!(hash = %chunk_hash.0, error = %e, "L2 cache write-through failed");
        }

        self.chunk_l1_cache
            .lock()
            .insert(chunk_hash.0, Arc::clone(&data));
        Ok(data)
    }

    /// Get a cached BlobNode, fetching and parsing from CAS on miss.
    async fn get_blobnode_cached(&self, blob_node_hash: NodeHash) -> Result<Arc<BlobNode>, String> {
        // Check cache
        {
            let cache = self.blobnode_cache.lock();
            if let Some(blob) = cache.get(&blob_node_hash) {
                return Ok(Arc::clone(blob));
            }
        }

        // Cache miss — fetch from CAS
        let node_data = self
            .cas
            .get_node(blob_node_hash)
            .await
            .map_err(|e| format!("failed to read BlobNode: {e}"))?
            .ok_or_else(|| format!("BlobNode {} not found", blob_node_hash.to_hex()))?;

        let node = CasNode::from_bytes(&node_data)
            .map_err(|e| format!("failed to deserialize BlobNode: {e}"))?;

        let blob = match node {
            CasNode::Blob(b) => b,
            other => {
                return Err(format!(
                    "expected BlobNode, got {:?}",
                    std::mem::discriminant(&other)
                ));
            }
        };

        let blob = Arc::new(blob);

        // Insert into cache
        {
            let mut cache = self.blobnode_cache.lock();
            cache.insert(blob_node_hash, Arc::clone(&blob));
        }

        Ok(blob)
    }

    /// Read a range from a file stored in CAS.
    ///
    /// For SmallFile entries (blob_node is None): fetch single chunk via get_chunk.
    /// For LargeFile entries: fetch BlobNode (cached), then relevant chunks
    /// (disk-cached, fetched in parallel).
    async fn read_cas_range(
        &self,
        hash: Blake3Hash,
        blob_node: Option<NodeHash>,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, String> {
        match blob_node {
            None => {
                // SmallFile: single chunk, content_hash == chunk_hash
                let data = self.read_chunk_cached(ChunkHash(hash)).await?;

                // Slice to requested range
                let start = (offset as usize).min(data.len());
                let end = ((offset + len) as usize).min(data.len());
                Ok(data[start..end].to_vec())
            }
            Some(blob_node_hash) => {
                // LargeFile: get cached BlobNode metadata
                let blob = self.get_blobnode_cached(blob_node_hash).await?;

                // Identify which chunks intersect [offset, offset+len)
                let end = offset + len;
                let mut needed_chunks: Vec<(usize, ChunkHash, u64)> = Vec::new(); // (index, hash, chunk_offset)
                let mut chunk_offset: u64 = 0;

                for (idx, chunk_entry) in blob.chunks.iter().enumerate() {
                    let chunk_end = chunk_offset + chunk_entry.size as u64;

                    if chunk_end <= offset {
                        chunk_offset = chunk_end;
                        continue;
                    }
                    if chunk_offset >= end {
                        break;
                    }

                    needed_chunks.push((idx, chunk_entry.hash, chunk_offset));
                    chunk_offset = chunk_end;
                }

                if needed_chunks.len() == 1 {
                    // Single chunk — no need for parallel machinery
                    let (_, ch, co) = needed_chunks[0];
                    let chunk_data = self.read_chunk_cached(ch).await?;
                    let slice_start = if offset > co {
                        (offset - co) as usize
                    } else {
                        0
                    };
                    let chunk_end = co + chunk_data.len() as u64;
                    let slice_end = if end < chunk_end {
                        (end - co) as usize
                    } else {
                        chunk_data.len()
                    };
                    return Ok(chunk_data[slice_start..slice_end.min(chunk_data.len())].to_vec());
                }

                // Multiple chunks — fetch in parallel (bounded concurrency)
                use futures_util::stream::{self, StreamExt};

                let fetches: Vec<_> = needed_chunks
                    .iter()
                    .map(|&(idx, ch, co)| async move {
                        let data = self.read_chunk_cached(ch).await?;
                        Ok::<_, String>((idx, data, co))
                    })
                    .collect();

                let mut results: Vec<Option<(Arc<[u8]>, u64)>> = vec![None; needed_chunks.len()];

                // buffer_unordered(8): up to 8 concurrent chunk fetches
                let mut stream = stream::iter(fetches).buffer_unordered(8);
                while let Some(result) = stream.next().await {
                    let (idx, data, co) = result?;
                    // Map idx back to position in needed_chunks
                    let pos = needed_chunks
                        .iter()
                        .position(|&(i, _, _)| i == idx)
                        .unwrap();
                    results[pos] = Some((data, co));
                }

                // Assemble result in order
                let mut result = Vec::with_capacity(len as usize);
                for slot in &results {
                    let (chunk_data, co) = slot.as_ref().unwrap();
                    let chunk_end = *co + chunk_data.len() as u64;

                    let slice_start = if offset > *co {
                        (offset - co) as usize
                    } else {
                        0
                    };
                    let slice_end = if end < chunk_end {
                        (end - co) as usize
                    } else {
                        chunk_data.len()
                    };

                    if slice_start < chunk_data.len() {
                        result.extend_from_slice(
                            &chunk_data[slice_start..slice_end.min(chunk_data.len())],
                        );
                    }
                }

                Ok(result)
            }
        }
    }

    fn size_checks_enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var("VX_VFSD_VERIFY_SIZE")
                .ok()
                .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        })
    }

    fn observe_identity(&self, item_id: ItemId, identity: ObservedIdentity, context: &'static str) {
        let mut observed = self.observed_identities.lock();
        if let Some(prev) = observed.get(&item_id).copied()
            && prev != identity
        {
            tracing::error!(
                item_id,
                context,
                prev_hash = %prev.cas_hash,
                prev_blob_node = ?prev.blob_node,
                prev_size = prev.size,
                new_hash = %identity.cas_hash,
                new_blob_node = ?identity.blob_node,
                new_size = identity.size,
                "CAS-backed file identity changed for existing item_id"
            );
        }
        observed.insert(item_id, identity);
    }

    fn clear_observed_identity(&self, item_id: ItemId) {
        self.observed_identities.lock().remove(&item_id);
    }

    async fn cas_truth_size(
        &self,
        cas_hash: Blake3Hash,
        blob_node: Option<NodeHash>,
    ) -> Result<u64, String> {
        match blob_node {
            None => {
                let data = self.read_chunk_cached(ChunkHash(cas_hash)).await?;
                Ok(data.len() as u64)
            }
            Some(blob_node_hash) => {
                let blob = self.get_blobnode_cached(blob_node_hash).await?;
                Ok(blob.total_size)
            }
        }
    }

    async fn verify_size_consistency(
        &self,
        item_id: ItemId,
        identity: ObservedIdentity,
        context: &'static str,
    ) {
        if !Self::size_checks_enabled() {
            return;
        }
        match self
            .cas_truth_size(identity.cas_hash, identity.blob_node)
            .await
        {
            Ok(actual) if actual == identity.size => {}
            Ok(actual) => {
                tracing::error!(
                    item_id,
                    context,
                    hash = %identity.cas_hash,
                    blob_node = ?identity.blob_node,
                    expected_size = identity.size,
                    actual_size = actual,
                    "item.size differs from CAS truth size"
                );
            }
            Err(e) => {
                tracing::error!(
                    item_id,
                    context,
                    hash = %identity.cas_hash,
                    blob_node = ?identity.blob_node,
                    error = %e,
                    "failed to compute CAS truth size for consistency check"
                );
            }
        }
    }
    /// Check if a prefix already exists
    pub async fn has_prefix(&self, prefix_id: &str) -> bool {
        self.state.lock().prefixes.contains_key(prefix_id)
    }

    /// Create a new action prefix
    ///
    /// Returns the prefix ID (e.g., "build-01JFX...") and the root ItemId
    pub async fn create_prefix(&self, prefix_id: String) -> (String, ItemId) {
        let prefix_root_id = self.alloc_id();

        {
            let mut state = self.state.lock();

            // Create the prefix directory under root
            state.items.insert(VfsItem {
                id: prefix_root_id,
                parent_id: 1, // under root
                name: prefix_id.clone(),
                item_type: ItemType::Directory,
                mode: mode::DIRECTORY,
                cas_hash: None,
                blob_node: None,
                size: 0,
            });

            // Register the prefix
            state.prefixes.insert(
                prefix_id.clone(),
                ActionPrefix {
                    id: prefix_id.clone(),
                    root_item_id: prefix_root_id,
                    path_mappings: HashMap::new(),
                },
            );
        }

        tracing::debug!(prefix_id = %prefix_id, item_id = prefix_root_id, "created action prefix");
        (prefix_id, prefix_root_id)
    }

    /// Add a file to a prefix from CAS
    ///
    /// `path` is relative to the prefix root, e.g., "toolchain/bin/cc"
    pub async fn add_file_to_prefix(
        &self,
        prefix_id: &str,
        path: &str,
        cas_hash: Blake3Hash,
        blob_node: Option<NodeHash>,
        size: u64,
        executable: bool,
    ) -> Option<ItemId> {
        tracing::trace!(
            prefix_id = %prefix_id,
            path = %path,
            hash = %cas_hash,
            size,
            executable,
            "VFS add_file_to_prefix"
        );

        let file_mode = if executable {
            mode::FILE_EXECUTABLE
        } else {
            mode::FILE_REGULAR
        };

        let result = self
            .add_entry_to_prefix(
                prefix_id,
                path,
                PrefixEntry {
                    item_type: ItemType::File,
                    cas_hash: Some(cas_hash),
                    blob_node,
                    size,
                    mode: file_mode,
                },
            )
            .await;

        tracing::trace!(
            prefix_id = %prefix_id,
            path = %path,
            item_id = ?result,
            "VFS add_file_to_prefix -> result"
        );

        result
    }

    /// Add a directory to a prefix
    pub async fn add_dir_to_prefix(&self, prefix_id: &str, path: &str) -> Option<ItemId> {
        self.add_entry_to_prefix(
            prefix_id,
            path,
            PrefixEntry {
                item_type: ItemType::Directory,
                cas_hash: None,
                blob_node: None,
                size: 0,
                mode: mode::DIRECTORY,
            },
        )
        .await
    }

    /// Internal: add an entry (file or dir) to a prefix.
    ///
    /// Creates intermediate directories as needed. Check-and-create happens
    /// under a single write lock to avoid TOCTOU races.
    async fn add_entry_to_prefix(
        &self,
        prefix_id: &str,
        path: &str,
        entry: PrefixEntry,
    ) -> Option<ItemId> {
        // Split path into components and create intermediate directories
        let components: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            return None;
        }

        let mut state = self.state.lock();
        let prefix_root_id = state.prefixes.get(prefix_id)?.root_item_id;

        let mut current_parent = prefix_root_id;

        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;

            // Check if this component already exists (O(1) via children index)
            if let Some(existing) = state.items.lookup_child(current_parent, component) {
                let existing_id = existing.id;
                let existing_type = existing.item_type;
                if is_last {
                    return Some(existing_id);
                } else if existing_type == ItemType::Directory {
                    current_parent = existing_id;
                    continue;
                } else {
                    // Path component is a file, can't traverse
                    return None;
                }
            }

            // Create the component
            let new_id = self.alloc_id();
            let (item_type, item_mode, cas_hash, blob_node, size) = if is_last {
                (
                    entry.item_type,
                    entry.mode,
                    entry.cas_hash,
                    entry.blob_node,
                    entry.size,
                )
            } else {
                (ItemType::Directory, mode::DIRECTORY, None, None, 0)
            };

            state.items.insert(VfsItem {
                id: new_id,
                parent_id: current_parent,
                name: component.to_string(),
                item_type,
                mode: item_mode,
                cas_hash,
                blob_node,
                size,
            });

            if is_last {
                if let Some(prefix) = state.prefixes.get_mut(prefix_id) {
                    prefix.path_mappings.insert(path.to_string(), entry);
                }
                return Some(new_id);
            }

            current_parent = new_id;
        }

        None
    }

    /// Remove an action prefix and all its contents
    pub async fn remove_prefix(&self, prefix_id: &str) {
        let mut state = self.state.lock();
        let Some(prefix) = state.prefixes.get(prefix_id) else {
            return;
        };
        let prefix_root_id = prefix.root_item_id;

        let to_remove = Self::collect_descendants_from(&state.items, prefix_root_id);
        // Remove in reverse (leaves first) to keep children index clean
        for &id in to_remove.iter().rev() {
            state.items.remove(id);
        }

        state.prefixes.remove(prefix_id);
        drop(state);
        for id in to_remove {
            self.clear_observed_identity(id);
        }

        tracing::debug!(prefix_id = %prefix_id, "removed action prefix");
    }

    /// Collect all descendant ItemIds (including the item itself).
    /// O(number of descendants) via children index.
    pub(crate) fn collect_descendants_from(items: &VfsItems, root_id: ItemId) -> Vec<ItemId> {
        let mut result = vec![root_id];
        let mut queue = vec![root_id];

        while let Some(parent_id) = queue.pop() {
            for child in items.children_of(parent_id) {
                result.push(child.id);
                if child.item_type == ItemType::Directory {
                    queue.push(child.id);
                }
            }
        }

        result
    }
}

impl Vfs for CasVfs {
    async fn lookup(&self, _cx: &roam::Context, parent_id: ItemId, name: String) -> LookupResult {
        tracing::trace!(parent_id, name = %name, "VFS lookup");

        let state = self.state.lock();
        if let Some(item) = state.items.lookup_child(parent_id, &name) {
            tracing::trace!(parent_id, name = %name, item_id = item.id, item_type = ?item.item_type, "VFS lookup -> found");
            return LookupResult {
                attrs: ItemAttributes {
                    item_id: item.id,
                    item_type: item.item_type,
                    size: item.size,
                    modified_time: 0,
                    created_time: 0,
                    mode: item.mode,
                },
                error: errno::OK,
            };
        }

        tracing::debug!(parent_id, name = %name, "VFS lookup -> NOT FOUND");
        LookupResult {
            attrs: ItemAttributes {
                item_id: 0,
                item_type: ItemType::File,
                size: 0,
                modified_time: 0,
                created_time: 0,
                mode: 0,
            },
            error: errno::ENOENT,
        }
    }

    async fn get_attributes(&self, _cx: &roam::Context, item_id: ItemId) -> GetAttributesResult {
        tracing::trace!(item_id, "VFS get_attributes");

        let (attrs, identity_to_check) = {
            let state = self.state.lock();
            match state.items.get(&item_id) {
                Some(item) => {
                    let identity_to_check = if item.item_type == ItemType::File
                        && let Some(cas_hash) = item.cas_hash
                        && !state.items.write_buffers.contains_key(&item_id)
                    {
                        Some(ObservedIdentity {
                            cas_hash,
                            blob_node: item.blob_node,
                            size: item.size,
                        })
                    } else {
                        None
                    };
                    (
                        ItemAttributes {
                            item_id: item.id,
                            item_type: item.item_type,
                            size: item.size,
                            modified_time: 0,
                            created_time: 0,
                            mode: item.mode,
                        },
                        identity_to_check,
                    )
                }
                None => {
                    return GetAttributesResult {
                        attrs: ItemAttributes {
                            item_id: 0,
                            item_type: ItemType::File,
                            size: 0,
                            modified_time: 0,
                            created_time: 0,
                            mode: 0,
                        },
                        error: errno::ENOENT,
                    };
                }
            }
        };

        if let Some(identity) = identity_to_check {
            self.observe_identity(item_id, identity, "get_attributes");
            self.verify_size_consistency(item_id, identity, "get_attributes")
                .await;
        }

        GetAttributesResult {
            attrs,
            error: errno::OK,
        }
    }

    async fn read_dir(&self, _cx: &roam::Context, item_id: ItemId, cursor: u64) -> ReadDirResult {
        let started = Instant::now();
        tracing::trace!(item_id, cursor, "VFS read_dir.start");

        let state = self.state.lock();
        let (result, child_count) = match state.items.get(&item_id) {
            Some(item) if item.item_type == ItemType::Directory => {
                let children: Vec<_> = state.items.children_of(item_id).collect();

                let entries: Vec<DirEntry> = children
                    .iter()
                    .skip(cursor as usize)
                    .take(100)
                    .map(|child| DirEntry {
                        name: child.name.clone(),
                        item_id: child.id,
                        item_type: child.item_type,
                        size: child.size,
                        modified_time: 0,
                        created_time: 0,
                        mode: child.mode,
                    })
                    .collect();

                let has_more = children.len() > (cursor as usize + entries.len());
                let next_cursor = if has_more {
                    cursor + entries.len() as u64
                } else {
                    0
                };

                (
                    ReadDirResult {
                        entries,
                        next_cursor,
                        error: errno::OK,
                    },
                    Some(children.len()),
                )
            }
            Some(_) => (
                ReadDirResult {
                    entries: Vec::new(),
                    next_cursor: 0,
                    error: errno::ENOTDIR,
                },
                None,
            ),
            None => (
                ReadDirResult {
                    entries: Vec::new(),
                    next_cursor: 0,
                    error: errno::ENOENT,
                },
                None,
            ),
        };
        drop(state);

        let elapsed = started.elapsed();
        tracing::trace!(
            item_id,
            cursor,
            child_count = ?child_count,
            entries = result.entries.len(),
            next_cursor = result.next_cursor,
            error = result.error,
            elapsed_us = elapsed.as_micros(),
            "VFS read_dir.done"
        );
        if elapsed >= Duration::from_millis(500) {
            tracing::warn!(
                item_id,
                cursor,
                child_count = ?child_count,
                entries = result.entries.len(),
                next_cursor = result.next_cursor,
                error = result.error,
                elapsed_ms = elapsed.as_millis(),
                "VFS read_dir.slow"
            );
        }

        result
    }

    async fn read(
        &self,
        _cx: &roam::Context,
        item_id: ItemId,
        offset: u64,
        len: u64,
    ) -> ReadResult {
        tracing::trace!(item_id, offset, len, "VFS read");

        // Extract what we need while holding the read lock — slice the write buffer
        // in-place instead of cloning the entire Vec<u8>.
        let (cas_hash, blob_node, size) = {
            let state = self.state.lock();
            let Some(item) = state.items.get(&item_id) else {
                return ReadResult {
                    data: Vec::new(),
                    error: errno::ENOENT,
                };
            };

            if item.item_type != ItemType::File {
                return ReadResult {
                    data: Vec::new(),
                    error: errno::EISDIR,
                };
            }

            // If there's buffered write data, serve from that (it's newer than CAS)
            if let Some(data) = state.items.write_buffers.get(&item_id) {
                let start = (offset as usize).min(data.len());
                let end = ((offset + len) as usize).min(data.len());
                return ReadResult {
                    data: data[start..end].to_vec(),
                    error: errno::OK,
                };
            }

            (item.cas_hash, item.blob_node, item.size)
        };

        // Fetch from CAS if we have a hash
        let Some(hash) = cas_hash else {
            // Empty file
            return ReadResult {
                data: Vec::new(),
                error: errno::OK,
            };
        };

        let identity = ObservedIdentity {
            cas_hash: hash,
            blob_node,
            size,
        };
        self.observe_identity(item_id, identity, "read");
        self.verify_size_consistency(item_id, identity, "read")
            .await;

        // Fetch via chunk-based CAS reads (SmallFile → single chunk, LargeFile → BlobNode)
        match self.read_cas_range(hash, blob_node, offset, len).await {
            Ok(data) => {
                let expected_len = if offset >= size {
                    0
                } else {
                    (size - offset).min(len) as usize
                };
                if data.len() != expected_len {
                    tracing::error!(
                        item_id,
                        hash = %hash,
                        file_size = size,
                        offset,
                        requested = len,
                        expected = expected_len,
                        actual = data.len(),
                        "short read before EOF detected in CAS-backed read path"
                    );
                    return ReadResult {
                        data: Vec::new(),
                        error: errno::EIO,
                    };
                }
                ReadResult {
                    data,
                    error: errno::OK,
                }
            }
            Err(e) => {
                tracing::error!(hash = %hash, error = %e, "failed to read blob range");
                ReadResult {
                    data: Vec::new(),
                    error: errno::EIO,
                }
            }
        }
    }

    async fn write(
        &self,
        _cx: &roam::Context,
        item_id: ItemId,
        offset: u64,
        data: Vec<u8>,
    ) -> WriteResult {
        tracing::trace!(item_id, offset, len = data.len(), "write");

        let mut state = self.state.lock();

        match state.items.get(&item_id).map(|i| i.item_type) {
            None => {
                return WriteResult {
                    bytes_written: 0,
                    error: errno::ENOENT,
                };
            }
            Some(t) if t != ItemType::File => {
                return WriteResult {
                    bytes_written: 0,
                    error: errno::EISDIR,
                };
            }
            _ => {}
        }

        // Initialize or extend write buffer
        let buffer = state.items.write_buffers.entry(item_id).or_default();
        let end = offset as usize + data.len();

        // Extend buffer if needed
        if buffer.len() < end {
            buffer.resize(end, 0);
        }

        // Copy data into buffer
        buffer[offset as usize..end].copy_from_slice(&data);
        let new_size = buffer.len() as u64;

        // Update size on the item
        state.items.get_mut(&item_id).unwrap().size = new_size;
        drop(state);
        self.clear_observed_identity(item_id);

        WriteResult {
            bytes_written: data.len() as u64,
            error: errno::OK,
        }
    }

    async fn create(
        &self,
        _cx: &roam::Context,
        parent_id: ItemId,
        name: String,
        item_type: ItemType,
    ) -> CreateResult {
        tracing::trace!(parent_id, name = %name, ?item_type, "create");

        // Single write lock for check+create (no TOCTOU)
        let new_id = self.alloc_id();
        let item_mode = match item_type {
            ItemType::Directory => mode::DIRECTORY,
            ItemType::File => mode::FILE_REGULAR,
            ItemType::Symlink => mode::FILE_REGULAR,
        };

        let mut state = self.state.lock();

        // Verify parent exists and is a directory
        match state.items.get(&parent_id) {
            Some(item) if item.item_type == ItemType::Directory => {}
            Some(_) => {
                return CreateResult {
                    item_id: 0,
                    error: errno::ENOTDIR,
                };
            }
            None => {
                return CreateResult {
                    item_id: 0,
                    error: errno::ENOENT,
                };
            }
        }

        // Check if name already exists (O(1) via index)
        if state.items.child_exists(parent_id, &name) {
            return CreateResult {
                item_id: 0,
                error: errno::EEXIST,
            };
        }

        state.items.insert(VfsItem {
            id: new_id,
            parent_id,
            name,
            item_type,
            mode: item_mode,
            cas_hash: None,
            blob_node: None,
            size: 0,
        });

        CreateResult {
            item_id: new_id,
            error: errno::OK,
        }
    }

    async fn delete(&self, _cx: &roam::Context, item_id: ItemId) -> VfsResult {
        tracing::trace!(item_id, "delete");

        if item_id == ROOT_ITEM_ID {
            return VfsResult {
                error: errno::EACCES,
            };
        }

        let mut state = self.state.lock();

        let Some(item) = state.items.get(&item_id) else {
            return VfsResult {
                error: errno::ENOENT,
            };
        };

        // If directory, check if empty (O(1) via index)
        if item.item_type == ItemType::Directory && state.items.has_children(item_id) {
            return VfsResult {
                error: errno::ENOTEMPTY,
            };
        }

        state.items.remove(item_id);
        drop(state);
        self.clear_observed_identity(item_id);
        VfsResult { error: errno::OK }
    }

    async fn rename(
        &self,
        _cx: &roam::Context,
        item_id: ItemId,
        new_parent_id: ItemId,
        new_name: String,
    ) -> VfsResult {
        tracing::trace!(item_id, new_parent_id, new_name = %new_name, "rename");

        if item_id == ROOT_ITEM_ID {
            return VfsResult {
                error: errno::EACCES,
            };
        }

        let mut state = self.state.lock();

        if !state.items.contains_key(&item_id) {
            return VfsResult {
                error: errno::ENOENT,
            };
        }

        // Check new parent exists and is directory
        match state.items.get(&new_parent_id) {
            Some(p) if p.item_type == ItemType::Directory => {}
            Some(_) => {
                return VfsResult {
                    error: errno::ENOTDIR,
                };
            }
            None => {
                return VfsResult {
                    error: errno::ENOENT,
                };
            }
        }

        // Check name doesn't exist in new parent (O(1) via index)
        if state.items.child_exists(new_parent_id, &new_name) {
            // Could be ourselves moving to same parent with same name — check
            if let Some(existing) = state.items.lookup_child(new_parent_id, &new_name)
                && existing.id != item_id
            {
                return VfsResult {
                    error: errno::EEXIST,
                };
            }
        }

        state.items.rename(item_id, new_parent_id, new_name);
        VfsResult { error: errno::OK }
    }

    async fn set_attributes(
        &self,
        _cx: &roam::Context,
        item_id: ItemId,
        params: SetAttributesParams,
    ) -> VfsResult {
        tracing::trace!(item_id, ?params, "set_attributes");

        if item_id == ROOT_ITEM_ID {
            return VfsResult { error: errno::OK };
        }

        let mut state = self.state.lock();

        if !state.items.contains_key(&item_id) {
            return VfsResult {
                error: errno::ENOENT,
            };
        }

        if let Some(mode) = params.mode {
            state.items.get_mut(&item_id).unwrap().mode = mode;
        }

        if let Some(new_size) = params.size {
            let buffer = state.items.write_buffers.entry(item_id).or_default();
            buffer.resize(new_size as usize, 0);
            state.items.get_mut(&item_id).unwrap().size = new_size;
            drop(state);
            self.clear_observed_identity(item_id);
            return VfsResult { error: errno::OK };
        }

        // modified_time is ignored for now (we don't track times)

        VfsResult { error: errno::OK }
    }

    async fn ping(&self, _cx: &roam::Context) -> String {
        "pong from vx-runner CAS VFS".to_string()
    }
}

/// Flush a file's write buffer to CAS and return the blob hash
impl CasVfs {
    pub async fn flush_to_cas(&self, item_id: ItemId) -> Option<(Blake3Hash, u64)> {
        let data = {
            let mut state = self.state.lock();
            state.items.get(&item_id)?; // check item exists
            state.items.write_buffers.remove(&item_id)?
        };

        if data.is_empty() {
            return None;
        }

        let size = data.len() as u64;
        let content_hash = Blake3Hash::from_bytes(&data);
        let chunk_hash = ChunkHash(content_hash);
        match self.cas.put_chunk(chunk_hash, data.clone()).await {
            Ok(()) => {
                let mut state = self.state.lock();
                if let Some(item) = state.items.get_mut(&item_id) {
                    item.cas_hash = Some(content_hash);
                    item.blob_node = None; // single chunk
                    item.size = size;
                }
                drop(state);
                self.clear_observed_identity(item_id);
                Some((content_hash, size))
            }
            Err(e) => {
                tracing::error!(item_id, error = %e, "failed to flush to CAS, restoring write buffer");
                // Restore the write buffer so data isn't lost
                let mut state = self.state.lock();
                if let Some(item) = state.items.get_mut(&item_id) {
                    item.size = data.len() as u64;
                }
                state.items.write_buffers.insert(item_id, data);
                None
            }
        }
    }

    /// Flush all written files in a prefix to CAS.
    /// Returns a map of relative paths to their CAS hashes.
    ///
    /// On FUSE (macOS/Linux), flushes in-memory write buffers.
    /// On ProjFS (Windows), reads files from the on-disk mount since writes
    /// bypass the VFS entirely.
    pub async fn flush_prefix_to_cas(&self, prefix_id: &str) -> HashMap<String, (Blake3Hash, u64)> {
        if cfg!(windows) {
            self.flush_prefix_from_disk(prefix_id).await
        } else {
            self.flush_prefix_from_buffers(prefix_id).await
        }
    }

    /// FUSE path: flush in-memory write buffers to CAS.
    async fn flush_prefix_from_buffers(
        &self,
        prefix_id: &str,
    ) -> HashMap<String, (Blake3Hash, u64)> {
        let mut results = HashMap::new();

        let prefix_root_id = {
            let state = self.state.lock();
            match state.prefixes.get(prefix_id) {
                Some(p) => p.root_item_id,
                None => return results,
            }
        };

        let items_to_flush: Vec<(ItemId, String)> = {
            let state = self.state.lock();
            let descendants = Self::collect_descendants_from(&state.items, prefix_root_id);

            descendants
                .into_iter()
                .filter_map(|id| {
                    state.items.get(&id)?; // check item exists
                    if state.items.write_buffers.contains_key(&id) {
                        let path = self.build_path(&state.items, id, prefix_root_id);
                        Some((id, path))
                    } else {
                        None
                    }
                })
                .collect()
        };

        for (item_id, path) in items_to_flush {
            if let Some(hash_and_size) = self.flush_to_cas(item_id).await {
                results.insert(path, hash_and_size);
            }
        }

        results
    }

    /// ProjFS path: walk the on-disk prefix directory and read files into CAS.
    async fn flush_prefix_from_disk(&self, prefix_id: &str) -> HashMap<String, (Blake3Hash, u64)> {
        let mut results = HashMap::new();
        let prefix_dir = self.mount_path.join(prefix_id);

        let walker = match std::fs::read_dir(prefix_dir.as_std_path()) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(%prefix_dir, error = %e, "failed to read prefix dir from disk");
                return results;
            }
        };

        // Recursively walk and collect all files
        let mut stack: Vec<(std::path::PathBuf, String)> = vec![];
        for entry in walker.flatten() {
            stack.push((entry.path(), String::new()));
        }

        while let Some((path, rel_parent)) = stack.pop() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let rel_path = if rel_parent.is_empty() {
                name.to_string()
            } else {
                format!("{}/{}", rel_parent, name)
            };

            if path.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    for entry in entries.flatten() {
                        stack.push((entry.path(), rel_path.clone()));
                    }
                }
            } else if path.is_file() {
                match peeps::fs::read(&path).await {
                    Ok(data) => {
                        let size = data.len() as u64;
                        let content_hash = Blake3Hash::from_bytes(&data);
                        let chunk_hash = ChunkHash(content_hash);
                        match self.cas.put_chunk(chunk_hash, data).await {
                            Ok(()) => {
                                tracing::debug!(
                                    path = %rel_path,
                                    size,
                                    hash = %content_hash,
                                    "flushed file from disk to CAS"
                                );
                                results.insert(rel_path, (content_hash, size));
                            }
                            Err(e) => {
                                tracing::error!(path = %rel_path, error = %e, "failed to put chunk");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(path = %rel_path, error = %e, "failed to read file from disk");
                    }
                }
            }
        }

        results
    }

    /// Dump VFS state for debugging
    #[cfg(unix)]
    pub fn dump_all_state(&self) {
        let state = self.state.lock();

        eprintln!("\n=== VFS STATE DUMP ===");
        eprintln!("Total items: {}", state.items.len());
        eprintln!("Total prefixes: {}", state.prefixes.len());

        for (prefix_id, prefix) in state.prefixes.iter() {
            eprintln!(
                "\nPrefix '{}' root_item_id={}",
                prefix_id, prefix.root_item_id
            );

            let children: Vec<_> = state
                .items
                .children_of(prefix.root_item_id)
                .map(|i| format!("{} (type={:?})", i.name, i.item_type))
                .collect();
            eprintln!("  Children: {:?}", children);
        }
    }

    pub async fn dump_state(&self, prefix_id: &str) {
        let state = self.state.lock();

        tracing::debug!("=== VFS STATE DUMP ===");
        tracing::debug!("Total items: {}", state.items.len());
        tracing::debug!("Total prefixes: {}", state.prefixes.len());

        // Show root children
        let root_children: Vec<_> = state
            .items
            .children_of(ROOT_ITEM_ID)
            .map(|i| format!("{} (id={})", i.name, i.id))
            .collect();
        tracing::debug!("Root children: {:?}", root_children);

        // Show prefix info
        if let Some(prefix) = state.prefixes.get(prefix_id) {
            tracing::debug!(
                "Prefix '{}' root_item_id={}",
                prefix_id,
                prefix.root_item_id
            );

            let prefix_children: Vec<_> = state
                .items
                .children_of(prefix.root_item_id)
                .map(|i| format!("{} (id={}, type={:?})", i.name, i.id, i.item_type))
                .collect();
            tracing::debug!("Prefix children: {:?}", prefix_children);

            if let Some(toolchain) = state.items.lookup_child(prefix.root_item_id, "toolchain") {
                let tc_id = toolchain.id;
                let tc_children: Vec<_> = state
                    .items
                    .children_of(tc_id)
                    .map(|i| format!("{} (id={})", i.name, i.id))
                    .collect();
                tracing::debug!("toolchain/ children: {:?}", tc_children);

                if let Some(bin) = state.items.lookup_child(tc_id, "bin") {
                    let bin_children: Vec<_> = state
                        .items
                        .children_of(bin.id)
                        .map(|i| format!("{} (id={})", i.name, i.id))
                        .collect();
                    tracing::debug!("toolchain/bin/ children: {:?}", bin_children);
                }
            }
        } else {
            tracing::debug!("Prefix '{}' NOT FOUND in prefixes map!", prefix_id);
        }
        tracing::debug!("=== END VFS STATE DUMP ===");
    }

    /// Build the path from an item up to (but not including) a root
    pub(crate) fn build_path(&self, items: &VfsItems, item_id: ItemId, root_id: ItemId) -> String {
        let mut components = Vec::new();
        let mut current = item_id;

        while current != root_id && current != 0 {
            if let Some(item) = items.get(&current) {
                components.push(item.name.clone());
                current = item.parent_id;
            } else {
                break;
            }
        }

        components.reverse();
        components.join("/")
    }
}
