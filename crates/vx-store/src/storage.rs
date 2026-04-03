//! Storage abstraction for CAS chunk and node operations.
//!
//! Two object types:
//! - **Chunks**: Raw bytes (≤4MB), stored at `chunks/blake3/{xx}/{hash}`
//! - **Nodes**: Versioned postcard-encoded structures, stored at `nodes/blake3/{xx}/{hash}`

use std::io;

use camino::{Utf8Path, Utf8PathBuf};

use vx_io::atomic_write;
use vx_services::{ChunkHash, NodeHash};

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

    fn chunks_dir(&self) -> Utf8PathBuf {
        self.root.join("chunks/blake3")
    }

    fn chunk_path_for(&self, hash: &ChunkHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        self.chunks_dir().join(&hex[..2]).join(&hex)
    }

    fn nodes_dir(&self) -> Utf8PathBuf {
        self.root.join("nodes/blake3")
    }

    fn node_path_for(&self, hash: &NodeHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        self.nodes_dir().join(&hex[..2]).join(&hex)
    }

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
// CasStore — concrete storage for chunks and nodes
// =============================================================================

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
        moire::fs::create_dir_all(root.join("tmp")).await?;
        Ok(())
    }

    pub(crate) async fn get_chunk(&self, hash: &ChunkHash) -> io::Result<Option<Vec<u8>>> {
        self.primary.get_chunk(hash).await
    }

    pub(crate) async fn put_chunk(&self, hash: &ChunkHash, data: &[u8]) -> io::Result<()> {
        self.primary.put_chunk(hash, data).await
    }

    pub(crate) async fn has_chunk(&self, hash: &ChunkHash) -> io::Result<bool> {
        self.primary.has_chunk(hash).await
    }

    pub(crate) async fn get_node(&self, hash: &NodeHash) -> io::Result<Option<Vec<u8>>> {
        self.primary.get_node(hash).await
    }

    pub(crate) async fn put_node(&self, hash: &NodeHash, data: &[u8]) -> io::Result<()> {
        self.primary.put_node(hash, data).await
    }

    pub(crate) async fn has_node(&self, hash: &NodeHash) -> io::Result<bool> {
        self.primary.has_node(hash).await
    }
}
