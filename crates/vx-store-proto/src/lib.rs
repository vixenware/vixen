//! CAS service protocol definitions
//!
//! The store is a validated key-value store with two object types:
//! - **Chunks**: Raw bytes (<=4MB), stored at `chunks/blake3/{xx}/{hash}`
//! - **Nodes**: Versioned postcard-encoded structures, stored at `nodes/blake3/{xx}/{hash}`
//!
//! All chunking, assembly, and Merkle tree construction logic lives in shared
//! library code used by clients. The store validates structure but never chunks
//! or reassembles.

pub mod cas_tree;
pub mod chunking;

use core::fmt;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};

use facet::Facet;

// =============================================================================
// Protocol Constants
// =============================================================================

/// Current cache key schema version.
/// Bump this when the cache key canonicalization changes.
pub const CACHE_KEY_SCHEMA_VERSION: u32 = 1;

/// CAS protocol version.
/// Bump this when making breaking changes to the CAS RPC interface.
pub const STORE_PROTOCOL_VERSION: u32 = 5;

/// Versioned postcard node format version.
/// Node bytes are `[version: u32 LE][postcard payload]`.
/// The version prefix is part of the hashed content — a version bump
/// produces different hashes for identical logical content.
pub const CAS_NODE_VERSION: u32 = 1;

// =============================================================================
// CDC Constants
// =============================================================================

/// Content-Defined Chunking minimum chunk size (512 KB)
pub const CDC_MIN_CHUNK: u32 = 512 * 1024;

/// Content-Defined Chunking average chunk size (2 MB)
pub const CDC_AVG_CHUNK: u32 = 2 * 1024 * 1024;

/// Content-Defined Chunking maximum chunk size (4 MB)
pub const CDC_MAX_CHUNK: u32 = 4 * 1024 * 1024;

/// Current CDC parameter set ID. Identifies the combination of CDC algorithm,
/// crate version, and parameters. Registered in KNOWN_CDC_PARAMS.
pub const CDC_PARAMS_ID: u8 = 1;

/// Known CDC parameter sets. Golden-tested.
/// (id, min, avg, max)
pub const KNOWN_CDC_PARAMS: &[(u8, u32, u32, u32)] =
    &[(1, CDC_MIN_CHUNK, CDC_AVG_CHUNK, CDC_MAX_CHUNK)];

// =============================================================================
// Validation Constants
// =============================================================================

/// Maximum number of chunks in a single BlobNode.
/// Enough for ~8TB files at 2MB avg, far beyond any reasonable file.
pub const MAX_CHUNKS_PER_BLOB: usize = 4096;

/// Maximum number of chunk hashes per list_missing_chunks call.
pub const MAX_MISSING_CHUNKS_BATCH: usize = 4096;

/// Maximum number of node hashes per list_missing_nodes call.
pub const MAX_MISSING_NODES_BATCH: usize = 4096;

/// Maximum serialized node size in bytes (16 MB).
/// Prevents OOM from huge directory nodes or adversarial payloads.
pub const MAX_NODE_BYTES: usize = 16 * 1024 * 1024;

// =============================================================================
// Hash Types
// =============================================================================

/// A blake3 hash, the underlying 32-byte value.
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

// =============================================================================
// Newtype Wrappers
// =============================================================================
//
// On the wire and in storage, everything is 32 bytes. But in Rust code, we use
// real newtype structs to prevent domain mixing at compile time.

/// Hash of chunk bytes: blake3(raw_bytes). Used to address chunks in chunks/.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
#[repr(transparent)]
pub struct ChunkHash(pub Blake3Hash);

impl ChunkHash {
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }

    pub fn short_hex(&self) -> String {
        self.0.short_hex()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        Blake3Hash::from_hex(s).map(Self)
    }
}

impl std::fmt::Display for ChunkHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Hash of serialized node bytes: blake3(node_bytes). Used to address nodes in nodes/.
/// ALL node types use the same addressing rule: hash of the serialized bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
#[repr(transparent)]
pub struct NodeHash(pub Blake3Hash);

impl NodeHash {
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }

    pub fn short_hex(&self) -> String {
        self.0.short_hex()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        Blake3Hash::from_hex(s).map(Self)
    }
}

impl std::fmt::Display for NodeHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Hash of full file content: blake3(file_bytes). Used for integrity verification.
/// For single-chunk files, ContentHash.0 == ChunkHash.0 (value equality, not type).
/// For multi-chunk files, this is NOT a storage address — the BlobNode is at a NodeHash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
#[repr(transparent)]
pub struct ContentHash(pub Blake3Hash);

