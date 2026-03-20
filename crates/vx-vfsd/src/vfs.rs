//! VFS implementation backed by CAS
//!
//! This module implements the Vixen VFS trait, providing a virtual filesystem
//! that serves content from the Content-Addressed Storage (CAS).
//!
//! # Architecture
//!
//! The VFS is mounted once at a stable global mount base (`/tmp/vx/vfs`)
//! managed by `vx-vfsd`, and serves multiple action prefixes:
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
use moire::sync::SyncMutex;
use vixenfs_proto::{
    CreateResult, DirEntry, GetAttributesResult, ItemAttributes, ItemId, ItemType, LookupResult,
    ReadDirResult, ReadResult, SetAttributesParams, Vfs, VfsDiskNotify, VfsResult, WriteResult,
    errno, mode,
};
use vx_store_proto::{
    Blake3Hash, BlobNode, CasNode, ChunkHash, DirEntryKind, NodeHash, StoreClient,
};

use crate::protocol::{VfsPrefixStateSummary, VfsTrackedObservation};

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
    /// For directories mounted from CAS, the backing subtree root.
    pub dir_node: Option<NodeHash>,
    /// Whether immediate children from `dir_node` have been populated into `items`.
    pub dir_children_loaded: bool,
}

/// An action prefix - represents a build directory like `/build-01JFX.../`
#[derive(Debug)]
pub struct ActionPrefix {
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
    /// For directories mounted from CAS, the backing subtree root.
    pub dir_node: Option<NodeHash>,
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
    /// Directory verifier generations keyed by directory ItemId.
    dir_verifiers: HashMap<ItemId, u64>,
    /// In-memory write buffers for files being written (FUSE only).
    /// Stored separately from VfsItem so items can be cloned cheaply for snapshots.
    write_buffers: HashMap<ItemId, Vec<u8>>,
}

impl VfsItems {
    fn new() -> Self {
        Self {
            items: HashMap::new(),
            children: HashMap::new(),
            dir_verifiers: HashMap::new(),
            write_buffers: HashMap::new(),
        }
    }

    fn bump_dir_verifier(&mut self, dir_id: ItemId) {
        let next = self
            .dir_verifiers
            .get(&dir_id)
            .copied()
            .unwrap_or(1)
            .wrapping_add(1);
        self.dir_verifiers.insert(dir_id, next.max(1));
    }

    fn dir_verifier(&self, dir_id: ItemId) -> u64 {
        self.dir_verifiers.get(&dir_id).copied().unwrap_or(1)
    }

