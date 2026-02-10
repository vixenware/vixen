//! CAS service protocol definitions
//!
//! CAS stores immutable content. Clients produce working directories.
//!
//! The store is purely about:
//! - put/get blobs (content-addressed by blake3)
//! - put/get manifests (structured build outputs)
//! - lookup/publish cache keys (mapping inputs → outputs)

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};

use facet::Facet;

/// Current cache key schema version.
/// Bump this when the cache key canonicalization changes.
pub const CACHE_KEY_SCHEMA_VERSION: u32 = 1;

/// CAS protocol version.
/// Bump this when making breaking changes to the CAS RPC interface.
pub const STORE_PROTOCOL_VERSION: u32 = 2;

/// A blake3 hash, used for blobs, manifests, and cache keys.
/// Internally stored as raw bytes; hex formatting is for display only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
pub struct Blake3Hash(#[facet(sensitive)] pub [u8; 32]);

impl Blake3Hash {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// Get first 16 hex chars (8 bytes) for display
    pub fn short_hex(&self) -> String {
        self.0[..8].iter().map(|b| format!("{:02x}", b)).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut arr = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex_str = std::str::from_utf8(chunk).ok()?;
            arr[i] = u8::from_str_radix(hex_str, 16).ok()?;
        }
        Some(Self(arr))
    }
}

impl std::fmt::Display for Blake3Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Passthrough hasher for Blake3Hash — the hash is already cryptographic, so re-hashing
/// with SipHash is pure waste. Extracts the first 8 bytes of the Blake3 digest as the
/// HashMap bucket selector.
#[derive(Default, Clone)]
pub struct Blake3HashBuilder;

impl BuildHasher for Blake3HashBuilder {
    type Hasher = Blake3Hasher;
    fn build_hasher(&self) -> Self::Hasher {
        Blake3Hasher(0)
    }
}

pub struct Blake3Hasher(u64);