impl ContentHash {
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }

    pub fn short_hex(&self) -> String {
        self.0.short_hex()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        Blake3Hash::from_hex(s).map(Self)
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Legacy aliases — kept temporarily for compilation during migration
/// Hash of a blob (raw bytes) — DEPRECATED, use ChunkHash/NodeHash/ContentHash
pub type BlobHash = Blake3Hash;
/// Hash of a manifest — DEPRECATED, use NodeHash
pub type ManifestHash = Blake3Hash;

/// Hash of all inputs (cache lookup key)
pub type CacheKey = Blake3Hash;

// =============================================================================
// CAS Node Types
// =============================================================================

/// Top-level CAS node enum. All nodes are stored at `nodes/blake3/{xx}/{hash}`
/// where hash = blake3(serialized_bytes).
///
/// Serialization format: `[CAS_NODE_VERSION: u32 LE][postcard payload]`
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum CasNode {
    Blob(BlobNode),
    Directory(DirectoryNode),
    Manifest(NodeManifest),
    Toolchain(ToolchainManifest),
    RegistryCrate(RegistryCrateManifest),
}

impl CasNode {
    /// Serialize to bytes with version prefix.
    /// Format: `[CAS_NODE_VERSION: u32 LE][postcard payload]`
    pub fn to_bytes(&self) -> Vec<u8> {
        let payload = facet_postcard::to_vec(self).expect("CasNode serialization should not fail");
        let mut bytes = Vec::with_capacity(4 + payload.len());
        bytes.extend_from_slice(&CAS_NODE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    /// Deserialize from bytes with version prefix.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CasNodeError> {
        if bytes.len() < 4 {
            return Err(CasNodeError::TooShort);
        }
        let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if version != CAS_NODE_VERSION {
            return Err(CasNodeError::UnknownVersion(version));
        }
        facet_postcard::from_slice(&bytes[4..]).map_err(CasNodeError::Postcard)
    }

    /// Serialize and compute the node hash (blake3 of the complete serialized bytes).
    pub fn to_bytes_and_hash(&self) -> (Vec<u8>, NodeHash) {
        let bytes = self.to_bytes();
        let hash = NodeHash(Blake3Hash(*blake3::hash(&bytes).as_bytes()));
        (bytes, hash)
    }
}

#[derive(Debug)]
pub enum CasNodeError {
    TooShort,
    UnknownVersion(u32),
    Postcard(facet_postcard::DeserializeError),
}

impl std::fmt::Display for CasNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CasNodeError::TooShort => write!(
                f,
                "node bytes too short (need at least 4 bytes for version prefix)"
            ),
            CasNodeError::UnknownVersion(v) => write!(
                f,
                "unknown CAS node version: {v} (expected {CAS_NODE_VERSION})"
            ),
            CasNodeError::Postcard(e) => write!(f, "postcard deserialization failed: {e}"),
        }
    }
}

impl std::error::Error for CasNodeError {}

// =============================================================================
// BlobNode (multi-chunk files only)
// =============================================================================

/// A multi-chunk file. Single-chunk files (≤ CDC_MAX_CHUNK) are represented
/// as SmallFile in directory entries and don't need a BlobNode.
///
/// Chunks are in file order: chunk[0] is the first bytes of the file,
/// chunk[1] immediately follows, etc. Reassembly concatenates in order.
/// Offset computation: chunk[i] starts at sum(chunk[0..i].size).
#[derive(Debug, Clone, Facet)]
pub struct BlobNode {
    /// Chunks in file order. Must have len >= 2 (single-chunk files use SmallFile).
    /// Capped at MAX_CHUNKS_PER_BLOB.
    pub chunks: Vec<ChunkEntry>,
    /// Total size of the file (sum of all chunk sizes).
    pub total_size: u64,
    /// Blake3 of full concatenated content — used for integrity verification
    /// when reassembling. NOT the storage key (storage key is blake3 of
    /// the serialized BlobNode bytes, like all other nodes).
    pub content_hash: ContentHash,
    /// CDC parameter set ID (always 1 today). Enables future migration
    /// if CDC parameters change.
    pub cdc_params_id: u8,
}

/// A single chunk within a BlobNode.
#[derive(Debug, Clone, Facet)]
pub struct ChunkEntry {
    /// Hash of the chunk bytes (used to fetch from chunks/).
    pub hash: ChunkHash,
    /// Size of this chunk in bytes. Must be in 1..=CDC_MAX_CHUNK.
    pub size: u32,
}

// =============================================================================
// DirectoryNode (Merkle directory)
// =============================================================================

// i[impl impl.tree]
/// A content-addressed directory node in the Merkle tree.
/// Replaces the flat TreeManifest.
///
/// Entries are sorted by `name.as_bytes()` (raw byte comparison, no Unicode
/// normalization). Two identical directories always produce the same hash.
#[derive(Debug, Clone, Facet)]
pub struct DirectoryNode {
    /// Directory entries, sorted by name.as_bytes() lexicographic.
    pub entries: Vec<DirEntry>,
    /// Recursive sum of file content bytes in this subtree.
    /// Symlinks contribute 0. For progress reporting only.
    /// Validated at finalization time (not on put_node).
    pub total_size: u64,
}

/// A single entry in a DirectoryNode.
#[derive(Debug, Clone, Facet)]
pub struct DirEntry {
    /// Filename only (not full path). Must be valid UTF-8.
    /// Must not be empty, ".", "..", or contain "/".
    pub name: String,
    /// Entry kind and associated data.
    pub kind: DirEntryKind,
}