    /// Insert an item, updating the children index.
    fn insert(&mut self, item: VfsItem) {
        let id = item.id;
        let parent_id = item.parent_id;
        let name = item.name.clone();
        let is_dir = item.item_type == ItemType::Directory;
        self.items.insert(id, item);
        if is_dir {
            self.dir_verifiers.entry(id).or_insert(1);
        }
        if parent_id != 0 {
            self.children.entry(parent_id).or_default().insert(name, id);
            self.bump_dir_verifier(parent_id);
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
            self.bump_dir_verifier(item.parent_id);
        }
        // Remove this item's children entry (even if non-empty — caller is
        // responsible for removing descendants first)
        self.children.remove(&id);
        self.dir_verifiers.remove(&id);
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

    /// Check whether `item_id` is within the subtree rooted at `ancestor_id`.
    fn is_within_subtree(&self, item_id: ItemId, ancestor_id: ItemId) -> bool {
        let mut current = item_id;
        loop {
            if current == ancestor_id {
                return true;
            }

            let Some(item) = self.items.get(&current) else {
                return false;
            };
            if item.parent_id == 0 {
                return false;
            }
            current = item.parent_id;
        }
    }

    /// Rename an item: update the children index for old and new parent.
    fn rename(&mut self, item_id: ItemId, new_parent_id: ItemId, new_name: String) {
        let mut old_parent = None;
        if let Some(item) = self.items.get(&item_id) {
            old_parent = Some(item.parent_id);
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
        if let Some(old_parent_id) = old_parent {
            self.bump_dir_verifier(old_parent_id);
            if old_parent_id != new_parent_id {
                self.bump_dir_verifier(new_parent_id);
            }
        }
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
            dir_verifiers: self.dir_verifiers.clone(),
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
    pub(crate) cas: Arc<StoreClient>,
    /// Unified VFS state lock: items + prefixes
    pub(crate) state: SyncMutex<VfsState>,
    /// Next available ItemId
    next_id: AtomicU64,
    /// L2 chunk cache (disk): persistent across vx-vfsd restarts.
    /// Layout: `{cache_dir}/{hex[..2]}/{hex}`.
    /// None if VX_HOME is not set.
    disk_cache_dir: Option<Utf8PathBuf>,
    /// BlobNode metadata cache: NodeHash → parsed BlobNode.
    /// BlobNodes are immutable (content-addressed) so caching is unconditionally safe.
    /// Small: each entry is ~100 bytes × MAX_CHUNKS_PER_BLOB chunk entries.
    blobnode_cache: SyncMutex<HashMap<NodeHash, Arc<BlobNode>>>,
    /// L1 chunk cache (memory): ChunkHash → bytes, bounded by total bytes.
    chunk_l1_cache: SyncMutex<ChunkL1Cache>,
    /// Diagnostic tracker for CAS-backed file identity stability.
    observed_identities: SyncMutex<HashMap<ItemId, ObservedIdentity>>,
    /// Observed directory snapshots from `read_dir`.
    observed_directories: SyncMutex<HashMap<ItemId, ObservedDirectory>>,
    /// Observed missing lookups under CAS-backed directories.
    observed_lookup_misses: SyncMutex<HashMap<LookupMissKey, ObservedLookupMiss>>,
    /// Mount path for reading back files written directly to disk (ProjFS on Windows).
    mount_path: Utf8PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedIdentity {
    cas_hash: Blake3Hash,
    blob_node: Option<NodeHash>,
    size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedDirectory {
    directory_node: NodeHash,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LookupMissKey {
    parent_id: ItemId,
    name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedLookupMiss {
    directory_node: NodeHash,
}

impl CasVfs {
    /// Create a new CAS-backed VFS
    ///
    /// `disk_cache_dir` enables persistent L2 blob cache across restarts.
    /// Pass `None` to disable (L1 memory-only).
    pub fn new(
        cas: Arc<StoreClient>,
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
            dir_node: None,
            dir_children_loaded: true,
        });

        Self {
            cas,
            state: SyncMutex::new(
                "CasVfs::state",
                VfsState {
                    items,
                    prefixes: HashMap::new(),
                },
            ),
            next_id: AtomicU64::new(2), // 1=root
            disk_cache_dir,
            blobnode_cache: SyncMutex::new("CasVfs::blobnode_cache", HashMap::new()),
            chunk_l1_cache: SyncMutex::new(
                "CasVfs::chunk_l1_cache",
                ChunkL1Cache::new(Self::chunk_l1_cache_max_bytes()),
            ),
            observed_identities: SyncMutex::new("CasVfs::observed_identities", HashMap::new()),
            observed_directories: SyncMutex::new("CasVfs::observed_directories", HashMap::new()),
            observed_lookup_misses: SyncMutex::new(
                "CasVfs::observed_lookup_misses",
                HashMap::new(),
            ),
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
            cas: &StoreClient,
            chunk_hash: ChunkHash,
        ) -> Result<Arc<[u8]>, String> {
            let data = cas
                .get_chunk(chunk_hash)
                .await
                .map_err(|e| format!("CAS chunk read error: {:?}", e))?
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
            match moire::fs::read(path.as_std_path()).await {
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
                    if let Err(e) = moire::fs::remove_file(path.as_std_path()).await
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %path, error = ?e, "failed to remove bad L2 cache file");
                    }
                    // Fall through to CAS refetch.
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Cache miss — fall through to RPC
                }
                Err(e) => {
                    tracing::warn!(hash = %chunk_hash.0, error = ?e, "L2 cache read error");
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
            tracing::warn!(hash = %chunk_hash.0, error = ?e, "L2 cache write-through failed");
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
            .map_err(|e| format!("failed to read BlobNode: {:?}", e))?
            .ok_or_else(|| format!("BlobNode {} not found", blob_node_hash.to_hex()))?;

        let node = CasNode::from_bytes(&node_data)
            .map_err(|e| format!("failed to deserialize BlobNode: {:?}", e))?;

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

    fn observe_directory(&self, item_id: ItemId, observation: ObservedDirectory) {
        self.observed_directories
            .lock()
            .insert(item_id, observation);
    }

    fn clear_observed_directory(&self, item_id: ItemId) {
        self.observed_directories.lock().remove(&item_id);
    }

    fn observe_lookup_miss(&self, parent_id: ItemId, name: &str, observation: ObservedLookupMiss) {
        self.observed_lookup_misses.lock().insert(
            LookupMissKey {
                parent_id,
                name: name.to_string(),
            },
            observation,
        );
    }

    fn clear_observed_lookup_miss(&self, parent_id: ItemId, name: &str) {
        self.observed_lookup_misses.lock().remove(&LookupMissKey {
            parent_id,
            name: name.to_string(),
        });
    }

    fn clear_observations_for_item(&self, item_id: ItemId) {
        self.clear_observed_identity(item_id);
        self.clear_observed_directory(item_id);
        self.observed_lookup_misses
            .lock()
            .retain(|key, _| key.parent_id != item_id);
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
                    error = ?e,
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
                dir_node: None,
                dir_children_loaded: true,
            });

            // Register the prefix
            state.prefixes.insert(
                prefix_id.clone(),
                ActionPrefix {
                    root_item_id: prefix_root_id,
                    path_mappings: HashMap::new(),
                },
            );
        }

        tracing::debug!(prefix_id = %prefix_id, item_id = prefix_root_id, "created action prefix");
        (prefix_id, prefix_root_id)
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
                dir_node: None,
            },
        )
        .await
    }

    /// Mount a CAS-backed directory tree at a prefix-relative path.
    pub async fn mount_tree_to_prefix(
        &self,
        prefix_id: &str,
        path: &str,
        dir_node: NodeHash,
    ) -> Option<ItemId> {
        let components: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        let mut state = self.state.lock();
        let prefix_root_id = state.prefixes.get(prefix_id)?.root_item_id;

        if components.is_empty() {
            let root = state.items.get_mut(&prefix_root_id)?;
            root.dir_node = Some(dir_node);
            root.dir_children_loaded = false;
            if let Some(prefix) = state.prefixes.get_mut(prefix_id) {
                prefix.path_mappings.insert(
                    String::new(),
                    PrefixEntry {
                        item_type: ItemType::Directory,
                        cas_hash: None,
                        blob_node: None,
                        size: 0,
                        mode: mode::DIRECTORY,
                        dir_node: Some(dir_node),
                    },
                );
            }
            return Some(prefix_root_id);
        }

        let mut current_parent = prefix_root_id;
        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;
            if let Some(existing) = state.items.lookup_child(current_parent, component) {
                let existing_id = existing.id;
                let existing_type = existing.item_type;
                if is_last {
                    if existing_type != ItemType::Directory {
                        return None;
                    }
                    let item = state.items.get_mut(&existing_id)?;
                    item.dir_node = Some(dir_node);
                    item.dir_children_loaded = false;
                    if let Some(prefix) = state.prefixes.get_mut(prefix_id) {
                        prefix.path_mappings.insert(
                            path.to_string(),
                            PrefixEntry {
                                item_type: ItemType::Directory,
                                cas_hash: None,
                                blob_node: None,
                                size: 0,
                                mode: mode::DIRECTORY,
                                dir_node: Some(dir_node),
                            },
                        );
                    }
                    return Some(existing_id);
                }
                if existing_type != ItemType::Directory {
                    return None;
                }
                current_parent = existing_id;
                continue;
            }

            let new_id = self.alloc_id();
            state.items.insert(VfsItem {
                id: new_id,
                parent_id: current_parent,
                name: component.to_string(),
                item_type: ItemType::Directory,
                mode: mode::DIRECTORY,
                cas_hash: None,
                blob_node: None,
                size: 0,
                dir_node: is_last.then_some(dir_node),
                dir_children_loaded: !is_last,
            });

            if is_last {
                if let Some(prefix) = state.prefixes.get_mut(prefix_id) {
                    prefix.path_mappings.insert(
                        path.to_string(),
                        PrefixEntry {
                            item_type: ItemType::Directory,
                            cas_hash: None,
                            blob_node: None,
                            size: 0,
                            mode: mode::DIRECTORY,
                            dir_node: Some(dir_node),
                        },
                    );
                }
                return Some(new_id);
            }

            current_parent = new_id;
        }

        None
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
            let (item_type, item_mode, cas_hash, blob_node, size, dir_node, dir_children_loaded) =
                if is_last {
                    (
                        entry.item_type,
                        entry.mode,
                        entry.cas_hash,
                        entry.blob_node,
                        entry.size,
                        entry.dir_node,
                        entry.item_type != ItemType::Directory || entry.dir_node.is_none(),
                    )
                } else {
                    (
                        ItemType::Directory,
                        mode::DIRECTORY,
                        None,
                        None,
                        0,
                        None,
                        true,
                    )
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
                dir_node,
                dir_children_loaded,
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
            self.clear_observations_for_item(id);
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

    async fn load_directory_entries(
        &self,
        dir_node: NodeHash,
    ) -> Result<Vec<(String, PrefixEntry)>, String> {
        let node_data = self
            .cas
            .get_node(dir_node)
            .await
            .map_err(|e| format!("failed to fetch DirectoryNode {dir_node}: {e:?}"))?
            .ok_or_else(|| format!("DirectoryNode {dir_node} not found in CAS"))?;
        let node = CasNode::from_bytes(&node_data).map_err(|e| {
            format!(
                "failed to deserialize DirectoryNode {} (len={}): {:?}",
                dir_node,
                node_data.len(),
                e
            )
        })?;
        let dir = match node {
            CasNode::Directory(dir) => dir,
            other => {
                return Err(format!(
                    "expected DirectoryNode for {}, got {other:?}",
                    dir_node
                ));
            }
        };

        let mut entries = Vec::with_capacity(dir.entries.len());
        for entry in dir.entries {
            let prefix_entry = match entry.kind {
                DirEntryKind::SmallFile {
                    content_hash,
                    size,
                    executable,
                } => PrefixEntry {
                    item_type: ItemType::File,
                    cas_hash: Some(content_hash.0),
                    blob_node: None,
                    size,
                    mode: if executable {
                        mode::FILE_EXECUTABLE
                    } else {
                        mode::FILE_REGULAR
                    },
                    dir_node: None,
                },
                DirEntryKind::LargeFile {
                    content_hash,
                    blob_node,
                    size,
                    executable,
                } => PrefixEntry {
                    item_type: ItemType::File,
                    cas_hash: Some(content_hash.0),
                    blob_node: Some(blob_node),
                    size,
                    mode: if executable {
                        mode::FILE_EXECUTABLE
                    } else {
                        mode::FILE_REGULAR
                    },
                    dir_node: None,
                },
                DirEntryKind::Dir { hash, .. } => PrefixEntry {
                    item_type: ItemType::Directory,
                    cas_hash: None,
                    blob_node: None,
                    size: 0,
                    mode: mode::DIRECTORY,
                    dir_node: Some(hash),
                },
                DirEntryKind::Symlink { .. } => {
                    tracing::warn!(name = %entry.name, "skipping symlink (not yet supported)");
                    continue;
                }
            };
            entries.push((entry.name, prefix_entry));
        }

        Ok(entries)
    }

    async fn ensure_dir_children_loaded(&self, item_id: ItemId) -> Result<(), String> {
        let dir_node = {
            let state = self.state.lock();
            let Some(item) = state.items.get(&item_id) else {
                return Ok(());
            };
            if item.item_type != ItemType::Directory || item.dir_children_loaded {
                return Ok(());
            }
            item.dir_node
        };

        let Some(dir_node) = dir_node else {
            let mut state = self.state.lock();
            if let Some(item) = state.items.get_mut(&item_id) {
                item.dir_children_loaded = true;
            }
            return Ok(());
        };

        let children = self.load_directory_entries(dir_node).await?;

        let mut state = self.state.lock();
        let (parent_id, parent_dir_node, already_loaded) = match state.items.get(&item_id) {
            Some(item) => (item.id, item.dir_node, item.dir_children_loaded),
            None => return Ok(()),
        };
        if already_loaded || parent_dir_node != Some(dir_node) {
            return Ok(());
        }

        for (name, entry) in children {
            if state.items.lookup_child(parent_id, &name).is_some() {
                continue;
            }
            let child_id = self.alloc_id();
            state.items.insert(VfsItem {
                id: child_id,
                parent_id,
                name,
                item_type: entry.item_type,
                mode: entry.mode,
                cas_hash: entry.cas_hash,
                blob_node: entry.blob_node,
                size: entry.size,
                dir_node: entry.dir_node,
                dir_children_loaded: entry.item_type != ItemType::Directory
                    || entry.dir_node.is_none(),
            });
        }

        if let Some(item) = state.items.get_mut(&item_id) {
            item.dir_children_loaded = true;
        }

        Ok(())
    }
}

impl CasVfs {
    pub(crate) async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult {
        tracing::trace!(parent_id, name = %name, "VFS lookup");

        {
            let state = self.state.lock();
            if let Some(item) = state.items.lookup_child(parent_id, &name) {
                self.clear_observed_lookup_miss(parent_id, &name);
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
        }

        if let Err(err) = self.ensure_dir_children_loaded(parent_id).await {
            tracing::error!(parent_id, name = %name, error = %err, "failed to populate mounted directory");
            return LookupResult {
                attrs: ItemAttributes {
                    item_id: 0,
                    item_type: ItemType::File,
                    size: 0,
                    modified_time: 0,
                    created_time: 0,
                    mode: 0,
                },
                error: errno::EIO,
            };
        }

        let state = self.state.lock();
        if let Some(item) = state.items.lookup_child(parent_id, &name) {
            self.clear_observed_lookup_miss(parent_id, &name);
            tracing::trace!(parent_id, name = %name, item_id = item.id, item_type = ?item.item_type, "VFS lookup -> found after populate");
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

        if let Some(directory_node) = state
            .items
            .get(&parent_id)
            .filter(|item| item.item_type == ItemType::Directory)
            .and_then(|item| item.dir_node)
        {
            self.observe_lookup_miss(parent_id, &name, ObservedLookupMiss { directory_node });
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

    pub(crate) async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult {
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

    pub(crate) async fn read_dir(
        &self,
        item_id: ItemId,
        cookie: u64,
        verifier: u64,
    ) -> ReadDirResult {
        let started = Instant::now();
        tracing::trace!(item_id, cookie, verifier, "VFS read_dir.start");

        if let Err(err) = self.ensure_dir_children_loaded(item_id).await {
            tracing::error!(item_id, error = %err, "failed to populate mounted directory for read_dir");
            return ReadDirResult {
                entries: Vec::new(),
                next_cookie: 0,
                verifier: 0,
                error: errno::EIO,
            };
        }

        let state = self.state.lock();
        let (result, child_count) = match state.items.get(&item_id) {
            Some(item) if item.item_type == ItemType::Directory => {
                let current_verifier = state.items.dir_verifier(item_id);
                if cookie != 0 && verifier != current_verifier {
                    (
                        ReadDirResult {
                            entries: Vec::new(),
                            next_cookie: 0,
                            verifier: current_verifier,
                            error: errno::ESTALE,
                        },
                        Some(0),
                    )
                } else {
                    let children: Vec<_> = state.items.children_of(item_id).collect();

                    let entries: Vec<DirEntry> = children
                        .iter()
                        .skip(cookie as usize)
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

                    let has_more = children.len() > (cookie as usize + entries.len());
                    let next_cookie = if has_more {
                        cookie + entries.len() as u64
                    } else {
                        0
                    };

                    (
                        ReadDirResult {
                            entries,
                            next_cookie,
                            verifier: current_verifier,
                            error: errno::OK,
                        },
                        Some(children.len()),
                    )
                }
            }
            Some(_) => (
                ReadDirResult {
                    entries: Vec::new(),
                    next_cookie: 0,
                    verifier: 0,
                    error: errno::ENOTDIR,
                },
                None,
            ),
            None => (
                ReadDirResult {
                    entries: Vec::new(),
                    next_cookie: 0,
                    verifier: 0,
                    error: errno::ENOENT,
                },
                None,
            ),
        };
        drop(state);

        if result.error == errno::OK
            && let Some(directory_node) = self
                .state
                .lock()
                .items
                .get(&item_id)
                .filter(|item| item.item_type == ItemType::Directory)
                .and_then(|item| item.dir_node)
        {
            self.observe_directory(item_id, ObservedDirectory { directory_node });
        }

        let elapsed = started.elapsed();
        tracing::trace!(
            item_id,
            cookie,
            verifier,
            child_count = ?child_count,
            entries = result.entries.len(),
            next_cookie = result.next_cookie,
            result_verifier = result.verifier,
            error = result.error,
            elapsed_us = elapsed.as_micros(),
            "VFS read_dir.done"
        );
        if elapsed >= Duration::from_millis(500) {
            tracing::warn!(
                item_id,
                cookie,
                verifier,
                child_count = ?child_count,
                entries = result.entries.len(),
                next_cookie = result.next_cookie,
                result_verifier = result.verifier,
                error = result.error,
                elapsed_ms = elapsed.as_millis(),
                "VFS read_dir.slow"
            );
        }

        result
    }

    pub(crate) async fn read(&self, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
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
                tracing::error!(hash = %hash, error = ?e, "failed to read blob range");
                ReadResult {
                    data: Vec::new(),
                    error: errno::EIO,
                }
            }
        }
    }

    pub(crate) async fn write(&self, item_id: ItemId, offset: u64, data: Vec<u8>) -> WriteResult {
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
        self.clear_observations_for_item(item_id);

        WriteResult {
            bytes_written: data.len() as u64,
            error: errno::OK,
        }
    }

    pub(crate) async fn create(
        &self,
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

        let dir_children_loaded = item_type != ItemType::Directory;
        state.items.insert(VfsItem {
            id: new_id,
            parent_id,
            name,
            item_type,
            mode: item_mode,
            cas_hash: None,
            blob_node: None,
            size: 0,
            dir_node: None,
            dir_children_loaded,
        });
        drop(state);
        self.clear_observations_for_item(parent_id);

        CreateResult {
            item_id: new_id,
            error: errno::OK,
        }
    }

    pub(crate) async fn delete(&self, item_id: ItemId) -> VfsResult {
        tracing::trace!(item_id, "delete");

        if item_id == ROOT_ITEM_ID {
            return VfsResult {
                error: errno::EACCES,
            };
        }

        let mut state = self.state.lock();

        let Some(item) = state.items.get(&item_id).cloned() else {
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
        self.clear_observations_for_item(item_id);
        self.clear_observations_for_item(item.parent_id);
        VfsResult { error: errno::OK }
    }

    pub(crate) async fn rename(
        &self,
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

        let Some(source_item) = state.items.get(&item_id).cloned() else {
            return VfsResult {
                error: errno::ENOENT,
            };
        };

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

        if source_item.item_type == ItemType::Directory
            && state.items.is_within_subtree(new_parent_id, item_id)
        {
            return VfsResult {
                error: errno::EINVAL,
            };
        }

        let mut replaced_item_id = None;

        if let Some(existing) = state.items.lookup_child(new_parent_id, &new_name).cloned()
            && existing.id != item_id
        {
            match (source_item.item_type, existing.item_type) {
                (ItemType::Directory, ItemType::File | ItemType::Symlink) => {
                    return VfsResult {
                        error: errno::ENOTDIR,
                    };
                }
                (ItemType::File | ItemType::Symlink, ItemType::Directory) => {
                    return VfsResult {
                        error: errno::EISDIR,
                    };
                }
                (ItemType::Directory, ItemType::Directory)
                    if state.items.has_children(existing.id) =>
                {
                    return VfsResult {
                        error: errno::ENOTEMPTY,
                    };
                }
                _ => {}
            }

            state.items.remove(existing.id);
            replaced_item_id = Some(existing.id);
        }

        state.items.rename(item_id, new_parent_id, new_name);
        drop(state);

        if let Some(replaced_item_id) = replaced_item_id {
            self.clear_observations_for_item(replaced_item_id);
        }
        self.clear_observations_for_item(item_id);
        self.clear_observations_for_item(source_item.parent_id);
        self.clear_observations_for_item(new_parent_id);
        VfsResult { error: errno::OK }
    }

    pub(crate) async fn set_attributes(
        &self,
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
            self.clear_observations_for_item(item_id);
            return VfsResult { error: errno::OK };
        }

        // modified_time is ignored for now (we don't track times)

        VfsResult { error: errno::OK }
    }

    pub(crate) async fn ping(&self) -> String {
        "pong from vx-runner CAS VFS".to_string()
    }
}

impl Vfs for CasVfs {
    async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult {
        CasVfs::lookup(self, parent_id, name).await
    }

    async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult {
        CasVfs::get_attributes(self, item_id).await
    }

    async fn read_dir(&self, item_id: ItemId, cookie: u64, verifier: u64) -> ReadDirResult {
        CasVfs::read_dir(self, item_id, cookie, verifier).await
    }

    async fn read(&self, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        CasVfs::read(self, item_id, offset, len).await
    }

    async fn write(&self, item_id: ItemId, offset: u64, data: Vec<u8>) -> WriteResult {
        CasVfs::write(self, item_id, offset, data).await
    }

    async fn create(&self, parent_id: ItemId, name: String, item_type: ItemType) -> CreateResult {
        CasVfs::create(self, parent_id, name, item_type).await
    }

    async fn delete(&self, item_id: ItemId) -> VfsResult {
        CasVfs::delete(self, item_id).await
    }

    async fn rename(&self, item_id: ItemId, new_parent_id: ItemId, new_name: String) -> VfsResult {
        CasVfs::rename(self, item_id, new_parent_id, new_name).await
    }

    async fn set_attributes(&self, item_id: ItemId, params: SetAttributesParams) -> VfsResult {
        CasVfs::set_attributes(self, item_id, params).await
    }

    async fn ping(&self) -> String {
        CasVfs::ping(self).await
    }
}

impl VfsDiskNotify for CasVfs {
    fn on_disk_file_created(&self, path: &str, is_directory: bool) {
        // Path is relative to mount root, e.g. "prefix-id/out/main.o" or
        // "prefix-id\\out\\main.o" on Windows. Normalize separators.
        let path = path.replace('\\', "/");
        let mut components = path.split('/');
        let Some(prefix_id) = components.next() else {
            return;
        };

        let mut state = self.state.lock();
        let parent_id = match state.prefixes.get(prefix_id) {
            Some(p) => p.root_item_id,
            None => {
                tracing::debug!(%prefix_id, "on_disk_file_created: unknown prefix, ignoring");
                return;
            }
        };

        let remaining: Vec<&str> = components.collect();
        if remaining.is_empty() {
            return; // notification for the prefix root itself
        }

        // Walk/create intermediate directories, then create the leaf.
        let (dir_parts, leaf) = remaining.split_at(remaining.len() - 1);
        let mut current_parent = parent_id;

        for &dir_name in dir_parts {
            if let Some(child) = state.items.lookup_child(current_parent, dir_name) {
                current_parent = child.id;
            } else {
                let dir_id = self.alloc_id();
                state.items.insert(VfsItem {
                    id: dir_id,
                    parent_id: current_parent,
                    name: dir_name.to_string(),
                    item_type: ItemType::Directory,
                    mode: 0o755,
                    cas_hash: None,
                    blob_node: None,
                    size: 0,
                    dir_node: None,
                    dir_children_loaded: true,
                });
                tracing::debug!(
                    %prefix_id,
                    dir_name,
                    dir_id,
                    "on_disk_file_created: registered directory"
                );
                current_parent = dir_id;
            }
        }

        let leaf_name = leaf[0];
        if state
            .items
            .lookup_child(current_parent, leaf_name)
            .is_some()
        {
            // Item already tracked (e.g. modified, not new). Nothing to do —
            // flush_prefix_from_disk will re-read the content from disk.
            return;
        }

        let item_type = if is_directory {
            ItemType::Directory
        } else {
            ItemType::File
        };
        let dir_children_loaded = item_type != ItemType::Directory;
        let item_id = self.alloc_id();
        state.items.insert(VfsItem {
            id: item_id,
            parent_id: current_parent,
            name: leaf_name.to_string(),
            item_type,
            mode: if is_directory { 0o755 } else { 0o644 },
            cas_hash: None,
            blob_node: None,
            size: 0,
            dir_node: None,
            dir_children_loaded,
        });
        tracing::debug!(
            %prefix_id,
            path = %path,
            item_id,
            ?item_type,
            "on_disk_file_created: registered disk file"
        );
    }

    fn on_disk_file_deleted(&self, path: &str) {
        let path = path.replace('\\', "/");
        let mut components = path.split('/');
        let Some(prefix_id) = components.next() else {
            return;
        };

        let mut state = self.state.lock();
        let parent_id = match state.prefixes.get(prefix_id) {
            Some(p) => p.root_item_id,
            None => return,
        };

        // Walk to the item.
        let mut current_parent = parent_id;
        let remaining: Vec<&str> = components.collect();
        if remaining.is_empty() {
            return;
        }

        let (dir_parts, leaf) = remaining.split_at(remaining.len() - 1);
        for &dir_name in dir_parts {
            match state.items.lookup_child(current_parent, dir_name) {
                Some(child) => current_parent = child.id,
                None => return, // parent doesn't exist, nothing to delete
            }
        }

        if let Some(child) = state.items.lookup_child(current_parent, leaf[0]) {
            let child_id = child.id;
            state.items.remove(child_id);
            tracing::debug!(
                %prefix_id,
                path = %path,
                child_id,
                "on_disk_file_deleted: removed item"
            );
        }
    }

    fn on_disk_file_renamed(&self, source: &str, destination: &str) {
        // For simplicity: delete source, create destination.
        // This avoids complex reparenting logic for a rare operation.
        self.on_disk_file_deleted(source);
        self.on_disk_file_created(destination, false);
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
            Ok(_) => {
                let mut state = self.state.lock();
                if let Some(item) = state.items.get_mut(&item_id) {
                    item.cas_hash = Some(content_hash);
                    item.blob_node = None; // single chunk
                    item.size = size;
                }
                drop(state);
                self.clear_observations_for_item(item_id);
                Some((content_hash, size))
            }
            Err(e) => {
                tracing::error!(item_id, error = ?e, "failed to flush to CAS, restoring write buffer");
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

        let files = {
            let mut files = Vec::new();
            let mut stack: Vec<(std::path::PathBuf, String)> = Vec::new();

            let mut root_entries = match tokio::fs::read_dir(prefix_dir.as_std_path()).await {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::error!(%prefix_dir, error = ?e, "failed to read prefix dir from disk");
                    return results;
                }
            };

            loop {
                match root_entries.next_entry().await {
                    Ok(Some(entry)) => stack.push((entry.path(), String::new())),
                    Ok(None) => break,
                    Err(e) => {
                        tracing::error!(%prefix_dir, error = ?e, "failed to iterate prefix dir");
                        break;
                    }
                }
            }

            while let Some((path, rel_parent)) = stack.pop() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let rel_path = if rel_parent.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", rel_parent, name)
                };

                match tokio::fs::symlink_metadata(&path).await {
                    Ok(metadata) if metadata.is_dir() => match tokio::fs::read_dir(&path).await {
                        Ok(mut entries) => loop {
                            match entries.next_entry().await {
                                Ok(Some(entry)) => stack.push((entry.path(), rel_path.clone())),
                                Ok(None) => break,
                                Err(e) => {
                                    tracing::warn!(
                                        path = %path.display(),
                                        error = ?e,
                                        "failed to iterate directory while flushing prefix"
                                    );
                                    break;
                                }
                            }
                        },
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = ?e,
                                "failed to read directory while flushing prefix"
                            );
                        }
                    },
                    Ok(metadata) if metadata.is_file() => files.push((path, rel_path)),
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = ?e,
                            "failed to stat path while flushing prefix"
                        );
                    }
                }
            }

            files
        };

