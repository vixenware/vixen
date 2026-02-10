//! vxd protocol definitions
//!
//! The daemon is the brain of vx. It:
//! - Owns the picante incremental computation runtime
//! - Parses manifests and tracks source files
//! - Computes cache keys
//! - Orchestrates builds via CAS and Exec services
//!
//! The CLI is a thin client that just asks the daemon to do things.

use camino::Utf8PathBuf;
use facet::Facet;

/// Daemon protocol version.
/// Bump this when making breaking changes to the daemon RPC interface.
pub const VXD_PROTOCOL_VERSION: u32 = 2;

/// Request to build a project
#[derive(Debug, Clone, Facet)]
pub struct BuildRequest {
    /// Path to the project directory (containing Cargo.toml)
    pub project_path: Utf8PathBuf,
    /// Build in release mode
    pub release: bool,
}

/// Result of a build
#[derive(Debug, Clone, Facet)]
pub struct BuildResult {
    /// Whether the build succeeded
    pub success: bool,
    /// Human-readable status message
    pub message: String,
    /// Whether this was a cache hit
    pub cached: bool,
    /// Build duration in milliseconds (0 if cached)
    pub duration_ms: u64,
    /// Path to the output binary (if successful)
    pub output_path: Option<Utf8PathBuf>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Total number of actions in the build graph
    pub total_actions: usize,
    /// Number of actions that were cache hits
    pub cache_hits: usize,
    /// Number of actions that were actually executed
    pub rebuilt: usize,
    /// Number of actions that were cancelled
    pub cancelled: usize,
}

/// Action type being tracked
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
#[repr(C)]
pub enum ActionType {
    /// Compiling a Rust crate
    CompileRust(String),
    /// Acquiring full Rust toolchain (legacy single action mode)
    AcquireToolchain,
    /// Downloading one Rust toolchain component
    DownloadRustToolchainComponent(String), // component name
    /// Extracting one Rust toolchain component
    ExtractRustToolchainComponent(String), // component name
    /// Acquiring Zig toolchain
    AcquireZigToolchain(String), // version
    /// Acquiring registry crate
    AcquireRegistryCrate(String, String), // name, version
    /// Compiling a C source file
    CompileC(String), // source path
    /// Linking C binary
    LinkC(String), // binary name
    /// Ingesting source tree to CAS
    IngestSource(String),
}

impl ActionType {
    /// Get a display name for this action
    pub fn display_name(&self) -> String {
        match self {
            ActionType::CompileRust(name) => format!("compile {}", name),
            ActionType::AcquireToolchain => "acquire toolchain".to_string(),
            ActionType::DownloadRustToolchainComponent(component) => {
                format!("download toolchain {}", component)
            }
            ActionType::ExtractRustToolchainComponent(component) => {
                format!("extract toolchain {}", component)
            }
            ActionType::AcquireZigToolchain(version) => format!("acquire zig {}", version),
            ActionType::AcquireRegistryCrate(name, version) => {
                format!("acquire {}@{}", name, version)
            }
            ActionType::CompileC(source) => format!("compile {}", source),
            ActionType::LinkC(name) => format!("link {}", name),
            ActionType::IngestSource(name) => format!("ingest {}", name),
        }
    }
}

/// The daemon service trait
#[roam::service]
pub trait Vxd {
    /// Get service version information (for health checks and compatibility)
    async fn version(&self) -> vx_store_proto::ServiceVersion;

    /// Build a project
    async fn build(&self, request: BuildRequest) -> BuildResult;

    /// Set a session ID for cache key namespacing. When set, all cache keys
    /// computed during this connection include the session ID, providing
    /// isolation between concurrent sessions (e.g., parallel tests).
    /// An empty string means "session zero" (shared/global namespace).
    async fn set_session(&self, session_id: String);

    /// Shutdown the daemon gracefully
    async fn shutdown(&self);
}

/// Progress update for a running action.
///
/// When `current` and `total` are both set, the TUI renders a progress bar
/// with percentage. When only `message` is set, it's shown as a status string.
#[derive(Debug, Clone, Facet)]
pub struct ActionProgress {
    /// Human-readable phase description (e.g., "downloading rustc", "extracting")
    pub message: String,
    /// Bytes or items completed so far
    pub current: Option<u64>,
    /// Total bytes or items expected
    pub total: Option<u64>,
}

/// Progress reporting service (implemented by vx CLI, called by daemon)
#[roam::service]
pub trait ProgressListener {
    /// Set the total number of actions
    async fn set_total(&self, total: u64);