/// Kind of directory entry. The type explicitly encodes whether a file is
/// single-chunk or multi-chunk, eliminating any "try chunks/ then nodes/" guessing.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum DirEntryKind {
    /// Regular file, single chunk (≤ CDC_MAX_CHUNK bytes).
    /// `content_hash` is blake3 of the file content, which is also the chunk hash.
    /// Fetch via `get_chunk(content_hash.0)`.
    SmallFile {
        content_hash: ContentHash,
        size: u64,
        executable: bool,
    },
    /// Regular file, multi-chunk (> CDC_MAX_CHUNK bytes).
    /// `content_hash` is blake3 of the full file content (for verification).
    /// `blob_node` is the NodeHash of the BlobNode in nodes/ (for fetching).
    /// Fetch via `get_node(blob_node)` → BlobNode → get_chunk per chunk.
    LargeFile {
        content_hash: ContentHash,
        blob_node: NodeHash,
        size: u64,
        executable: bool,
    },
    /// Subdirectory. hash points to a child DirectoryNode in nodes/.
    Dir { hash: NodeHash },
    /// Symbolic link. Target must be valid UTF-8 (same restriction as name).
    Symlink { target: String },
}

// =============================================================================
// BlobRef — unambiguous "some bytes" reference
// =============================================================================

/// Reference to stored blob data. Used everywhere a "some bytes" reference
/// appears (output entries, tarball refs, etc.). Eliminates "is it a chunk
/// or a node?" ambiguity.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum BlobRef {
    /// Single chunk (≤ CDC_MAX_CHUNK bytes). Fetch via get_chunk.
    Small { chunk: ChunkHash, size: u64 },
    /// Multi-chunk. Fetch via get_node → BlobNode → get_chunk per chunk.
    Large {
        blob_node: NodeHash,
        content_hash: ContentHash,
        size: u64,
    },
}

impl BlobRef {
    /// Convert to the corresponding `DirEntryKind` for embedding in a directory node.
    pub fn to_dir_entry_kind(&self, executable: bool) -> DirEntryKind {
        match self {
            BlobRef::Small { chunk, size } => DirEntryKind::SmallFile {
                content_hash: ContentHash(chunk.0),
                size: *size,
                executable,
            },
            BlobRef::Large {
                blob_node,
                content_hash,
                size,
            } => DirEntryKind::LargeFile {
                blob_node: *blob_node,
                content_hash: *content_hash,
                size: *size,
                executable,
            },
        }
    }
}

// =============================================================================
// FlatTreeEntry (tarball extraction output)
// =============================================================================

/// Flat file entry returned by tarball extraction.
/// The tarball crate returns a flat list of these; the caller builds a
/// DirectoryNode Merkle tree from them.
#[derive(Debug, Clone)]
pub struct FlatTreeEntry {
    /// Full relative path within the tree (UTF-8, forward slashes).
    pub path: String,
    /// Entry kind.
    pub kind: FlatTreeEntryKind,
}

/// Kind of flat tree entry.
#[derive(Debug, Clone)]
pub enum FlatTreeEntryKind {
    /// Regular file.
    File {
        /// Blake3 of file content.
        content_hash: ContentHash,
        /// Size in bytes.
        size: u64,
        /// Whether the file is executable (unix +x).
        executable: bool,
        /// If the file was multi-chunk, the NodeHash of the BlobNode.
        /// None for single-chunk files (≤ CDC_MAX_CHUNK).
        blob_node: Option<NodeHash>,
    },
    /// Symbolic link.
    Symlink { target: String },
    /// Empty directory.
    Directory,
}

// =============================================================================
// Validation Error (finalization)
// =============================================================================

/// Structured validation error returned by finalization points (publish,
/// finalize_rust_toolchain, etc.) when the object graph is incomplete.
#[derive(Debug, Clone, Facet)]
pub struct ValidationError {
    pub missing_chunks: Vec<ChunkHash>,
    pub missing_nodes: Vec<NodeHash>,
    /// Nodes that exist but are the wrong CasNode variant
    /// (e.g., expected "Directory", got "Blob"). Carries (hash, description).
    pub wrong_variant: Vec<WrongVariantEntry>,
    /// Inconsistencies like content_hash/size mismatch, bad total_size.
    /// Carries (hash, description).
    pub inconsistent: Vec<InconsistentEntry>,
    /// Nodes involved in cycles.
    pub cycles: Vec<NodeHash>,
}

#[derive(Debug, Clone, Facet)]
pub struct WrongVariantEntry {
    pub hash: NodeHash,
    pub description: String,
}