        for (path, rel_path) in files {
            match moire::fs::read(&path).await {
                Ok(data) => {
                    let size = data.len() as u64;
                    let content_hash = Blake3Hash::from_bytes(&data);
                    let chunk_hash = ChunkHash(content_hash);
                    match self.cas.put_chunk(chunk_hash, data).await {
                        Ok(_) => {
                            tracing::debug!(
                                path = %rel_path,
                                size,
                                hash = %content_hash,
                                "flushed file from disk to CAS"
                            );
                            results.insert(rel_path, (content_hash, size));
                        }
                        Err(e) => {
                            tracing::error!(path = %rel_path, error = ?e, "failed to put chunk");
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(path = %rel_path, error = ?e, "failed to read file from disk");
                }
            }
        }

        // Update CAS hash and size on items already tracked via ProjFS
        // notifications (VfsDiskNotify). Items should already exist in
        // state.items — notifications register them when the compiler
        // creates/modifies files on disk.
        if !results.is_empty() {
            let mut state = self.state.lock();
            let prefix_root_id = match state.prefixes.get(prefix_id) {
                Some(p) => p.root_item_id,
                None => return results,
            };

            for (rel_path, &(content_hash, size)) in &results {
                let components: Vec<&str> = rel_path.split('/').collect();
                let (dir_components, file_name) = components.split_at(components.len() - 1);

                let mut parent_id = prefix_root_id;
                let mut found = true;
                for &dir_name in dir_components {
                    match state.items.lookup_child(parent_id, dir_name) {
                        Some(child) => parent_id = child.id,
                        None => {
                            tracing::warn!(
                                %prefix_id,
                                path = %rel_path,
                                missing_dir = dir_name,
                                "flush: directory not tracked by notifications"
                            );
                            found = false;
                            break;
                        }
                    }
                }

                if !found {
                    continue;
                }

                let file_name = file_name[0];
                match state.items.lookup_child(parent_id, file_name).map(|i| i.id) {
                    Some(id) => {
                        if let Some(item) = state.items.get_mut(&id) {
                            item.cas_hash = Some(content_hash);
                            item.size = size;
                        }
                    }
                    None => {
                        tracing::warn!(
                            %prefix_id,
                            path = %rel_path,
                            "flush: file not tracked by notifications"
                        );
                    }
                }
            }
        }

        results
    }

    pub async fn dump_state(&self, prefix_id: &str) {
        let summary = self.describe_prefix_state(prefix_id).await;

        tracing::info!("=== VFS STATE DUMP ===");
        tracing::info!("Total items: {}", summary.total_items);
        tracing::info!("Total prefixes: {}", summary.total_prefixes);
        tracing::info!("Root children: {:?}", summary.root_children);

        if summary.prefix_found {
            tracing::info!("Prefix '{}': found", summary.prefix_id);
            tracing::info!("Prefix children: {:?}", summary.prefix_children);
            tracing::info!("src/ children: {:?}", summary.src_children);
            tracing::info!("toolchain/ children: {:?}", summary.toolchain_children);
            tracing::info!(
                "toolchain/bin/ children: {:?}",
                summary.toolchain_bin_children
            );
            tracing::info!("path mappings (sample): {:?}", summary.path_mappings_sample);
            tracing::info!(
                "src path mappings (sample): {:?}",
                summary.src_path_mappings_sample
            );
        } else {
            tracing::info!("Prefix '{}' NOT FOUND in prefixes map!", summary.prefix_id);
        }
        tracing::info!("=== END VFS STATE DUMP ===");
    }

    fn sorted_prefix_entries(
        state: &VfsState,
        parent_id: ItemId,
        include_type: bool,
    ) -> Vec<String> {
        state
            .items
            .children_of(parent_id)
            .map(|item| {
                if include_type {
                    format!("{} (id={}, type={:?})", item.name, item.id, item.item_type)
                } else {
                    format!("{} (id={})", item.name, item.id)
                }
            })
            .collect()
    }

    fn sorted_path_mappings(paths: impl Iterator<Item = String>) -> Vec<String> {
        let mut paths: Vec<_> = paths.collect();
        paths.sort();
        paths
    }

    fn path_under_root(
        &self,
        items: &VfsItems,
        item_id: ItemId,
        root_id: ItemId,
    ) -> Option<String> {
        let mut components = Vec::new();
        let mut current = item_id;

        while current != root_id {
            let item = items.get(&current)?;
            if current == ROOT_ITEM_ID || item.parent_id == 0 {
                return None;
            }
            components.push(item.name.clone());
            current = item.parent_id;
        }

        components.reverse();
        Some(components.join("/"))
    }

    pub async fn describe_prefix_state(&self, prefix_id: &str) -> VfsPrefixStateSummary {
        let state = self.state.lock();
        let root_children = Self::sorted_prefix_entries(&state, ROOT_ITEM_ID, false);

        let Some(prefix) = state.prefixes.get(prefix_id) else {
            return VfsPrefixStateSummary {
                prefix_id: prefix_id.to_string(),
                prefix_found: false,
                total_items: state.items.len() as u64,
                total_prefixes: state.prefixes.len() as u64,
                path_mappings_count: 0,
                root_children,
                prefix_children: Vec::new(),
                src_children: Vec::new(),
                toolchain_children: Vec::new(),
                toolchain_bin_children: Vec::new(),
                path_mappings_sample: Vec::new(),
                src_path_mappings_sample: Vec::new(),
            };
        };

        let prefix_children = Self::sorted_prefix_entries(&state, prefix.root_item_id, true);
        let src_children = state
            .items
            .lookup_child(prefix.root_item_id, "src")
            .map(|src| Self::sorted_prefix_entries(&state, src.id, true))
            .unwrap_or_default();
        let toolchain_children = state
            .items
            .lookup_child(prefix.root_item_id, "toolchain")
            .map(|toolchain| Self::sorted_prefix_entries(&state, toolchain.id, false))
            .unwrap_or_default();
        let toolchain_bin_children = state
            .items
            .lookup_child(prefix.root_item_id, "toolchain")
            .and_then(|toolchain| state.items.lookup_child(toolchain.id, "bin"))
            .map(|bin| Self::sorted_prefix_entries(&state, bin.id, false))
            .unwrap_or_default();
        let path_mappings_sample = Self::sorted_path_mappings(prefix.path_mappings.keys().cloned())
            .into_iter()
            .take(16)
            .collect();
        let src_path_mappings_sample = Self::sorted_path_mappings(
            prefix
                .path_mappings
                .keys()
                .filter(|path| path == &"src" || path.starts_with("src/"))
                .cloned(),
        )
        .into_iter()
        .take(32)
        .collect();

        VfsPrefixStateSummary {
            prefix_id: prefix_id.to_string(),
            prefix_found: true,
            total_items: state.items.len() as u64,
            total_prefixes: state.prefixes.len() as u64,
            path_mappings_count: prefix.path_mappings.len() as u64,
            root_children,
            prefix_children,
            src_children,
            toolchain_children,
            toolchain_bin_children,
            path_mappings_sample,
            src_path_mappings_sample,
        }
    }

    pub async fn tracked_observations(
        &self,
        prefix_id: &str,
        roots: &[String],
    ) -> Vec<VfsTrackedObservation> {
        let state = self.state.lock();
        let Some(prefix) = state.prefixes.get(prefix_id) else {
            return Vec::new();
        };
        let observed_files = self.observed_identities.lock();
        let observed_directories = self.observed_directories.lock();
        let observed_lookup_misses = self.observed_lookup_misses.lock();

        let includes_path = |path: &str| {
            roots.iter().any(|root| {
                root.is_empty()
                    || path == root
                    || path
                        .strip_prefix(root.as_str())
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
        };

        let mut observations = observed_files
            .iter()
            .filter_map(|(item_id, identity)| {
                let path = self.path_under_root(&state.items, *item_id, prefix.root_item_id)?;
                if !includes_path(&path) {
                    return None;
                }
                Some(VfsTrackedObservation::File {
                    path,
                    content_hash: identity.cas_hash,
                    blob_node: identity.blob_node,
                    size: identity.size,
                })
            })
            .collect::<Vec<_>>();

        observations.extend(
            observed_directories
                .iter()
                .filter_map(|(item_id, directory)| {
                    let path = self.path_under_root(&state.items, *item_id, prefix.root_item_id)?;
                    if !includes_path(&path) {
                        return None;
                    }
                    Some(VfsTrackedObservation::Directory {
                        path,
                        directory_node: directory.directory_node,
                    })
                }),
        );

        observations.extend(observed_lookup_misses.iter().filter_map(|(key, miss)| {
            let parent_path =
                self.path_under_root(&state.items, key.parent_id, prefix.root_item_id)?;
            let path = if parent_path.is_empty() {
                key.name.clone()
            } else {
                format!("{parent_path}/{}", key.name)
            };
            if !includes_path(&path) {
                return None;
            }
            Some(VfsTrackedObservation::LookupMiss {
                path,
                parent_path,
                directory_node: miss.directory_node,
            })
        }));

        observations.sort_by_key(|observation| match observation {
            VfsTrackedObservation::File { path, .. } => (path.clone(), 0u8),
            VfsTrackedObservation::Directory { path, .. } => (path.clone(), 1u8),
            VfsTrackedObservation::LookupMiss { path, .. } => (path.clone(), 2u8),
        });
        observations
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
