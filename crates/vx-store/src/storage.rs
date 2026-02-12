//! Storage abstraction for CAS chunk and node operations.
//!
//! Two object types:
//! - **Chunks**: Raw bytes (≤4MB), stored at `chunks/blake3/{xx}/{hash}`
//! - **Nodes**: Versioned postcard-encoded structures, stored at `nodes/blake3/{xx}/{hash}`
//!
//! Plus ancillary storage for cache mappings, toolchain specs, and registry specs.
//!
//! `FsStorage` is the single filesystem backend. `CasStore` wraps a primary
//! `FsStorage` with an optional read-only base for layered access.

use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use vx_io::atomic_write;
use vx_store_proto::{
    Blake3Hash, CacheKey, ChunkHash, ManifestHash, NodeHash, RegistrySpecKey, ToolchainSpecKey,
};

// =============================================================================
// FsStorage — single-directory storage backend
// =============================================================================

pub(crate) struct FsStorage {
    root: Utf8PathBuf,
}

impl FsStorage {
    pub(crate) fn new(root: Utf8PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn root(&self) -> &Utf8Path {
        &self.root
    }

    // -- Chunk paths --

    fn chunks_dir(&self) -> Utf8PathBuf {
        self.root.join("chunks/blake3")
    }

    fn chunk_path_for(&self, hash: &ChunkHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        self.chunks_dir().join(&hex[..2]).join(&hex)
    }

    // -- Node paths --

    fn nodes_dir(&self) -> Utf8PathBuf {
        self.root.join("nodes/blake3")
    }

    fn node_path_for(&self, hash: &NodeHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        self.nodes_dir().join(&hex[..2]).join(&hex)
    }

    // -- Chunk operations --

    pub(crate) async fn get_chunk(&self, hash: &ChunkHash) -> io::Result<Option<Vec<u8>>> {
        let path = self.chunk_path_for(hash);
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn put_chunk(&self, hash: &ChunkHash, data: &[u8]) -> io::Result<()> {
        atomic_write(&self.chunk_path_for(hash), data).await
    }

    pub(crate) async fn has_chunk(&self, hash: &ChunkHash) -> io::Result<bool> {
        let path = self.chunk_path_for(hash);
        match tokio::fs::try_exists(&path).await {
            Ok(exists) => Ok(exists),
            Err(e) => Err(e),
        }
    }

    // -- Node operations --

    pub(crate) async fn get_node(&self, hash: &NodeHash) -> io::Result<Option<Vec<u8>>> {
        let path = self.node_path_for(hash);
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn put_node(&self, hash: &NodeHash, data: &[u8]) -> io::Result<()> {
        atomic_write(&self.node_path_for(hash), data).await
    }

    pub(crate) async fn has_node(&self, hash: &NodeHash) -> io::Result<bool> {
        let path = self.node_path_for(hash);
        match tokio::fs::try_exists(&path).await {
            Ok(exists) => Ok(exists),
            Err(e) => Err(e),
        }
    }
}

// =============================================================================
// CasStore — concrete storage with layered access for everything
// =============================================================================

/// Complete CAS storage: chunks and nodes, plus ancillary mappings (cache,
/// toolchain specs, registry specs).
pub(crate) struct CasStore {
    primary: FsStorage,
}

impl CasStore {
    pub(crate) fn new(primary: FsStorage) -> Self {
        Self { primary }
    }

    /// Create all required subdirectories.
    pub(crate) async fn init(&self) -> io::Result<()> {
        let root = self.primary.root();
        tokio::fs::create_dir_all(root.join("chunks/blake3")).await?;
        tokio::fs::create_dir_all(root.join("nodes/blake3")).await?;
        tokio::fs::create_dir_all(root.join("cache/blake3")).await?;
        tokio::fs::create_dir_all(root.join("tmp")).await?;
        tokio::fs::create_dir_all(root.join("toolchains")).await?;
        tokio::fs::create_dir_all(root.join("registry/spec")).await?;
        Ok(())
    }

    pub(crate) fn tmp_dir(&self) -> Utf8PathBuf {
        self.primary.root().join("tmp")
    }

    // -- Chunk operations --

    pub(crate) async fn get_chunk(&self, hash: &ChunkHash) -> io::Result<Option<Vec<u8>>> {
        self.primary.get_chunk(hash).await
    }

    pub(crate) async fn put_chunk(&self, hash: &ChunkHash, data: &[u8]) -> io::Result<()> {
        self.primary.put_chunk(hash, data).await
    }

    pub(crate) async fn has_chunk(&self, hash: &ChunkHash) -> io::Result<bool> {
        self.primary.has_chunk(hash).await
    }

    // -- Node operations --

    pub(crate) async fn get_node(&self, hash: &NodeHash) -> io::Result<Option<Vec<u8>>> {
        self.primary.get_node(hash).await
    }

    pub(crate) async fn put_node(&self, hash: &NodeHash, data: &[u8]) -> io::Result<()> {
        self.primary.put_node(hash, data).await
    }

    pub(crate) async fn has_node(&self, hash: &NodeHash) -> io::Result<bool> {
        self.primary.has_node(hash).await
    }

    // -- Tree manifests (stored in trees/blake3/) --

    fn tree_manifest_path(root: &Utf8Path, hash: &ManifestHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        root.join("trees/blake3")
            .join(&hex[..2])
            .join(format!("{}.json", hex))
    }

    pub(crate) async fn get_tree_manifest(
        &self,
        hash: &ManifestHash,
    ) -> io::Result<Option<String>> {
        let path = Self::tree_manifest_path(self.primary.root(), hash);
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => Ok(Some(json)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    // -- Toolchain specs (spec_key → manifest_hash mapping) --

    fn toolchain_spec_path(root: &Utf8Path, key: &ToolchainSpecKey) -> Utf8PathBuf {
        let hex = key.to_hex();
        root.join("toolchains").join(&hex[..2]).join(&hex)
    }

    pub(crate) async fn get_toolchain_spec(
        &self,
        key: &ToolchainSpecKey,
    ) -> io::Result<Option<Blake3Hash>> {
        let path = Self::toolchain_spec_path(self.primary.root(), key);
        read_hash_mapping(&path).await
    }

    pub(crate) async fn put_toolchain_spec(
        &self,
        key: &ToolchainSpecKey,
        manifest_hash: &Blake3Hash,
    ) -> io::Result<bool> {
        let path = Self::toolchain_spec_path(self.primary.root(), key);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(false);
        }
        match atomic_write(&path, manifest_hash.to_hex().as_bytes()).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    // -- Registry specs (spec_key → manifest_hash mapping) --

    fn registry_spec_path(root: &Utf8Path, key: &RegistrySpecKey) -> Utf8PathBuf {
        let hex = key.to_hex();
        root.join("registry/spec").join(&hex[..2]).join(&hex)
    }

    pub(crate) async fn get_registry_spec(
        &self,
        key: &RegistrySpecKey,
    ) -> io::Result<Option<Blake3Hash>> {
        let path = Self::registry_spec_path(self.primary.root(), key);
        read_hash_mapping(&path).await
    }

    pub(crate) async fn put_registry_spec(
        &self,
        key: &RegistrySpecKey,
        manifest_hash: &Blake3Hash,
    ) -> io::Result<bool> {
        let path = Self::registry_spec_path(self.primary.root(), key);
        let parent = path
            .parent()
            .expect("registry spec path has parent")
            .to_path_buf();
        let manifest_hex = manifest_hash.to_hex();

        tokio::task::spawn_blocking(move || {
            use std::fs::{File, OpenOptions};
            use std::io::Write;

            std::fs::create_dir_all(&parent)?;

            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(manifest_hex.as_bytes())?;
                    file.sync_all()?;
                    if let Ok(dir) = File::open(&parent)
                        && let Err(e) = dir.sync_all()
                    {
                        tracing::warn!("failed to sync directory {}: {}", parent, e);
                    }
                    Ok(true)
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        })
        .await
        .expect("spawn_blocking join failed")
    }

    // -- Cache (cache_key → node_hash mapping) --

    fn cache_path(root: &Utf8Path, key: &CacheKey) -> Utf8PathBuf {
        let hex = key.to_hex();
        root.join("cache/blake3").join(&hex[..2]).join(&hex)
    }

    pub(crate) async fn get_cache(&self, key: &CacheKey) -> io::Result<Option<Blake3Hash>> {
        let path = Self::cache_path(self.primary.root(), key);
        read_hash_mapping(&path).await
    }

    pub(crate) async fn put_cache(
        &self,
        key: &CacheKey,
        manifest_hash: &Blake3Hash,
    ) -> io::Result<()> {
        let dest = Self::cache_path(self.primary.root(), key);
        atomic_write(&dest, manifest_hash.to_hex().as_bytes()).await
    }

    pub(crate) async fn delete_cache(&self, key: &CacheKey) -> io::Result<bool> {
        let path = Self::cache_path(self.primary.root(), key);
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

async fn read_hash_mapping(path: &Utf8Path) -> io::Result<Option<Blake3Hash>> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(Blake3Hash::from_hex(content.trim())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