#[derive(Debug, Clone, Facet)]
pub struct InconsistentEntry {
    pub hash: NodeHash,
    pub description: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.missing_chunks.is_empty() {
            writeln!(f, "missing chunks: {}", self.missing_chunks.len())?;
        }
        if !self.missing_nodes.is_empty() {
            writeln!(f, "missing nodes: {}", self.missing_nodes.len())?;
        }
        for e in &self.wrong_variant {
            writeln!(f, "wrong variant at {}: {}", e.hash, e.description)?;
        }
        for e in &self.inconsistent {
            writeln!(f, "inconsistent at {}: {}", e.hash, e.description)?;
        }
        if !self.cycles.is_empty() {
            writeln!(f, "cycles involving {} nodes", self.cycles.len())?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

// =============================================================================
// Manifest / Output Types
// =============================================================================

/// A stable, human-readable node identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct NodeId(pub String);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Reference to output blob data in a manifest's OutputEntry.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum OutputBlobRef {
    /// Single-chunk output (≤ CDC_MAX_CHUNK).
    Small { chunk: ChunkHash, size: u64 },
    /// Multi-chunk output.
    Large {
        blob_node: NodeHash,
        content_hash: ContentHash,
        size: u64,
    },
}

/// Output entry in a manifest
#[derive(Debug, Clone, Facet)]
pub struct OutputEntry {
    /// Logical name ("bin", "rlib", etc.)
    pub logical: String,
    /// Filename for materialization ("hello", "libfoo.rlib")
    pub filename: String,
    /// Reference to the output blob
    pub blob: OutputBlobRef,
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

impl fmt::Display for ServiceVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} v{} (proto {})",
            self.service, self.version, self.protocol_version
        )
    }
}

/// Progress update streamed during long-running store operations (downloads, extraction).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
pub enum RemoteTracePhase {
    Begin,
    End,
    Instant,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
pub enum RemoteTraceSpan {
    RunnerPrepareVfs,
    RunnerCommand,
    RunnerObserver,
    RunnerFlushOutputs,
    RunnerStoreManifest,
    RunnerCleanup,
}

#[derive(Debug, Clone, Facet)]
pub struct RemoteTraceEvent {
    pub span: RemoteTraceSpan,
    pub phase: RemoteTracePhase,
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
    /// Optional structured trace event bridged back to the caller.
    pub trace_event: Option<RemoteTraceEvent>,
}

impl StoreProgress {
    pub fn message(message: impl Into<String>, current: Option<u64>, total: Option<u64>) -> Self {
        Self {
            message: message.into(),
            current,
            total,
            trace_event: None,
        }
    }

    pub fn trace(span: RemoteTraceSpan, phase: RemoteTracePhase) -> Self {
        Self {
            message: String::new(),
            current: None,
            total: None,
            trace_event: Some(RemoteTraceEvent { span, phase }),
        }
    }
}

// =============================================================================
// Store Service Trait
// =============================================================================

/// CAS service trait.
///
/// The store is a validated key-value store accepting chunk-shaped and
/// node-shaped requests. All chunking, assembly, and Merkle tree construction
/// logic lives in shared library code used by clients.
#[roam::service]
#[allow(async_fn_in_trait)]
pub trait Store {
    // =========================================================================
    // Service metadata
    // =========================================================================

    /// Get service version information (for health checks and compatibility)
    async fn version(&self) -> ServiceVersion;

    // =========================================================================
    // Chunk operations
    // =========================================================================

    /// Store a chunk. Validates: blake3(data) == hash, len(data) <= CDC_MAX_CHUNK.
    async fn put_chunk(&self, hash: ChunkHash, data: Vec<u8>) -> Result<(), String>;

    /// Get a chunk by hash. Returns None if not found.
    async fn get_chunk(&self, hash: ChunkHash) -> Option<Vec<u8>>;

    /// Check if a chunk exists.
    async fn has_chunk(&self, hash: ChunkHash) -> bool;

    /// Batch existence check: given a list of chunk hashes, return the subset
    /// that the store does NOT have. Enables upload dedup — client only sends
    /// chunks the store is missing.
    /// Capped at MAX_MISSING_CHUNKS_BATCH hashes per call.
    async fn list_missing_chunks(&self, hashes: Vec<ChunkHash>) -> Vec<ChunkHash>;

    // =========================================================================
    // Node operations
    // =========================================================================

    /// Store a node. Validates: blake3(data) == hash (uniform addressing),
    /// well-formed CasNode, version prefix correct, data.len() <= MAX_NODE_BYTES.
    ///
    /// For BlobNode: validates sum(chunk.size) == total_size, chunks.len() in 2..=MAX_CHUNKS_PER_BLOB,
    /// each chunk.size in 1..=CDC_MAX_CHUNK, total_size > CDC_MAX_CHUNK, cdc_params_id known.
    ///
    /// For DirectoryNode: validates sorted, no dups, no empty/dot/dotdot/slash names,
    /// SmallFile.size <= CDC_MAX_CHUNK, LargeFile.size > CDC_MAX_CHUNK.
    ///
    /// Does NOT validate chunk existence — that's checked at finalization time.
    async fn put_node(&self, hash: NodeHash, data: Vec<u8>) -> Result<(), String>;

    /// Get a node by hash. Returns None if not found.
    async fn get_node(&self, hash: NodeHash) -> Option<Vec<u8>>;

