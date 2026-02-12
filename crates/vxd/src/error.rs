//! Error types for vxd.

use camino::Utf8PathBuf;
use facet::Facet;
use thiserror::Error;
use vx_store_proto::NodeHash;

/// Errors that can occur during daemon operations.
#[derive(Debug, Clone, Error, Facet)]
#[repr(u8)]
pub enum VxdError {
    // === RPC Errors ===
    #[error("CAS RPC error: {0}")]
    CasRpc(String),

    #[error("Exec RPC error: {0}")]
    ExecRpc(String),

    // === Toolchain Errors ===
    #[error("toolchain acquisition failed: {0}")]
    ToolchainAcquisition(String),

    #[error("no manifest hash returned from toolchain acquisition")]
    NoManifestHash,

    #[error("no toolchain id returned from toolchain acquisition")]
    NoToolchainId,

    #[error("toolchain manifest not found: {0}")]
    ToolchainManifestNotFound(NodeHash),

    // === Registry Crate Errors ===
    #[error("registry crate {name}@{version} acquisition failed: {reason}")]
    RegistryCrateAcquisition {
        name: String,
        version: String,
        reason: String,
    },

    #[error("registry crate {name}@{version}: no manifest hash returned")]
    RegistryCrateNoManifest { name: String, version: String },

    // === Graph/Module Errors ===
    #[error("failed to build crate graph: {0}")]
    CrateGraph(String),

    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("failed to compute source closure for {crate_name}: {message}")]
    SourceClosure { crate_name: String, message: String },

    #[error("failed to hash source closure: {0}")]
    SourceHash(String),

    // === IO Errors ===
    #[error("failed to read {path}: {message}")]
    FileRead { path: Utf8PathBuf, message: String },

    #[error("failed to create directory {path}: {message}")]
    CreateDir { path: Utf8PathBuf, message: String },

    #[error("failed to write output to {path}: {message}")]
    WriteOutput { path: Utf8PathBuf, message: String },

    // === CAS Errors ===
    #[error("tree ingestion failed: {0}")]
    TreeIngestion(String),

    #[error("output manifest not found: {0}")]
    OutputManifestNotFound(NodeHash),

    #[error("blob not found: {0}")]
    BlobNotFound(String),

    // === Compilation Errors ===
    #[error("compilation failed for {crate_name}: {message}")]
    Compilation { crate_name: String, message: String },

    #[error("no output manifest returned for {crate_name}")]
    NoOutputManifest { crate_name: String },

    // === Dependency Errors ===
    #[error("dependency {dep_name} not yet compiled for {crate_name}")]
    DependencyNotCompiled {
        dep_name: String,
        crate_name: String,
    },

    #[error("dependency node not found: {dep_name}")]
    DependencyNodeNotFound { dep_name: String },

    // === Cancellation ===
    #[error("action cancelled")]
    Cancelled,

    // === Cache/Picante Errors ===
    #[error("picante error: {0}")]
    Picante(String),

    #[error("cache key computation failed: {0}")]
    CacheKey(String),
}

/// Result type for daemon operations.
pub type Result<T> = std::result::Result<T, VxdError>;
