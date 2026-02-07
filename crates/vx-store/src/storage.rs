//! Storage abstraction for CAS blob operations.
//!
//! The `CasStorage` trait covers only blob-level operations — the fundamental
//! content-addressed primitive. Everything else (manifests, specs, cache) is
//! built on top as methods on the concrete `CasStore` type.
//!
//! Two blob backends:
//! - `FsStorage`: single filesystem directory
//! - `LayeredStorage`: reads fall through from primary to base, writes go to primary only
//!
//! `CasStore` wraps a primary `FsStorage` with an optional read-only base, providing
//! layered access for all storage categories (blobs, manifests, specs, cache).

use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use vx_io::atomic_write;
use vx_store_proto::{
    Blake3Hash, BlobHash, CacheKey, ManifestHash, RegistrySpecKey, ToolchainSpecKey,
};

// =============================================================================
// CasStorage trait — blobs only
// =============================================================================

/// Content-addressed blob storage.
///
/// All blob paths use a two-level directory scheme: `blobs/blake3/{hex[..2]}/{hex}`.
pub(crate) trait CasStorage: Send + Sync {
    /// Read a blob by hash. Returns `None` if the blob does not exist.
    async fn get_blob(&self, hash: &BlobHash) -> io::Result<Option<Vec<u8>>>;

    /// Write a blob. Idempotent — writing the same hash twice is a no-op.
    async fn put_blob(&self, hash: &BlobHash, data: &[u8]) -> io::Result<()>;

    /// Check whether a blob exists.
    async fn has_blob(&self, hash: &BlobHash) -> io::Result<bool>;

    /// Return the filesystem path to a blob, for streaming reads.
    /// Returns `None` if the blob does not exist at any layer.
    async fn blob_path(&self, hash: &BlobHash) -> io::Result<Option<Utf8PathBuf>>;
}

// =============================================================================
// FsStorage — single-directory blob backend
// =============================================================================

pub(crate) struct FsStorage {
    root: Utf8PathBuf,
}

impl FsStorage {
    pub(crate) fn new(root: Utf8PathBuf) -> Self {
        Self { root }
    }

    fn blobs_dir(&self) -> Utf8PathBuf {
        self.root.join("blobs/blake3")
    }

    fn blob_path_for(&self, hash: &BlobHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        self.blobs_dir().join(&hex[..2]).join(&hex)
    }

    pub(crate) fn root(&self) -> &Utf8Path {
        &self.root
    }
}