    /// Check if a node exists.
    async fn has_node(&self, hash: NodeHash) -> bool;

    /// Batch existence check: given a list of node hashes, return the subset
    /// that the store does NOT have. Enables upload dedup for blob nodes,
    /// directory nodes, manifests, and other node payloads.
    /// Capped at MAX_MISSING_NODES_BATCH hashes per call.
    async fn list_missing_nodes(&self, hashes: Vec<NodeHash>) -> Vec<NodeHash>;

    // =========================================================================
    // Cache key operations
    // =========================================================================

    /// Look up a cache key, returns manifest node hash if found
    async fn lookup(&self, cache_key: CacheKey) -> Option<NodeHash>;

    /// Publish a cache entry atomically.
    /// Validates the object graph (finalization point).
    async fn publish(&self, cache_key: CacheKey, manifest_hash: NodeHash) -> PublishResult;

    /// Remove a cache key mapping. Chunks and nodes are left intact
    /// (they're content-addressed and harmless to keep).
    /// Used by ephemeral sessions to roll back cache entries on disconnect.
    async fn unpublish(&self, cache_key: CacheKey) -> bool;

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

    /// Extract a single Rust toolchain component tarball into a directory tree.
    async fn extract_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        tarball: BlobRef,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult;

    /// Finalize a Rust toolchain manifest from already extracted components.
    /// This is a finalization point — validates the entire object graph.
    async fn finalize_rust_toolchain(&self, spec: RustToolchainSpec) -> EnsureToolchainResult;

