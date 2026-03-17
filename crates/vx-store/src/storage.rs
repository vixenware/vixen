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
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use tokio::io::AsyncWriteExt;
use vx_io::atomic_write;
use vx_store_proto::{
    Blake3Hash, CacheKey, ChunkHash, NodeHash, RegistrySpecKey, ToolchainSpecKey,
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
        match moire::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn put_chunk(&self, hash: &ChunkHash, data: &[u8]) -> io::Result<()> {
        if self.has_chunk(hash).await? {
            return Ok(());
        }
        atomic_write(&self.chunk_path_for(hash), data, false).await
    }

    pub(crate) async fn has_chunk(&self, hash: &ChunkHash) -> io::Result<bool> {
        let path = self.chunk_path_for(hash);
        match moire::fs::try_exists(&path).await {
            Ok(exists) => Ok(exists),
            Err(e) => Err(e),
        }
    }

    // -- Node operations --

    pub(crate) async fn get_node(&self, hash: &NodeHash) -> io::Result<Option<Vec<u8>>> {
        let path = self.node_path_for(hash);
        match moire::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn put_node(&self, hash: &NodeHash, data: &[u8]) -> io::Result<()> {
        if self.has_node(hash).await? {
            return Ok(());
        }
        atomic_write(&self.node_path_for(hash), data, false).await
    }

    pub(crate) async fn has_node(&self, hash: &NodeHash) -> io::Result<bool> {
        let path = self.node_path_for(hash);
        match moire::fs::try_exists(&path).await {
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
        moire::fs::create_dir_all(root.join("chunks/blake3")).await?;
        moire::fs::create_dir_all(root.join("nodes/blake3")).await?;
        moire::fs::create_dir_all(root.join("cache/blake3")).await?;
        moire::fs::create_dir_all(root.join("tmp")).await?;
        moire::fs::create_dir_all(root.join("toolchains")).await?;
        moire::fs::create_dir_all(root.join("registry/spec")).await?;
        Ok(())
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
        let parent = path
            .parent()
            .expect("toolchain spec path has parent")
            .to_path_buf();
        let manifest_hex = manifest_hash.to_hex();

        moire::fs::create_dir_all(&parent).await?;

        let mut opts = moire::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        match opts.open(&path).await {
            Ok(mut file) => {
                file.write_all(manifest_hex.as_bytes()).await?;
                file.sync_all().await?;
                if let Ok(dir) = moire::fs::File::open(&parent).await
                    && let Err(e) = dir.sync_all().await
                {
                    tracing::warn!("failed to sync directory {}: {}", parent, e);
                }
                Ok(true)
            }
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

        moire::fs::create_dir_all(&parent).await?;

        let mut opts = moire::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        match opts.open(&path).await {
            Ok(mut file) => {
                file.write_all(manifest_hex.as_bytes()).await?;
                file.sync_all().await?;
                if let Ok(dir) = moire::fs::File::open(&parent).await
                    && let Err(e) = dir.sync_all().await
                {
                    tracing::warn!("failed to sync directory {}: {}", parent, e);
                }
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
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
        atomic_write(&dest, manifest_hash.to_hex().as_bytes(), false).await
    }

    pub(crate) async fn delete_cache(&self, key: &CacheKey) -> io::Result<bool> {
        let path = Self::cache_path(self.primary.root(), key);
        match moire::fs::remove_file(path).await {
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
    const MAX_ATTEMPTS: usize = 20;
    const RETRY_DELAY: Duration = Duration::from_millis(10);

    for attempt in 0..MAX_ATTEMPTS {
        match moire::fs::read_to_string(path).await {
            Ok(content) => {
                let trimmed = content.trim();
                if let Some(hash) = Blake3Hash::from_hex(trimmed) {
                    return Ok(Some(hash));
                }
                if attempt + 1 < MAX_ATTEMPTS {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid hash mapping in {}", path),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("failed to read hash mapping in {}", path),
    ))
}