    /// Start tracking an action, returning its ID
    async fn start_action(&self, action_type: ActionType) -> u64;

    /// Update progress on a running action (download bytes, extraction files, etc.)
    async fn update_progress(&self, action_id: u64, progress: ActionProgress);

    /// Mark an action as completed
    async fn complete_action(&self, action_id: u64);

    /// Add a log message
    async fn log_message(&self, message: String);
}

#[cfg(test)]
mod tests {
    use super::*;
    use facet_postcard::{from_slice, to_vec};

    #[test]
    fn test_build_result_roundtrip() {
        let result = BuildResult {
            success: true,
            message: "done".to_string(),
            cached: false,
            duration_ms: 100,
            output_path: Some(Utf8PathBuf::from("/output/binary")),
            error: None,
            total_actions: 5,
            cache_hits: 2,
            rebuilt: 3,
            cancelled: 0,
        };

        let bytes = to_vec(&result).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: BuildResult = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded.success, result.success);
        assert_eq!(decoded.message, result.message);
        assert_eq!(decoded.output_path, result.output_path);
    }

    #[test]
    fn test_build_request_roundtrip() {
        let request = BuildRequest {
            project_path: Utf8PathBuf::from("/home/user/project"),
            release: true,
        };

        let bytes = to_vec(&request).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: BuildRequest = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded.project_path, request.project_path);
        assert_eq!(decoded.release, request.release);
    }

    #[test]
    fn test_option_utf8pathbuf_some() {
        let value: Option<Utf8PathBuf> = Some(Utf8PathBuf::from("/path"));

        let bytes = to_vec(&value).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: Option<Utf8PathBuf> = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, value);
    }

    #[test]
    fn test_option_utf8pathbuf_none() {
        let value: Option<Utf8PathBuf> = None;

        let bytes = to_vec(&value).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: Option<Utf8PathBuf> = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, value);
    }

    // Minimal struct to reproduce the issue
    #[derive(Debug, Clone, PartialEq, Facet)]
    struct SimpleWithOptionalPath {
        value: u32,
        path: Option<Utf8PathBuf>,
    }

    #[test]
    fn test_simple_struct_with_optional_path() {
        let value = SimpleWithOptionalPath {
            value: 42,
            path: Some(Utf8PathBuf::from("/path")),
        };

        let bytes = to_vec(&value).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: SimpleWithOptionalPath = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, value);
    }

    // Mirror BuildResult structure but with u64 instead of usize
    #[derive(Debug, Clone, PartialEq, Facet)]
    struct MirrorBuildResultU64 {
        success: bool,
        message: String,
        cached: bool,
        duration_ms: u64,
        output_path: Option<Utf8PathBuf>, // Field 5 of 9
        error: Option<String>,
        total_actions: u64,
        cache_hits: u64,
        rebuilt: u64,
    }

    // With usize
    #[derive(Debug, Clone, PartialEq, Facet)]
    struct MirrorBuildResult {
        success: bool,
        message: String,
        cached: bool,
        duration_ms: u64,
        output_path: Option<Utf8PathBuf>, // Field 5 of 9
        error: Option<String>,
        total_actions: usize,
        cache_hits: usize,
        rebuilt: usize,
    }

    #[test]
    fn test_action_progress_roundtrip() {
        let progress = ActionProgress {
            message: "downloading rustc".to_string(),
            current: Some(82_000_000),
            total: Some(158_000_000),
        };

        let bytes = to_vec(&progress).expect("serialize");
        let decoded: ActionProgress = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded.message, progress.message);
        assert_eq!(decoded.current, progress.current);
        assert_eq!(decoded.total, progress.total);

        // Also test with None values
        let progress_indeterminate = ActionProgress {
            message: "extracting".to_string(),
            current: None,
            total: None,
        };
        let bytes = to_vec(&progress_indeterminate).expect("serialize");
        let decoded: ActionProgress = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded.message, "extracting");
        assert_eq!(decoded.current, None);
        assert_eq!(decoded.total, None);
    }

    #[test]
    fn test_mirror_build_result() {
        let value = MirrorBuildResult {
            success: true,
            message: "done".to_string(),
            cached: false,
            duration_ms: 100,
            output_path: Some(Utf8PathBuf::from("/output/binary")),
            error: None,
            total_actions: 5,
            cache_hits: 2,
            rebuilt: 3,
        };

        let bytes = to_vec(&value).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: MirrorBuildResult = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, value);
    }
}