    /// Ensure a Zig toolchain exists in the store. Downloads if needed.
    /// Progress updates are streamed to `progress`.
    async fn ensure_zig_toolchain(
        &self,
        spec: ZigToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult;

    /// Get toolchain manifest by its node hash
    async fn get_toolchain_manifest(&self, manifest_hash: NodeHash) -> Option<ToolchainManifest>;

    /// Get materialization plan for a toolchain
    async fn get_materialization_plan(
        &self,
        manifest_hash: NodeHash,
    ) -> Option<MaterializationPlan>;

    /// Lookup manifest hash by toolchain spec key
    async fn lookup_toolchain_spec(&self, spec_key: ToolchainSpecKey) -> Option<NodeHash>;

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

    /// Get a registry crate manifest by its node hash
    async fn get_registry_manifest(&self, manifest_hash: NodeHash)
    -> Option<RegistryCrateManifest>;

    /// Lookup manifest hash by spec key
    async fn lookup_registry_spec(&self, spec_key: Blake3Hash) -> Option<NodeHash>;
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

/// Schema version 5: removed created_at from content-addressed manifest payload.
pub const TOOLCHAIN_MANIFEST_SCHEMA_VERSION: u32 = 5;

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
    // === Rust-specific fields ===
    /// Resolved manifest date (for stable/beta this captures the actual version)
    pub rust_manifest_date: Option<String>,
    /// Rustc version string
    pub rust_version: Option<String>,

    // === Zig-specific fields ===
    pub zig_version: Option<String>,

    /// Components stored as DirectoryNode trees
    pub components: Vec<ToolchainComponentTree>,
}

/// A toolchain component stored as a DirectoryNode tree
#[derive(Debug, Clone, Facet)]
pub struct ToolchainComponentTree {
    /// Component name: "rustc", "rust-std", "zig-exe", "zig-lib"
    pub name: String,
    /// Target triple (for rust-std)
    pub target: Option<String>,
    /// Hash of the root DirectoryNode for this component
    pub tree_root: NodeHash,
    /// Upstream SHA256 hex (for provenance/verification)
    pub sha256: String,
    /// Total size of all files in the tree
    pub total_size_bytes: u64,
    /// Number of files in the tree
    pub file_count: u32,
    /// Number of unique chunks (for dedup stats)
    pub unique_chunks: u32,
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

pub const MATERIALIZATION_LAYOUT_VERSION: u32 = 2;

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum MaterializeStep {
    /// Ensure a directory exists (for explicit dir creation).
    /// relpath must be non-empty; use "." for toolchain root if needed.
    EnsureDir { relpath: String },
    /// Create a symlink (target must not escape toolchain root)
    Symlink { relpath: String, target: String },
    /// Materialize a tree from a DirectoryNode
    MaterializeTree {
        /// Hash of the root DirectoryNode
        tree_root: NodeHash,
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

    /// Hash of the ToolchainManifest node in CAS
    pub manifest_hash: Option<NodeHash>,

    pub status: EnsureStatus,

    pub error: Option<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct EnsureRustToolchainComponentResult {
    /// None if spec was invalid and couldn't be hashed
    pub spec_key: Option<ToolchainSpecKey>,

    /// Component this result belongs to
    pub component: RustComponent,

    /// Reference to the downloaded component tarball
    pub tarball: Option<BlobRef>,

    /// Hash of the root DirectoryNode for the extracted component
    pub tree_root: Option<NodeHash>,

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
pub const REGISTRY_MANIFEST_SCHEMA_VERSION: u32 = 3;

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

    /// Reference to the .crate tarball bytes in CAS
    pub crate_tarball: BlobRef,
}

// Registry RPC Types

/// Result of ensuring a registry crate exists in CAS.
#[derive(Debug, Clone, Facet)]
pub struct EnsureRegistryCrateResult {
    /// The computed spec key (None if spec was invalid)
    pub spec_key: Option<RegistrySpecKey>,

    /// Hash of the RegistryCrateManifest node in CAS (None on failure)
    pub manifest_hash: Option<NodeHash>,

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
// Legacy Types — kept temporarily during migration
// =============================================================================

/// DEPRECATED: Use DirectoryNode instead.
/// A content-addressed directory tree stored in CAS (flat list format).
#[derive(Debug, Clone, Facet)]
pub struct TreeManifest {
    pub schema_version: u32,
    pub entries: Vec<TreeEntry>,
    pub total_size_bytes: u64,
    pub unique_blobs: u32,
}

pub const TREE_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// DEPRECATED: Use DirEntry instead.
#[derive(Debug, Clone, Facet)]
pub struct TreeEntry {
    pub path: String,
    pub kind: TreeEntryKind,
}

/// DEPRECATED: Use DirEntryKind instead.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum TreeEntryKind {
    File {
        blob: Blake3Hash,
        size: u64,
        executable: bool,
    },
    Symlink {
        target: String,
    },
    Directory,
}

impl Default for TreeManifest {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeManifest {
    pub fn new() -> Self {
        Self {
            schema_version: TREE_MANIFEST_SCHEMA_VERSION,
            entries: Vec::new(),
            total_size_bytes: 0,
            unique_blobs: 0,
        }
    }

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

    pub fn add_symlink(&mut self, path: String, target: String) {
        self.entries.push(TreeEntry {
            path,
            kind: TreeEntryKind::Symlink { target },
        });
    }

    pub fn add_directory(&mut self, path: String) {
        self.entries.push(TreeEntry {
            path,
            kind: TreeEntryKind::Directory,
        });
    }

    pub fn finalize(&mut self) {
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut seen = std::collections::HashSet::new();
        for entry in &self.entries {
            if let TreeEntryKind::File { blob, .. } = &entry.kind {
                seen.insert(*blob);
            }
        }
        self.unique_blobs = seen.len() as u32;
    }

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

    pub fn symlinks(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter_map(|e| match &e.kind {
            TreeEntryKind::Symlink { target } => Some((e.path.as_str(), target.as_str())),
            _ => None,
        })
    }

    pub fn directories(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|e| match &e.kind {
            TreeEntryKind::Directory => Some(e.path.as_str()),
            _ => None,
        })
    }
}

// Legacy types that are no longer needed but kept for compilation

/// DEPRECATED: Use chunk/node operations directly.
#[derive(Debug, Clone, Facet)]
pub struct TreeFile {
    pub path: String,
    pub contents: Vec<u8>,
    pub executable: bool,
}

/// DEPRECATED: Use chunk/node operations directly.
#[derive(Debug, Clone, Facet)]
pub struct IngestTreeRequest {
    pub files: Vec<TreeFile>,
}

/// DEPRECATED: Use chunk/node operations directly.
#[derive(Debug, Clone, Facet)]
pub struct IngestTreeResult {
    pub success: bool,
    pub manifest_hash: Option<ManifestHash>,
    pub file_count: u32,
    pub total_bytes: u64,
    pub new_blobs: u32,
    pub error: Option<String>,
}

// =============================================================================
// Tests
// =============================================================================

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
    // Newtype wrapper roundtrip tests
    // =========================================================================

    #[test]
    fn test_chunk_hash_roundtrip() {
        let hash = ChunkHash(Blake3Hash::from_bytes(b"chunk data"));
        let bytes = to_vec(&hash).expect("serialize ChunkHash");
        let decoded: ChunkHash = from_slice(&bytes).expect("deserialize ChunkHash");
        assert_eq!(decoded, hash);
    }

    #[test]
    fn test_node_hash_roundtrip() {
        let hash = NodeHash(Blake3Hash::from_bytes(b"node data"));
        let bytes = to_vec(&hash).expect("serialize NodeHash");
        let decoded: NodeHash = from_slice(&bytes).expect("deserialize NodeHash");
        assert_eq!(decoded, hash);
    }

    #[test]
    fn test_content_hash_roundtrip() {
        let hash = ContentHash(Blake3Hash::from_bytes(b"file content"));
        let bytes = to_vec(&hash).expect("serialize ContentHash");
        let decoded: ContentHash = from_slice(&bytes).expect("deserialize ContentHash");
        assert_eq!(decoded, hash);
    }

    // =========================================================================
    // CasNode serialization roundtrip tests
    // =========================================================================

    #[test]
    fn test_cas_node_blob_roundtrip() {
        let node = CasNode::Blob(BlobNode {
            chunks: vec![
                ChunkEntry {
                    hash: ChunkHash(Blake3Hash([1u8; 32])),
                    size: 2_000_000,
                },
                ChunkEntry {
                    hash: ChunkHash(Blake3Hash([2u8; 32])),
                    size: 1_500_000,
                },
            ],
            total_size: 3_500_000,
            content_hash: ContentHash(Blake3Hash([3u8; 32])),
            cdc_params_id: 1,
        });

        let bytes = node.to_bytes();
        let decoded = CasNode::from_bytes(&bytes).expect("deserialize CasNode::Blob");
        match decoded {
            CasNode::Blob(blob) => {
                assert_eq!(blob.chunks.len(), 2);
                assert_eq!(blob.total_size, 3_500_000);
                assert_eq!(blob.content_hash, ContentHash(Blake3Hash([3u8; 32])));
                assert_eq!(blob.cdc_params_id, 1);
            }
            other => panic!("expected Blob, got {other:?}"),
        }
    }

    #[test]
    // i[verify impl.tree]
    fn test_cas_node_directory_roundtrip() {
        let node = CasNode::Directory(DirectoryNode {
            entries: vec![
                DirEntry {
                    name: "README.md".to_string(),
                    kind: DirEntryKind::SmallFile {
                        content_hash: ContentHash(Blake3Hash([10u8; 32])),
                        size: 1024,
                        executable: false,
                    },
                },
                DirEntry {
                    name: "big_binary".to_string(),
                    kind: DirEntryKind::LargeFile {
                        content_hash: ContentHash(Blake3Hash([20u8; 32])),
                        blob_node: NodeHash(Blake3Hash([21u8; 32])),
                        size: 100_000_000,
                        executable: true,
                    },
                },
                DirEntry {
                    name: "link".to_string(),
                    kind: DirEntryKind::Symlink {
                        target: "../other".to_string(),
                    },
                },
                DirEntry {
                    name: "subdir".to_string(),
                    kind: DirEntryKind::Dir {
                        hash: NodeHash(Blake3Hash([30u8; 32])),
                    },
                },
            ],
            total_size: 100_051_024,
        });

        let bytes = node.to_bytes();
        let decoded = CasNode::from_bytes(&bytes).expect("deserialize CasNode::Directory");
        match decoded {
            CasNode::Directory(dir) => {
                assert_eq!(dir.entries.len(), 4);
                assert_eq!(dir.total_size, 100_051_024);
                assert_eq!(dir.entries[0].name, "README.md");
                assert_eq!(dir.entries[3].name, "subdir");
            }
            other => panic!("expected Directory, got {other:?}"),
        }
    }

    #[test]
    // i[verify impl.tree]
    fn directory_nodes_reference_subtrees_by_hash() {
        let child = CasNode::Directory(DirectoryNode {
            entries: vec![DirEntry {
                name: "lib.rs".to_string(),
                kind: DirEntryKind::SmallFile {
                    content_hash: ContentHash(Blake3Hash([0x11; 32])),
                    size: 123,
                    executable: false,
                },
            }],
            total_size: 123,
        });
        let (_, child_hash) = child.to_bytes_and_hash();

        let parent = CasNode::Directory(DirectoryNode {
            entries: vec![DirEntry {
                name: "src".to_string(),
                kind: DirEntryKind::Dir { hash: child_hash },
            }],
            total_size: 123,
        });

        let bytes = parent.to_bytes();
        let decoded = CasNode::from_bytes(&bytes).expect("deserialize parent directory");
        let CasNode::Directory(dir) = decoded else {
            panic!("expected Directory node");
        };
        let DirEntryKind::Dir { hash } = &dir.entries[0].kind else {
            panic!("expected child directory entry");
        };

        assert_eq!(*hash, child_hash);
    }

    #[test]
    fn test_cas_node_version_prefix() {
        let node = CasNode::Directory(DirectoryNode {
            entries: vec![],
            total_size: 0,
        });
        let bytes = node.to_bytes();

        // First 4 bytes should be CAS_NODE_VERSION in LE
        let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(version, CAS_NODE_VERSION);
    }

    #[test]
    fn test_cas_node_unknown_version() {
        let mut bytes = CasNode::Directory(DirectoryNode {
            entries: vec![],
            total_size: 0,
        })
        .to_bytes();

        // Corrupt version to 99
        bytes[0..4].copy_from_slice(&99u32.to_le_bytes());

        match CasNode::from_bytes(&bytes) {
            Err(CasNodeError::UnknownVersion(99)) => {}
            other => panic!("expected UnknownVersion(99), got {other:?}"),
        }
    }

    #[test]
    fn test_cas_node_too_short() {
        match CasNode::from_bytes(&[1, 2, 3]) {
            Err(CasNodeError::TooShort) => {}
            other => panic!("expected TooShort, got {other:?}"),
        }
    }

    #[test]
    fn test_cas_node_to_bytes_and_hash_determinism() {
        let node = CasNode::Directory(DirectoryNode {
            entries: vec![DirEntry {
                name: "test.txt".to_string(),
                kind: DirEntryKind::SmallFile {
                    content_hash: ContentHash(Blake3Hash([42u8; 32])),
                    size: 100,
                    executable: false,
                },
            }],
            total_size: 100,
        });

        let (bytes1, hash1) = node.to_bytes_and_hash();
        let (bytes2, hash2) = node.to_bytes_and_hash();
        assert_eq!(bytes1, bytes2);
        assert_eq!(hash1, hash2);

        // Hash is blake3 of the bytes
        let expected_hash = NodeHash(Blake3Hash(*blake3::hash(&bytes1).as_bytes()));
        assert_eq!(hash1, expected_hash);
    }

    // =========================================================================
    // BlobRef roundtrip tests
    // =========================================================================

    #[test]
    fn test_blob_ref_small_roundtrip() {
        let r = BlobRef::Small {
            chunk: ChunkHash(Blake3Hash([5u8; 32])),
            size: 1024,
        };
        let bytes = to_vec(&r).expect("serialize BlobRef::Small");
        let decoded: BlobRef = from_slice(&bytes).expect("deserialize BlobRef::Small");
        match decoded {
            BlobRef::Small { chunk, size } => {
                assert_eq!(chunk, ChunkHash(Blake3Hash([5u8; 32])));
                assert_eq!(size, 1024);
            }
            other => panic!("expected Small, got {other:?}"),
        }
    }

    #[test]
    fn test_blob_ref_large_roundtrip() {
        let r = BlobRef::Large {
            blob_node: NodeHash(Blake3Hash([6u8; 32])),
            content_hash: ContentHash(Blake3Hash([7u8; 32])),
            size: 100_000_000,
        };
        let bytes = to_vec(&r).expect("serialize BlobRef::Large");
        let decoded: BlobRef = from_slice(&bytes).expect("deserialize BlobRef::Large");
        match decoded {
            BlobRef::Large {
                blob_node,
                content_hash,
                size,
            } => {
                assert_eq!(blob_node, NodeHash(Blake3Hash([6u8; 32])));
                assert_eq!(content_hash, ContentHash(Blake3Hash([7u8; 32])));
                assert_eq!(size, 100_000_000);
            }
            other => panic!("expected Large, got {other:?}"),
        }
    }

    // =========================================================================
    // GOLDEN-BYTE TESTS: Serialization Stability
    // =========================================================================
    //
    // These tests pin exact serialized byte output for known inputs. If any test
    // fails, it means the serialization format changed — which invalidates ALL
    // existing nodes in the store (because hash = blake3(serialized_bytes)).
    //
    // If a test fails:
    //   (a) intentional: bump CAS_NODE_VERSION, update golden values, note what changed
    //   (b) accidental: revert whatever broke it

    #[test]
    fn golden_empty_directory_node() {
        let node = CasNode::Directory(DirectoryNode {
            entries: vec![],
            total_size: 0,
        });
        let bytes = node.to_bytes();
        let (_, hash) = node.to_bytes_and_hash();
        assert_eq!(
            hash.to_hex(),
            Blake3Hash::from_bytes(&bytes).to_hex(),
            "hash should be blake3 of serialized bytes"
        );
        assert_eq!(
            hex::encode(&bytes),
            "01000000010000",
            "Empty DirectoryNode golden bytes changed! Bump CAS_NODE_VERSION if intentional."
        );
        assert_eq!(
            hash.to_hex(),
            "a56f894b739937a2b8c1636a591f735bc34cbf401b7934d4bb3a5ba24f860ad7",
            "Empty DirectoryNode golden hash changed!"
        );
    }

    #[test]
    fn golden_small_file_dir_entry() {
        let entry = DirEntry {
            name: "hello.txt".to_string(),
            kind: DirEntryKind::SmallFile {
                content_hash: ContentHash(Blake3Hash([0xAA; 32])),
                size: 42,
                executable: false,
            },
        };
        let bytes = to_vec(&entry).expect("serialize DirEntry");
        assert_eq!(
            hex::encode(&bytes),
            "0968656c6c6f2e74787400aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2a00",
            "SmallFile DirEntry golden bytes changed!"
        );
    }

    #[test]
    fn golden_blob_node() {
        let node = CasNode::Blob(BlobNode {
            chunks: vec![
                ChunkEntry {
                    hash: ChunkHash(Blake3Hash([0xBB; 32])),
                    size: 2_000_000,
                },
                ChunkEntry {
                    hash: ChunkHash(Blake3Hash([0xCC; 32])),
                    size: 1_500_000,
                },
            ],
            total_size: 3_500_000,
            content_hash: ContentHash(Blake3Hash([0xDD; 32])),
            cdc_params_id: 1,
        });
        let bytes = node.to_bytes();
        assert_eq!(
            hex::encode(&bytes),
            "010000000002bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb80897acccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccce0c65be0cfd501dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd01",
            "BlobNode golden bytes changed! Bump CAS_NODE_VERSION if intentional."
        );
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
