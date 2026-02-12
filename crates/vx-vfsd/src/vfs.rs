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

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use camino::Utf8PathBuf;
use peeps::locks::DiagnosticRwLock;
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
    /// For files being written: buffered data
    pub write_buffer: Option<Vec<u8>>,
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
    /// parent_id → { child_name → child_id }
    children: HashMap<ItemId, HashMap<String, ItemId>>,
}

impl VfsItems {
    fn new() -> Self {
        Self {
            items: HashMap::new(),
            children: HashMap::new(),
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

    /// Build a minimal snapshot for flush: no write_buffers, includes children index.
    /// Single pass over items, O(N) where N = total items.
    pub(crate) fn snapshot_for_flush(&self) -> FlushSnapshot {
        let mut mini_items = HashMap::with_capacity(self.items.len());
        let mut children: HashMap<ItemId, Vec<(String, ItemId)>> = HashMap::new();

        for (id, item) in &self.items {
            mini_items.insert(
                *id,
                MiniItem {
                    id: item.id,
                    name: item.name.clone(),
                    item_type: item.item_type,
                    mode: item.mode,
                    cas_hash: item.cas_hash,
                    blob_node: item.blob_node,
                    size: item.size,
                },
            );
            if item.parent_id != 0 {
                children
                    .entry(item.parent_id)
                    .or_default()
                    .push((item.name.clone(), *id));
            }
        }

        // Sort children by name for deterministic DirectoryNode hashing
        for kids in children.values_mut() {
            kids.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        }

        FlushSnapshot {
            items: mini_items,
            children,
        }
    }
}

/// Minimal item for flush snapshots — no write_buffer, no heap-heavy fields.
pub(crate) struct MiniItem {
    pub id: ItemId,
    pub name: String,
    pub item_type: ItemType,
    pub mode: u32,
    pub cas_hash: Option<Blake3Hash>,
    pub blob_node: Option<NodeHash>,
    pub size: u64,
}

/// Snapshot of VFS state for tree-building during flush.
/// Built in a single O(N) pass, no write_buffers included.
pub(crate) struct FlushSnapshot {
    pub items: HashMap<ItemId, MiniItem>,
    /// parent_id → sorted Vec of (child_name, child_id)
    pub children: HashMap<ItemId, Vec<(String, ItemId)>>,
}

/// CAS-backed VFS implementation
pub struct CasVfs {
    /// CAS client for fetching blobs
    pub(crate) cas: Arc<StoreClient<crate::StoreCaller>>,
    /// All items in the VFS with adjacency index
    pub(crate) items: DiagnosticRwLock<VfsItems>,
    /// Active action prefixes (keyed by prefix ID like "build-01JFX...")
    pub(crate) prefixes: DiagnosticRwLock<HashMap<String, ActionPrefix>>,
    /// Next available ItemId
    next_id: AtomicU64,
    /// L2 chunk cache (disk): persistent across vx-vfsd restarts.
    /// Layout: `{cache_dir}/{hex[..2]}/{hex}`.
    /// None if VX_HOME is not set.
    disk_cache_dir: Option<Utf8PathBuf>,
    /// BlobNode metadata cache: NodeHash → parsed BlobNode.
    /// BlobNodes are immutable (content-addressed) so caching is unconditionally safe.
    /// Small: each entry is ~100 bytes × MAX_CHUNKS_PER_BLOB chunk entries.
    blobnode_cache: DiagnosticRwLock<HashMap<NodeHash, Arc<BlobNode>>>,
    /// Mount path for reading back files written directly to disk (ProjFS on Windows).
    mount_path: Utf8PathBuf,
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
            write_buffer: None,
        });