impl CasStorage for FsStorage {
    async fn get_blob(&self, hash: &BlobHash) -> io::Result<Option<Vec<u8>>> {
        let path = self.blob_path_for(hash);
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn put_blob(&self, hash: &BlobHash, data: &[u8]) -> io::Result<()> {
        atomic_write(&self.blob_path_for(hash), data).await
    }

    async fn has_blob(&self, hash: &BlobHash) -> io::Result<bool> {
        let path = self.blob_path_for(hash);
        match tokio::fs::try_exists(&path).await {
            Ok(exists) => Ok(exists),
            Err(e) => Err(e),
        }
    }

    async fn blob_path(&self, hash: &BlobHash) -> io::Result<Option<Utf8PathBuf>> {
        let path = self.blob_path_for(hash);
        match tokio::fs::try_exists(&path).await {
            Ok(true) => Ok(Some(path)),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// =============================================================================
// CasStore — concrete storage with layered access for everything
// =============================================================================

/// Complete CAS storage: blobs via `CasStorage` trait, plus manifests, specs,
/// cache as inherent methods. Optionally layers a read-only base under the primary.
pub(crate) struct CasStore {
    primary: FsStorage,
    base: Option<FsStorage>,
}

impl CasStore {
    pub(crate) fn new(primary: FsStorage) -> Self {
        Self {
            primary,
            base: None,
        }
    }

    pub(crate) fn with_base(primary: FsStorage, base: FsStorage) -> Self {
        Self {
            primary,
            base: Some(base),
        }
    }

    /// Create all required subdirectories (primary only — base is read-only).
    pub(crate) async fn init(&self) -> io::Result<()> {
        let root = self.primary.root();
        tokio::fs::create_dir_all(root.join("blobs/blake3")).await?;
        tokio::fs::create_dir_all(root.join("manifests/blake3")).await?;
        tokio::fs::create_dir_all(root.join("trees/blake3")).await?;
        tokio::fs::create_dir_all(root.join("cache/blake3")).await?;
        tokio::fs::create_dir_all(root.join("tmp")).await?;
        tokio::fs::create_dir_all(root.join("toolchains")).await?;
        tokio::fs::create_dir_all(root.join("registry/spec")).await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn tmp_dir(&self) -> Utf8PathBuf {
        self.primary.root().join("tmp")
    }

    // -- Blob operations (delegated to CasStorage trait) --

    pub(crate) async fn get_blob(&self, hash: &BlobHash) -> io::Result<Option<Vec<u8>>> {
        if let Some(data) = self.primary.get_blob(hash).await? {
            return Ok(Some(data));
        }
        if let Some(base) = &self.base {
            return base.get_blob(hash).await;
        }
        Ok(None)
    }

    pub(crate) async fn put_blob(&self, hash: &BlobHash, data: &[u8]) -> io::Result<()> {
        self.primary.put_blob(hash, data).await
    }

    pub(crate) async fn has_blob(&self, hash: &BlobHash) -> io::Result<bool> {
        if self.primary.has_blob(hash).await? {
            return Ok(true);
        }
        if let Some(base) = &self.base {
            return base.has_blob(hash).await;
        }
        Ok(false)
    }

    pub(crate) async fn blob_path(&self, hash: &BlobHash) -> io::Result<Option<Utf8PathBuf>> {
        if let Some(path) = self.primary.blob_path(hash).await? {
            return Ok(Some(path));
        }
        if let Some(base) = &self.base {
            return base.blob_path(hash).await;
        }
        Ok(None)
    }

    // -- Manifests (JSON, stored in manifests/blake3/) --

    fn manifest_path(root: &Utf8Path, hash: &ManifestHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        root.join("manifests/blake3")
            .join(&hex[..2])
            .join(format!("{}.json", hex))
    }

    pub(crate) async fn get_manifest(&self, hash: &ManifestHash) -> io::Result<Option<String>> {
        let path = Self::manifest_path(self.primary.root(), hash);
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => return Ok(Some(json)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if let Some(base) = &self.base {
            let path = Self::manifest_path(base.root(), hash);
            match tokio::fs::read_to_string(&path).await {
                Ok(json) => return Ok(Some(json)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    pub(crate) async fn put_manifest(&self, hash: &ManifestHash, json: &[u8]) -> io::Result<()> {
        let path = Self::manifest_path(self.primary.root(), hash);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(());
        }
        atomic_write(&path, json).await
    }

    // -- Tree manifests (stored in trees/blake3/) --

    fn tree_manifest_path(root: &Utf8Path, hash: &ManifestHash) -> Utf8PathBuf {
        let hex = hash.to_hex();
        root.join("trees/blake3")
            .join(&hex[..2])
            .join(format!("{}.json", hex))
    }

    #[allow(dead_code)]
    pub(crate) async fn get_tree_manifest(
        &self,
        hash: &ManifestHash,
    ) -> io::Result<Option<String>> {
        let path = Self::tree_manifest_path(self.primary.root(), hash);
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => return Ok(Some(json)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if let Some(base) = &self.base {
            let path = Self::tree_manifest_path(base.root(), hash);
            match tokio::fs::read_to_string(&path).await {
                Ok(json) => return Ok(Some(json)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    pub(crate) async fn put_tree_manifest(
        &self,
        hash: &ManifestHash,
        json: &[u8],
    ) -> io::Result<()> {
        let path = Self::tree_manifest_path(self.primary.root(), hash);
        atomic_write(&path, json).await
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
        if let Some(hash) = read_hash_mapping(&path).await? {
            return Ok(Some(hash));
        }
        if let Some(base) = &self.base {
            let path = Self::toolchain_spec_path(base.root(), key);
            return read_hash_mapping(&path).await;
        }
        Ok(None)
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
        if let Some(hash) = read_hash_mapping(&path).await? {
            return Ok(Some(hash));
        }
        if let Some(base) = &self.base {
            let path = Self::registry_spec_path(base.root(), key);
            return read_hash_mapping(&path).await;
        }
        Ok(None)
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

    // -- Cache (cache_key → manifest_hash mapping) --

    fn cache_path(root: &Utf8Path, key: &CacheKey) -> Utf8PathBuf {
        let hex = key.to_hex();
        root.join("cache/blake3").join(&hex[..2]).join(&hex)
    }

    pub(crate) async fn get_cache(&self, key: &CacheKey) -> io::Result<Option<ManifestHash>> {
        let path = Self::cache_path(self.primary.root(), key);
        if let Some(hash) = read_manifest_hash_mapping(&path).await? {
            return Ok(Some(hash));
        }
        if let Some(base) = &self.base {
            let path = Self::cache_path(base.root(), key);
            return read_manifest_hash_mapping(&path).await;
        }
        Ok(None)
    }

    pub(crate) async fn put_cache(
        &self,
        key: &CacheKey,
        manifest_hash: &ManifestHash,
    ) -> io::Result<()> {
        let dest = Self::cache_path(self.primary.root(), key);
        atomic_write(&dest, manifest_hash.to_hex().as_bytes()).await
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

async fn read_manifest_hash_mapping(path: &Utf8Path) -> io::Result<Option<ManifestHash>> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(ManifestHash::from_hex(content.trim())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