impl Hasher for Blake3Hasher {
    fn write(&mut self, bytes: &[u8]) {
        // Blake3Hash.hash() writes the 32-byte array. We take the first 8 bytes as u64.
        // If fewer than 8 bytes are written (shouldn't happen for Blake3Hash), we still
        // accumulate what we get.
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.0 ^= u64::from_ne_bytes(buf);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// A HashMap using the passthrough hasher for Blake3Hash keys.
pub type Blake3HashMap<V> = HashMap<Blake3Hash, V, Blake3HashBuilder>;

/// Hash of a blob (raw bytes)
pub type BlobHash = Blake3Hash;

/// Hash of a manifest (JSON document)
pub type ManifestHash = Blake3Hash;

/// Hash of all inputs (cache lookup key)
pub type CacheKey = Blake3Hash;

/// A stable, human-readable node identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct NodeId(pub String);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Output entry in a manifest
#[derive(Debug, Clone, Facet)]
pub struct OutputEntry {
    /// Logical name ("bin", "rlib", etc.)
    pub logical: String,
    /// Filename for materialization ("hello", "libfoo.rlib")
    pub filename: String,
    /// Hash of the blob
    pub blob: BlobHash,
    /// Size in bytes
    pub size: u64,
    /// Whether the file should be executable
    pub executable: bool,
}

/// A node manifest (what a node produced)
#[derive(Debug, Clone, Facet)]
pub struct NodeManifest {
    pub node_id: NodeId,
    pub cache_key: CacheKey,
    pub produced_at: String,
    pub outputs: Vec<OutputEntry>,
}

/// Result of publishing a cache entry
#[derive(Debug, Clone, Facet)]
pub struct PublishResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Version information for a service
#[derive(Debug, Clone, Facet)]
pub struct ServiceVersion {
    /// Service name (e.g., "vx-store")
    pub service: String,
    /// Crate version (from CARGO_PKG_VERSION)
    pub version: String,
    /// Protocol/schema version (bump on breaking changes)
    pub protocol_version: u32,
}

// =============================================================================
// Tree Ingestion Types
// =============================================================================

/// A file to ingest into a tree
#[derive(Debug, Clone, Facet)]
pub struct TreeFile {
    /// Relative path within the tree (UTF-8, forward slashes)
    pub path: String,
    /// File contents
    pub contents: Vec<u8>,
    /// Whether the file is executable
    pub executable: bool,
}

/// Request to ingest a tree of files into CAS
#[derive(Debug, Clone, Facet)]
pub struct IngestTreeRequest {
    /// Files to ingest
    pub files: Vec<TreeFile>,
}

/// Result of tree ingestion
#[derive(Debug, Clone, Facet)]
pub struct IngestTreeResult {
    /// Whether ingestion succeeded
    pub success: bool,
    /// Hash of the TreeManifest (None on failure)
    pub manifest_hash: Option<ManifestHash>,
    /// Number of files ingested
    pub file_count: u32,
    /// Total bytes ingested
    pub total_bytes: u64,
    /// Number of new blobs stored (vs deduplicated)
    pub new_blobs: u32,
    /// Error message if failed
    pub error: Option<String>,
}

/// Progress update streamed during long-running store operations (downloads, extraction).
#[derive(Debug, Clone, Facet)]
pub struct StoreProgress {
    /// Human-readable phase description (e.g., "downloading rustc", "extracting rust-std")
    pub message: String,
    /// Bytes or items completed so far
    pub current: Option<u64>,
    /// Total bytes or items expected
    pub total: Option<u64>,
}

/// CAS service trait
///
/// CAS stores immutable content. Clients produce working directories.
#[roam::service]
#[allow(async_fn_in_trait)]
pub trait Store {
    // =========================================================================
    // Service metadata
    // =========================================================================

    /// Get service version information (for health checks and compatibility)
    async fn version(&self) -> ServiceVersion;

    // =========================================================================
    // Cache key operations
    // =========================================================================

    /// Look up a cache key, returns manifest hash if found
    async fn lookup(&self, cache_key: CacheKey) -> Option<ManifestHash>;

    /// Publish a cache entry atomically.
    /// Validates that the manifest exists (and optionally its blobs) before
    /// writing the cache key → manifest hash mapping.
    async fn publish(&self, cache_key: CacheKey, manifest_hash: ManifestHash) -> PublishResult;

    /// Remove a cache key mapping. Blobs and manifests are left intact
    /// (they're content-addressed and harmless to keep).
    /// Used by ephemeral sessions to roll back cache entries on disconnect.
    async fn unpublish(&self, cache_key: CacheKey) -> bool;

    // =========================================================================
    // Manifest operations
    // =========================================================================

    /// Store a manifest, returns its hash
    async fn put_manifest(&self, manifest: NodeManifest) -> ManifestHash;

    /// Get a manifest by hash
    async fn get_manifest(&self, hash: ManifestHash) -> Option<NodeManifest>;

    // =========================================================================
    // Blob operations (small blobs, recommended < 10MB)
    // =========================================================================

    /// Store a blob, returns its hash.
    /// For small blobs. Use chunked upload for large files.
    // NOTE: Vec<u8> is not zerocopy-friendly. For v0 this is acceptable.
    async fn put_blob(&self, data: Vec<u8>) -> BlobHash;

    /// Get a blob by hash.
    /// Returns None if not found.
    async fn get_blob(&self, hash: BlobHash) -> Option<Vec<u8>>;

    /// Get a range of bytes from a blob.
    /// Returns None if the blob does not exist.
    /// The returned Vec may be shorter than `len` if the range extends past EOF.
    async fn get_blob_range(&self, hash: BlobHash, offset: u64, len: u64) -> Option<Vec<u8>>;

    /// Check if a blob exists
    async fn has_blob(&self, hash: BlobHash) -> bool;

    // =========================================================================
    // Streaming blob operations (for large files like toolchains)
    // =========================================================================

    /// Stream blob chunks for extraction.
    ///
    /// NOTE: This is chunked delivery (Vec<u8> per chunk), not zero-copy.
    /// Each chunk involves allocation. Fine for v1 but don't assume this is
    /// free for multi-GB transfers.
    async fn stream_blob(&self, blob: Blake3Hash, output: roam::Tx<Vec<u8>>) -> Result<(), String>;

    // =========================================================================
    // Tree ingestion (for uploading source trees)
    // =========================================================================

    /// Ingest a tree of files into the store.
    ///
    /// Each file is stored as a blob, and a TreeManifest is created
    /// referencing all the blobs. Returns the hash of the TreeManifest.
    ///
    /// This is used by the daemon to upload source files before compilation.
    async fn ingest_tree(&self, request: IngestTreeRequest) -> IngestTreeResult;

    /// Get a TreeManifest by its hash
    async fn get_tree_manifest(&self, hash: ManifestHash) -> Option<TreeManifest>;

    // =========================================================================
    // Toolchain operations (called by execd/daemon)
    // =========================================================================

    /// Ensure a Rust toolchain exists in the store. Downloads if needed.
    /// Progress updates (download bytes, extraction files) are streamed to `progress`.
    async fn ensure_rust_toolchain(
        &self,
        spec: RustToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult;

    /// Download a single Rust toolchain component tarball.
    async fn download_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult;

    /// Extract a single Rust toolchain component tarball into a tree manifest.
    async fn extract_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        tarball_blob: Blake3Hash,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult;

    /// Finalize a Rust toolchain manifest from already extracted components.
    async fn finalize_rust_toolchain(&self, spec: RustToolchainSpec) -> EnsureToolchainResult;

    /// Ensure a Zig toolchain exists in the store. Downloads if needed.
    /// Progress updates are streamed to `progress`.
    async fn ensure_zig_toolchain(
        &self,
        spec: ZigToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult;

    /// Get toolchain manifest by its hash
    async fn get_toolchain_manifest(&self, manifest_hash: Blake3Hash) -> Option<ToolchainManifest>;

    /// Get materialization plan for a toolchain
    async fn get_materialization_plan(
        &self,
        manifest_hash: Blake3Hash,
    ) -> Option<MaterializationPlan>;

    /// Lookup manifest hash by toolchain spec key
    async fn lookup_toolchain_spec(&self, spec_key: ToolchainSpecKey) -> Option<Blake3Hash>;

    // =========================================================================
    // Registry operations (called by execd/daemon)
    // =========================================================================

    /// Ensure a registry crate exists in the store. Downloads if needed.
    /// Progress updates are streamed to `progress`.
    async fn ensure_registry_crate(
        &self,
        spec: RegistrySpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRegistryCrateResult;

    /// Get a registry crate manifest by its hash
    async fn get_registry_manifest(
        &self,
        manifest_hash: Blake3Hash,
    ) -> Option<RegistryCrateManifest>;

    /// Lookup manifest hash by spec key
    async fn lookup_registry_spec(&self, spec_key: Blake3Hash) -> Option<Blake3Hash>;
}

// =============================================================================
// Toolchain Types
// =============================================================================

pub type ToolchainSpecKey = Blake3Hash;
pub type ToolchainId = Blake3Hash;

// Toolchain Specs (What is Requested)

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[repr(u8)]
pub enum RustChannel {
    Stable,
    Beta,
    Nightly { date: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[repr(u8)]
pub enum RustComponent {
    Rustc,
    RustStd,
    Cargo,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RustToolchainSpec {
    pub channel: RustChannel,
    pub host: String,
    pub target: String,
    pub components: Vec<RustComponent>,
}

impl RustToolchainSpec {
    /// Compute canonical SpecKey.
    /// NOTE: No lowercasing - triples are already canonical. Just trim().
    pub fn spec_key(&self) -> Result<ToolchainSpecKey, &'static str> {
        // Reject empty components early
        if self.components.is_empty() {
            return Err("components cannot be empty");
        }
        if self.host.trim().is_empty() {
            return Err("host cannot be empty");
        }
        if self.target.trim().is_empty() {
            return Err("target cannot be empty");
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"toolchain-spec-v1\n");
        hasher.update(b"kind:rust\n");

        match &self.channel {
            RustChannel::Stable => {
                hasher.update(b"channel:stable\n");
            }
            RustChannel::Beta => {
                hasher.update(b"channel:beta\n");
            }
            RustChannel::Nightly { date } => {
                let date = date.trim();
                if date.is_empty() {
                    return Err("nightly date cannot be empty");
                }
                hasher.update(b"channel:nightly:");
                hasher.update(date.as_bytes());
                hasher.update(b"\n");
            }
        }

        hasher.update(b"host:");
        hasher.update(self.host.trim().as_bytes());
        hasher.update(b"\n");

        hasher.update(b"target:");
        hasher.update(self.target.trim().as_bytes());
        hasher.update(b"\n");

        // Sort components for determinism
        let mut components: Vec<_> = self.components.iter().collect();
        components.sort_by_key(|c| match c {
            RustComponent::Rustc => 0,
            RustComponent::RustStd => 1,
            RustComponent::Cargo => 2,
        });

        hasher.update(b"components:");
        for (i, c) in components.iter().enumerate() {
            if i > 0 {
                hasher.update(b",");
            }
            let _ = match c {
                RustComponent::Rustc => hasher.update(b"rustc"),
                RustComponent::RustStd => hasher.update(b"rust-std"),
                RustComponent::Cargo => hasher.update(b"cargo"),
            };
        }
        hasher.update(b"\n");

        Ok(Blake3Hash(*hasher.finalize().as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct ZigToolchainSpec {
    pub version: String,
    pub host_platform: String,
}

impl ZigToolchainSpec {
    pub fn spec_key(&self) -> Result<ToolchainSpecKey, &'static str> {
        if self.version.trim().is_empty() {
            return Err("version cannot be empty");
        }
        if self.host_platform.trim().is_empty() {
            return Err("host_platform cannot be empty");
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"toolchain-spec-v1\n");
        hasher.update(b"kind:zig\n");
        hasher.update(b"version:");
        hasher.update(self.version.trim().as_bytes());
        hasher.update(b"\n");
        hasher.update(b"platform:");
        hasher.update(self.host_platform.trim().as_bytes());
        hasher.update(b"\n");
        Ok(Blake3Hash(*hasher.finalize().as_bytes()))
    }
}

// Toolchain Manifest (What Was Acquired)

#[derive(Debug, Clone, Facet)]
pub struct ToolchainManifest {
    /// Schema version for format evolution
    pub schema_version: u32,
    /// Kind of toolchain
    pub kind: ToolchainKind,
    /// SpecKey that produced this manifest (for reverse lookup)
    pub spec_key: ToolchainSpecKey,
    /// Content-derived toolchain identity
    pub toolchain_id: ToolchainId,
    /// When acquired (ISO 8601 string)
    pub created_at: String,

    // === Rust-specific fields ===
    /// Resolved manifest date (for stable/beta this captures the actual version)
    pub rust_manifest_date: Option<String>,
    /// Rustc version string
    pub rust_version: Option<String>,

    // === Zig-specific fields ===
    pub zig_version: Option<String>,

    /// Components stored as trees (deduped file storage)
    pub components: Vec<ToolchainComponentTree>,
}

/// Schema version 3: components stored as trees instead of tarballs
pub const TOOLCHAIN_MANIFEST_SCHEMA_VERSION: u32 = 3;

/// A toolchain component stored as a tree (deduped file storage)
#[derive(Debug, Clone, Facet)]
pub struct ToolchainComponentTree {
    /// Component name: "rustc", "rust-std", "zig-exe", "zig-lib"
    pub name: String,
    /// Target triple (for rust-std)
    pub target: Option<String>,
    /// Hash of the TreeManifest blob
    pub tree_manifest: Blake3Hash,
    /// Upstream SHA256 hex (for provenance/verification)
    pub sha256: String,
    /// Total size of all files in the tree
    pub total_size_bytes: u64,
    /// Number of files in the tree
    pub file_count: u32,
    /// Number of unique blobs (for dedup stats)
    pub unique_blobs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum ToolchainKind {
    Rust,
    Zig,
}

// Materialization Plan (How to Materialize)

/// Materialization plan generated by the store, executed by the runner.
///
/// STABILITY CONTRACT: For a given manifest_hash, this plan is bit-for-bit
/// stable across time and store versions within the same layout_version.
/// If plan generation changes, bump MATERIALIZATION_LAYOUT_VERSION.
#[derive(Debug, Clone, Facet)]
pub struct MaterializationPlan {
    pub toolchain_id: ToolchainId,
    /// Layout version - execd cache includes this to avoid corruption
    pub layout_version: u32,
    pub steps: Vec<MaterializeStep>,
}

pub const MATERIALIZATION_LAYOUT_VERSION: u32 = 1;

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum MaterializeStep {
    /// Ensure a directory exists (for explicit dir creation).
    /// relpath must be non-empty; use "." for toolchain root if needed.
    EnsureDir { relpath: String },
    /// Create a symlink (target must not escape toolchain root)
    Symlink { relpath: String, target: String },
    /// Materialize a tree from a TreeManifest
    MaterializeTree {
        /// Hash of the TreeManifest blob
        tree_manifest: Blake3Hash,
        /// Destination subdirectory relative to toolchain root
        dest_subdir: String,
    },
}

// Toolchain RPC Result Types

#[derive(Debug, Clone, Facet)]
pub struct EnsureToolchainResult {
    /// None if spec was invalid and couldn't be hashed
    pub spec_key: Option<ToolchainSpecKey>,

    /// None on failure
    pub toolchain_id: Option<ToolchainId>,

    /// Hash of the ToolchainManifest in CAS
    pub manifest_hash: Option<Blake3Hash>,

    pub status: EnsureStatus,

    pub error: Option<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct EnsureRustToolchainComponentResult {
    /// None if spec was invalid and couldn't be hashed
    pub spec_key: Option<ToolchainSpecKey>,

    /// Component this result belongs to
    pub component: RustComponent,

    /// Hash of downloaded component tarball blob (download step)
    pub tarball_blob: Option<Blake3Hash>,

    /// Hash of extracted component tree manifest blob (extract step)
    pub tree_manifest: Option<Blake3Hash>,

    pub status: EnsureStatus,

    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum EnsureStatus {
    Hit,        // Already in CAS
    Downloaded, // Just acquired
    Failed,     // Acquisition failed
}

// =============================================================================
// Registry Types
// =============================================================================

pub type RegistrySpecKey = Blake3Hash;

/// The crates.io registry URL (the only supported registry in v1)
pub const CRATES_IO_REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// Current schema version for registry crate manifests.
/// Bump this when the manifest format changes.
pub const REGISTRY_MANIFEST_SCHEMA_VERSION: u32 = 1;

// Registry Spec (What is Requested)

/// Identifies a specific crate version from a registry.
///
/// In v1, only crates.io is supported. The checksum is the SHA256 from
/// Cargo.lock, which is verified against the downloaded tarball.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RegistrySpec {
    /// Registry URL (v1 must equal CRATES_IO_REGISTRY)
    pub registry_url: String,
    /// Crate name
    pub name: String,
    /// Exact version string (e.g., "1.0.197")
    pub version: String,
    /// SHA256 checksum from Cargo.lock (hex-encoded)
    pub checksum: String,
}

impl RegistrySpec {
    /// Create a new RegistrySpec for a crates.io crate.
    pub fn crates_io(
        name: impl Into<String>,
        version: impl Into<String>,
        checksum: impl Into<String>,
    ) -> Self {
        Self {
            registry_url: CRATES_IO_REGISTRY.to_string(),
            name: name.into(),
            version: version.into(),
            checksum: checksum.into(),
        }
    }

    /// Compute canonical SpecKey for cache lookups.
    ///
    /// The spec key is a blake3 hash of the normalized spec fields.
    /// This is used to map spec -> manifest in CAS.
    pub fn spec_key(&self) -> Result<RegistrySpecKey, &'static str> {
        if self.name.trim().is_empty() {
            return Err("name cannot be empty");
        }
        if self.version.trim().is_empty() {
            return Err("version cannot be empty");
        }
        if self.checksum.trim().is_empty() {
            return Err("checksum cannot be empty");
        }
        if self.checksum.len() != 64 {
            return Err("checksum must be 64 hex characters (SHA256)");
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"registry-spec-v1\n");
        hasher.update(b"registry:");
        hasher.update(self.registry_url.as_bytes());
        hasher.update(b"\n");
        hasher.update(b"name:");
        hasher.update(self.name.as_bytes());
        hasher.update(b"\n");
        hasher.update(b"version:");
        hasher.update(self.version.as_bytes());
        hasher.update(b"\n");
        hasher.update(b"checksum:");
        hasher.update(self.checksum.to_lowercase().as_bytes());
        hasher.update(b"\n");

        Ok(Blake3Hash(*hasher.finalize().as_bytes()))
    }

    /// Returns the download URL for this crate from crates.io.
    pub fn download_url(&self) -> String {
        format!(
            "https://crates.io/api/v1/crates/{}/{}/download",
            self.name, self.version
        )
    }
}

// Registry Crate Manifest (What Was Acquired)

/// Manifest for an acquired registry crate, stored in CAS.
///
/// This records what was downloaded and where the tarball bytes are stored.
/// The manifest is content-addressed and immutable once written.
#[derive(Debug, Clone, Facet)]
pub struct RegistryCrateManifest {
    /// Schema version for format evolution
    pub schema_version: u32,

    /// The spec that produced this manifest (includes checksum for provenance)
    pub spec: RegistrySpec,

    /// Blake3 hash of the .crate tarball bytes in CAS
    pub crate_tarball_blob: Blake3Hash,

    /// When acquired (ISO 8601 string)
    pub created_at: String,
}

// Registry RPC Types

/// Result of ensuring a registry crate exists in CAS.
#[derive(Debug, Clone, Facet)]
pub struct EnsureRegistryCrateResult {
    /// The computed spec key (None if spec was invalid)
    pub spec_key: Option<RegistrySpecKey>,

    /// Hash of the RegistryCrateManifest in CAS (None on failure)
    pub manifest_hash: Option<Blake3Hash>,

    /// Status of the operation
    pub status: EnsureStatus,

    /// Error message if status is Failed
    pub error: Option<String>,
}

/// Result of materializing a registry crate.
#[derive(Debug, Clone, Facet)]
pub struct RegistryMaterializationResult {
    /// Workspace-relative path to the materialized crate (e.g., ".vx/registry/serde/1.0.197")
    pub workspace_rel_path: String,

    /// Global cache path (for debugging/reporting)
    pub global_cache_path: String,

    /// Whether the workspace copy was already present with matching checksum
    pub was_cached: bool,
}

// =============================================================================
// Tree Manifest (Content-Addressed Directory)
// =============================================================================

/// A content-addressed directory tree stored in CAS.
///
/// Instead of storing tarballs as blobs, we extract them and store each file
/// as a separate blob. This enables:
/// - Deduplication across versions (e.g., unchanged files between Rust 1.83 and 1.84)
/// - Deduplication across crates (LICENSE files, similar Cargo.toml structures)
/// - Efficient partial materialization (only fetch missing blobs)
///
/// The tree manifest itself is stored as a blob, and its hash serves as the
/// "tree hash" for the entire directory.
#[derive(Debug, Clone, Facet)]
pub struct TreeManifest {
    /// Schema version for format evolution
    pub schema_version: u32,

    /// Entries sorted by path for deterministic hashing
    pub entries: Vec<TreeEntry>,

    /// Total size of all files (for progress reporting)
    pub total_size_bytes: u64,

    /// Number of unique blobs (for dedup stats)
    pub unique_blobs: u32,
}

/// Current schema version for tree manifests
pub const TREE_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// A single entry in a tree manifest
#[derive(Debug, Clone, Facet)]
pub struct TreeEntry {
    /// Relative path within the tree (UTF-8, forward slashes)
    pub path: String,

    /// Entry kind and associated data
    pub kind: TreeEntryKind,
}

/// Kind of tree entry
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum TreeEntryKind {
    /// Regular file
    File {
        /// Blake3 hash of file contents
        blob: Blake3Hash,

        /// File size in bytes
        size: u64,

        /// Whether the file is executable (unix +x)
        executable: bool,
    },
    /// Symbolic link
    Symlink {
        /// Link target (relative path)
        target: String,
    },
    /// Directory (only needed for empty directories)
    Directory,
}

impl Default for TreeManifest {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeManifest {
    /// Create a new empty tree manifest
    pub fn new() -> Self {
        Self {
            schema_version: TREE_MANIFEST_SCHEMA_VERSION,
            entries: Vec::new(),
            total_size_bytes: 0,
            unique_blobs: 0,
        }
    }

    /// Add a file entry
    pub fn add_file(&mut self, path: String, blob: Blake3Hash, size: u64, executable: bool) {
        self.entries.push(TreeEntry {
            path,
            kind: TreeEntryKind::File {
                blob,
                size,
                executable,
            },
        });
        self.total_size_bytes += size;
    }

    /// Add a symlink entry
    pub fn add_symlink(&mut self, path: String, target: String) {
        self.entries.push(TreeEntry {
            path,
            kind: TreeEntryKind::Symlink { target },
        });
    }

    /// Add an empty directory entry
    pub fn add_directory(&mut self, path: String) {
        self.entries.push(TreeEntry {
            path,
            kind: TreeEntryKind::Directory,
        });
    }

    /// Sort entries by path and compute unique blob count
    pub fn finalize(&mut self) {
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));

        // Count unique blobs
        let mut seen = std::collections::HashSet::new();
        for entry in &self.entries {
            if let TreeEntryKind::File { blob, .. } = &entry.kind {
                seen.insert(*blob);
            }
        }
        self.unique_blobs = seen.len() as u32;
    }

    /// Iterate over file entries (for materialization)
    pub fn files(&self) -> impl Iterator<Item = (&str, &Blake3Hash, u64, bool)> {
        self.entries.iter().filter_map(|e| match &e.kind {
            TreeEntryKind::File {
                blob,
                size,
                executable,
            } => Some((e.path.as_str(), blob, *size, *executable)),
            _ => None,
        })
    }

    /// Iterate over symlink entries
    pub fn symlinks(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter_map(|e| match &e.kind {
            TreeEntryKind::Symlink { target } => Some((e.path.as_str(), target.as_str())),
            _ => None,
        })
    }

    /// Iterate over directory entries (empty dirs only)
    pub fn directories(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|e| match &e.kind {
            TreeEntryKind::Directory => Some(e.path.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facet_postcard::{from_slice, to_vec};

    #[test]
    fn test_blake3hash_roundtrip() {
        let hash = Blake3Hash::from_bytes(b"hello world");

        let bytes = to_vec(&hash).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: Blake3Hash = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, hash);
    }

    #[test]
    fn test_option_blake3hash_roundtrip() {
        let hash = Some(Blake3Hash::from_bytes(b"hello world"));

        let bytes = to_vec(&hash).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: Option<Blake3Hash> = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, hash);
    }

    #[test]
    fn test_raw_array_roundtrip() {
        let arr: [u8; 32] = [42u8; 32];

        let bytes = to_vec(&arr).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: [u8; 32] = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, arr);
    }

    // =========================================================================
    // GOLDEN-FILE TESTS: Spec Key Stability
    // =========================================================================
    //
    // These tests pin exact spec key values for known inputs. If any test fails,
    // it means spec key computation changed — which causes toolchain re-downloads
    // and cache misses. That's either:
    //   (a) intentional: update the golden value and note what changed
    //   (b) accidental: revert whatever broke it
    //
    // See: https://github.com/bearcove/vixen/issues/131

    #[test]
    fn golden_rust_toolchain_spec_key_stable() {
        let spec = RustToolchainSpec {
            channel: RustChannel::Stable,
            host: "x86_64-unknown-linux-gnu".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            components: vec![RustComponent::Rustc, RustComponent::RustStd],
        };
        let key = spec.spec_key().unwrap();
        assert_eq!(
            key.to_hex(),
            "e7f69f74b135f66fd7e7d1c4f0e90106c42f8d15f931ff81df6952140fa52d2a",
            "RustToolchainSpec::spec_key (stable) golden value changed! \
             This causes toolchain re-downloads. If intentional, update this value."
        );
    }

    #[test]
    fn golden_rust_toolchain_spec_key_nightly() {
        let spec = RustToolchainSpec {
            channel: RustChannel::Nightly {
                date: "2025-01-15".to_string(),
            },
            host: "aarch64-apple-darwin".to_string(),
            target: "aarch64-apple-darwin".to_string(),
            components: vec![
                RustComponent::Rustc,
                RustComponent::RustStd,
                RustComponent::Cargo,
            ],
        };
        let key = spec.spec_key().unwrap();
        assert_eq!(
            key.to_hex(),
            "b1aa790ac77e55296bcceaf9e1bb1a4e0cdb11a0f0fab457d1fc6be93f416cff",
            "RustToolchainSpec::spec_key (nightly) golden value changed! \
             This causes toolchain re-downloads. If intentional, update this value."
        );
    }

    #[test]
    fn golden_zig_toolchain_spec_key() {
        let spec = ZigToolchainSpec {
            version: "0.13.0".to_string(),
            host_platform: "x86_64-linux".to_string(),
        };
        let key = spec.spec_key().unwrap();
        assert_eq!(
            key.to_hex(),
            "b303dc7b65d5f177c92dbd92bdca5a8fbd74e8806017bc29fcbd0ffd52b3c837",
            "ZigToolchainSpec::spec_key golden value changed! \
             This causes toolchain re-downloads. If intentional, update this value."
        );
    }

    #[test]
    fn golden_registry_spec_key() {
        let spec = RegistrySpec::crates_io(
            "serde",
            "1.0.197",
            "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0",
        );
        let key = spec.spec_key().unwrap();
        assert_eq!(
            key.to_hex(),
            "ef622bfb7aabcd2fdd23f734c072faec8df1904b40308221f0618bfb29a1c5ee",
            "RegistrySpec::spec_key golden value changed! \
             This causes crate re-downloads. If intentional, update this value."
        );
    }

    #[test]
    fn golden_blake3hash_from_bytes() {
        let hash = Blake3Hash::from_bytes(b"golden-test-input");
        assert_eq!(
            hash.to_hex(),
            "96a1166154903a02c7ee757746e587b8ca48ad5c6305383cdbb683f4fcfbf30d",
            "Blake3Hash::from_bytes golden value changed! \
             If intentional, update this value."
        );
    }
}