        Self {
            cas,
            items: DiagnosticRwLock::new("CasVfs::items", items),
            prefixes: DiagnosticRwLock::new("CasVfs::prefixes", HashMap::new()),
            next_id: AtomicU64::new(2), // 1=root
            disk_cache_dir,
            blobnode_cache: DiagnosticRwLock::new("CasVfs::blobnode_cache", HashMap::new()),
            mount_path,
        }
    }

    /// Allocate a new ItemId
    fn alloc_id(&self) -> ItemId {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Compute the disk cache path for a chunk.
    /// Layout: `{cache_dir}/{hex[..2]}/{hex}`
    fn chunk_cache_path(&self, hash: &Blake3Hash) -> Option<Utf8PathBuf> {
        let dir = self.disk_cache_dir.as_ref()?;
        let hex = hash.to_hex();
        Some(dir.join(&hex[..2]).join(hex))
    }

    /// Fetch a chunk with L2 disk cache. On miss, fetches from CAS and writes through.
    async fn read_chunk_cached(&self, chunk_hash: ChunkHash) -> Result<Vec<u8>, String> {
        // Try L2 disk cache first
        if let Some(path) = self.chunk_cache_path(&chunk_hash.0) {
            match tokio::fs::read(path.as_std_path()).await {
                Ok(data) => return Ok(data),
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
        let data = self
            .cas
            .get_chunk(chunk_hash)
            .await
            .map_err(|e| format!("CAS chunk read error: {e}"))?
            .ok_or_else(|| format!("chunk {} not found", chunk_hash.to_hex()))?;

        // Write-through to L2
        if let Some(path) = self.chunk_cache_path(&chunk_hash.0)
            && let Err(e) = vx_io::atomic_write(&path, &data).await
        {
            tracing::warn!(hash = %chunk_hash.0, error = %e, "L2 cache write-through failed");
        }

        Ok(data)
    }

    /// Get a cached BlobNode, fetching and parsing from CAS on miss.
    async fn get_blobnode_cached(&self, blob_node_hash: NodeHash) -> Result<Arc<BlobNode>, String> {
        // Check cache
        {
            let cache = self.blobnode_cache.read();
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
            let mut cache = self.blobnode_cache.write();
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

                let mut results: Vec<Option<(Vec<u8>, u64)>> = vec![None; needed_chunks.len()];

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
    /// Check if a prefix already exists
    pub async fn has_prefix(&self, prefix_id: &str) -> bool {
        self.prefixes.read().contains_key(prefix_id)
    }

    /// Create a new action prefix
    ///
    /// Returns the prefix ID (e.g., "build-01JFX...") and the root ItemId
    pub async fn create_prefix(&self, prefix_id: String) -> (String, ItemId) {
        let prefix_root_id = self.alloc_id();

        // Create the prefix directory under root
        {
            let mut items = self.items.write();
            items.insert(VfsItem {
                id: prefix_root_id,
                parent_id: 1, // under root
                name: prefix_id.clone(),
                item_type: ItemType::Directory,
                mode: mode::DIRECTORY,
                cas_hash: None,
                blob_node: None,
                size: 0,
                write_buffer: None,
            });
        }

        // Register the prefix
        {
            let mut prefixes = self.prefixes.write();
            prefixes.insert(
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
        // Get the prefix root
        let prefix_root_id = {
            let prefixes = self.prefixes.read();
            prefixes.get(prefix_id)?.root_item_id
        };

        // Split path into components and create intermediate directories
        let components: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            return None;
        }

        // Single write lock for the entire check+create sequence
        let mut items = self.items.write();
        let mut current_parent = prefix_root_id;

        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;

            // Check if this component already exists (O(1) via children index)
            if let Some(existing) = items.lookup_child(current_parent, component) {
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

            items.insert(VfsItem {
                id: new_id,
                parent_id: current_parent,
                name: component.to_string(),
                item_type,
                mode: item_mode,
                cas_hash,
                blob_node,
                size,
                write_buffer: None,
            });

            if is_last {
                drop(items);
                // Update prefix mappings
                let mut prefixes = self.prefixes.write();
                if let Some(prefix) = prefixes.get_mut(prefix_id) {
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
        let prefix_root_id = {
            let prefixes = self.prefixes.read();
            let Some(prefix) = prefixes.get(prefix_id) else {
                return;
            };
            prefix.root_item_id
        };

        // Collect then remove under one write lock
        {
            let mut items = self.items.write();
            let to_remove = Self::collect_descendants_from(&items, prefix_root_id);
            // Remove in reverse (leaves first) to keep children index clean
            for &id in to_remove.iter().rev() {
                items.remove(id);
            }
        }

        // Remove prefix registration
        {
            let mut prefixes = self.prefixes.write();
            prefixes.remove(prefix_id);
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

        let items = self.items.read();
        if let Some(item) = items.lookup_child(parent_id, &name) {
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

        let items = self.items.read();
        match items.get(&item_id) {
            Some(item) => GetAttributesResult {
                attrs: ItemAttributes {
                    item_id: item.id,
                    item_type: item.item_type,
                    size: item.size,
                    modified_time: 0,
                    created_time: 0,
                    mode: item.mode,
                },
                error: errno::OK,
            },
            None => GetAttributesResult {
                attrs: ItemAttributes {
                    item_id: 0,
                    item_type: ItemType::File,
                    size: 0,
                    modified_time: 0,
                    created_time: 0,
                    mode: 0,
                },
                error: errno::ENOENT,
            },
        }
    }

    async fn read_dir(&self, _cx: &roam::Context, item_id: ItemId, cursor: u64) -> ReadDirResult {
        tracing::trace!(item_id, cursor, "VFS read_dir");

        let items = self.items.read();
        match items.get(&item_id) {
            Some(item) if item.item_type == ItemType::Directory => {
                let children: Vec<_> = items.children_of(item_id).collect();

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

                ReadDirResult {
                    entries,
                    next_cursor,
                    error: errno::OK,
                }
            }
            Some(_) => ReadDirResult {
                entries: Vec::new(),
                next_cursor: 0,
                error: errno::ENOTDIR,
            },
            None => ReadDirResult {
                entries: Vec::new(),
                next_cursor: 0,
                error: errno::ENOENT,
            },
        }
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
        let (cas_hash, blob_node) = {
            let items = self.items.read();
            let Some(item) = items.get(&item_id) else {
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
            if let Some(ref data) = item.write_buffer {
                let start = (offset as usize).min(data.len());
                let end = ((offset + len) as usize).min(data.len());
                return ReadResult {
                    data: data[start..end].to_vec(),
                    error: errno::OK,
                };
            }

            (item.cas_hash, item.blob_node)
        };

        // Fetch from CAS if we have a hash
        let Some(hash) = cas_hash else {
            // Empty file
            return ReadResult {
                data: Vec::new(),
                error: errno::OK,
            };
        };

        // Fetch via chunk-based CAS reads (SmallFile → single chunk, LargeFile → BlobNode)
        match self.read_cas_range(hash, blob_node, offset, len).await {
            Ok(data) => ReadResult {
                data,
                error: errno::OK,
            },
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

        let mut items = self.items.write();
        let Some(item) = items.get_mut(&item_id) else {
            return WriteResult {
                bytes_written: 0,
                error: errno::ENOENT,
            };
        };

        if item.item_type != ItemType::File {
            return WriteResult {
                bytes_written: 0,
                error: errno::EISDIR,
            };
        }

        // Initialize or extend write buffer
        let buffer = item.write_buffer.get_or_insert_with(Vec::new);
        let end = offset as usize + data.len();

        // Extend buffer if needed
        if buffer.len() < end {
            buffer.resize(end, 0);
        }

        // Copy data into buffer
        buffer[offset as usize..end].copy_from_slice(&data);
        item.size = buffer.len() as u64;

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

        let mut items = self.items.write();

        // Verify parent exists and is a directory
        match items.get(&parent_id) {
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
        if items.child_exists(parent_id, &name) {
            return CreateResult {
                item_id: 0,
                error: errno::EEXIST,
            };
        }

        items.insert(VfsItem {
            id: new_id,
            parent_id,
            name,
            item_type,
            mode: item_mode,
            cas_hash: None,
            blob_node: None,
            size: 0,
            write_buffer: None,
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

        let mut items = self.items.write();

        let Some(item) = items.get(&item_id) else {
            return VfsResult {
                error: errno::ENOENT,
            };
        };

        // If directory, check if empty (O(1) via index)
        if item.item_type == ItemType::Directory && items.has_children(item_id) {
            return VfsResult {
                error: errno::ENOTEMPTY,
            };
        }

        items.remove(item_id);
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

        let mut items = self.items.write();

        if !items.contains_key(&item_id) {
            return VfsResult {
                error: errno::ENOENT,
            };
        }

        // Check new parent exists and is directory
        match items.get(&new_parent_id) {
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
        if items.child_exists(new_parent_id, &new_name) {
            // Could be ourselves moving to same parent with same name — check
            if let Some(existing) = items.lookup_child(new_parent_id, &new_name)
                && existing.id != item_id
            {
                return VfsResult {
                    error: errno::EEXIST,
                };
            }
        }

        items.rename(item_id, new_parent_id, new_name);
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

        let mut items = self.items.write();

        let Some(item) = items.get_mut(&item_id) else {
            return VfsResult {
                error: errno::ENOENT,
            };
        };

        if let Some(mode) = params.mode {
            item.mode = mode;
        }

        if let Some(new_size) = params.size {
            let buffer = item.write_buffer.get_or_insert_with(Vec::new);
            buffer.resize(new_size as usize, 0);
            item.size = new_size;
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
            let mut items = self.items.write();
            let item = items.get_mut(&item_id)?;
            item.write_buffer.take()?
        };

        if data.is_empty() {
            return None;
        }

        let size = data.len() as u64;
        let content_hash = Blake3Hash::from_bytes(&data);
        let chunk_hash = ChunkHash(content_hash);
        match self.cas.put_chunk(chunk_hash, data.clone()).await {
            Ok(()) => {
                let mut items = self.items.write();
                if let Some(item) = items.get_mut(&item_id) {
                    item.cas_hash = Some(content_hash);
                    item.blob_node = None; // single chunk
                    item.size = size;
                }
                Some((content_hash, size))
            }
            Err(e) => {
                tracing::error!(item_id, error = %e, "failed to flush to CAS, restoring write buffer");
                // Restore the write buffer so data isn't lost
                let mut items = self.items.write();
                if let Some(item) = items.get_mut(&item_id) {
                    item.size = data.len() as u64;
                    item.write_buffer = Some(data);
                }
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
            let prefixes = self.prefixes.read();
            match prefixes.get(prefix_id) {
                Some(p) => p.root_item_id,
                None => return results,
            }
        };

        let items_to_flush: Vec<(ItemId, String)> = {
            let items = self.items.read();
            let descendants = Self::collect_descendants_from(&items, prefix_root_id);

            descendants
                .into_iter()
                .filter_map(|id| {
                    let item = items.get(&id)?;
                    if item.write_buffer.is_some() {
                        let path = self.build_path(&items, id, prefix_root_id);
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
                match tokio::fs::read(&path).await {
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
        let items = self.items.read();
        let prefixes = self.prefixes.read();

        eprintln!("\n=== VFS STATE DUMP ===");
        eprintln!("Total items: {}", items.len());
        eprintln!("Total prefixes: {}", prefixes.len());

        for (prefix_id, prefix) in prefixes.iter() {
            eprintln!(
                "\nPrefix '{}' root_item_id={}",
                prefix_id, prefix.root_item_id
            );

            let children: Vec<_> = items
                .children_of(prefix.root_item_id)
                .map(|i| format!("{} (type={:?})", i.name, i.item_type))
                .collect();
            eprintln!("  Children: {:?}", children);
        }
    }

    pub async fn dump_state(&self, prefix_id: &str) {
        let items = self.items.read();
        let prefixes = self.prefixes.read();

        tracing::debug!("=== VFS STATE DUMP ===");
        tracing::debug!("Total items: {}", items.len());
        tracing::debug!("Total prefixes: {}", prefixes.len());

        // Show root children
        let root_children: Vec<_> = items
            .children_of(ROOT_ITEM_ID)
            .map(|i| format!("{} (id={})", i.name, i.id))
            .collect();
        tracing::debug!("Root children: {:?}", root_children);

        // Show prefix info
        if let Some(prefix) = prefixes.get(prefix_id) {
            tracing::debug!(
                "Prefix '{}' root_item_id={}",
                prefix_id,
                prefix.root_item_id
            );

            let prefix_children: Vec<_> = items
                .children_of(prefix.root_item_id)
                .map(|i| format!("{} (id={}, type={:?})", i.name, i.id, i.item_type))
                .collect();
            tracing::debug!("Prefix children: {:?}", prefix_children);

            if let Some(toolchain) = items.lookup_child(prefix.root_item_id, "toolchain") {
                let tc_id = toolchain.id;
                let tc_children: Vec<_> = items
                    .children_of(tc_id)
                    .map(|i| format!("{} (id={})", i.name, i.id))
                    .collect();
                tracing::debug!("toolchain/ children: {:?}", tc_children);

                if let Some(bin) = items.lookup_child(tc_id, "bin") {
                    let bin_children: Vec<_> = items
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
