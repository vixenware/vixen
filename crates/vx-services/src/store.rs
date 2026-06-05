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

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};

use facet::Facet;

// =============================================================================
// Protocol Constants
// =============================================================================

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
pub struct Blake3Hash(pub [u8; 32]);

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
    CacheRecord(CacheRecordNode),
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
// Cache records
// =============================================================================

/// Digest of the pre-run cache key. The key material is computed before running;
/// read witnesses live in the record body and never become the primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Facet)]
#[repr(transparent)]
pub struct CacheInputKeyDigest(pub Blake3Hash);

/// A portable cache record stored as a CAS node.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct CacheRecordNode {
    pub namespace: String,
    pub input_key: CacheInputKeyDigest,
    pub body: CacheRecordBody,
    /// Future read-set/witness payload. A witness is post-run provenance; it is
    /// not part of the pre-run key.
    pub read_witness: Option<NodeHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum CacheRecordBody {
    Value(CacheRecordValue),
    /// Transitional payload for record bodies whose portable schema is still
    /// being carved out. The namespace and format string must identify the
    /// decoder.
    Opaque {
        format: String,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum CacheRecordValue {
    U64(u64),
    Bool(bool),
    String(String),
    Unit,
    Tree(NodeHash),
}

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
    pub entries: Vec<DirectoryEntry>,
    /// Recursive sum of file content bytes in this subtree.
    /// Symlinks contribute 0. For progress reporting only.
    /// Validated at finalization time (not on put_node).
    pub total_size: u64,
}

/// A single entry in a DirectoryNode.
#[derive(Debug, Clone, Facet)]
pub struct DirectoryEntry {
    /// Filename only (not full path). Must be valid UTF-8.
    /// Must not be empty, ".", "..", or contain "/".
    pub name: String,
    /// Entry kind and associated data.
    pub kind: DirectoryEntryKind,
}

/// Kind of directory entry. The type explicitly encodes whether a file is
/// single-chunk or multi-chunk, eliminating any "try chunks/ then nodes/" guessing.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum DirectoryEntryKind {
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
    pub fn to_dir_entry_kind(&self, executable: bool) -> DirectoryEntryKind {
        match self {
            BlobRef::Small { chunk, size } => DirectoryEntryKind::SmallFile {
                content_hash: ContentHash(chunk.0),
                size: *size,
                executable,
            },
            BlobRef::Large {
                blob_node,
                content_hash,
                size,
            } => DirectoryEntryKind::LargeFile {
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

// =============================================================================
// Store Service Trait
// =============================================================================

/// CAS service trait.
///
/// The store is a validated key-value store accepting chunk-shaped and
/// node-shaped requests. All chunking, assembly, and Merkle tree construction
/// logic lives in shared library code used by clients.
#[vox::service]
#[allow(async_fn_in_trait)]
pub trait Store {
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
}
