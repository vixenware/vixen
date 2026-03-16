//! vxd protocol definitions
//!
//! The daemon is the brain of vx. It:
//! - Owns the picante incremental computation runtime
//! - Parses manifests and tracks source files
//! - Computes cache keys
//! - Orchestrates builds via CAS and Exec services
//!
//! The CLI is a thin client that just asks the daemon to do things.

use std::fmt;

use camino::Utf8PathBuf;
use facet::Facet;
use vx_store_proto::CacheKey;

/// Daemon protocol version.
/// Bump this when making breaking changes to the daemon RPC interface.
pub const VXD_PROTOCOL_VERSION: u32 = 5;

/// Request to build a project
#[derive(Debug, Clone, Facet)]
pub struct BuildRequest {
    /// Path to the project directory (containing Cargo.toml)
    pub project_path: Utf8PathBuf,
    /// Optional explicit build file to evaluate instead of `build.vx`
    pub build_file_path: Option<Utf8PathBuf>,
    /// Build in release mode
    pub release: bool,
}

/// Result of a build
#[derive(Clone, Facet)]
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
    /// Structured build failure details (if available)
    pub failure: Option<BuildFailure>,
    /// Total number of actions in the build graph
    pub total_actions: usize,
    /// Number of actions that were cache hits
    pub cache_hits: usize,
    /// Number of actions that were actually executed
    pub rebuilt: usize,
    /// Number of actions that were cancelled
    pub cancelled: usize,
}

/// Structured details for a user-facing build failure.
#[derive(Clone, PartialEq, Eq, Facet)]
pub struct BuildFailure {
    /// One-line fallback summary.
    pub message: String,
    /// Source texts referenced by the rendered reports.
    pub sources: Vec<BuildDiagnosticSource>,
    /// Reports to render in order.
    pub reports: Vec<BuildDiagnosticReport>,
}

/// A source file referenced by structured diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct BuildDiagnosticSource {
    /// Stable source identifier, usually a file path.
    pub source_id: String,
    /// Optional language identifier used for syntax highlighting.
    pub language: Option<String>,
    /// Full source text.
    pub text: String,
}

/// One Ariadne-style report within a build failure.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct BuildDiagnosticReport {
    /// Severity / visual treatment.
    pub severity: BuildDiagnosticSeverity,
    /// Report headline.
    pub message: String,
    /// Primary span for the report.
    pub primary_span: BuildDiagnosticSpan,
    /// Labels shown inside the snippet.
    pub labels: Vec<BuildDiagnosticLabel>,
    /// Additional notes shown under the snippet.
    pub notes: Vec<String>,
}

/// Severity of a build diagnostic report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum BuildDiagnosticSeverity {
    Error,
    Advice,
}

/// A source span stored in byte offsets.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct BuildDiagnosticSpan {
    pub source_id: String,
    pub start: u32,
    pub end: u32,
}

/// A labeled span inside a report.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct BuildDiagnosticLabel {
    pub span: BuildDiagnosticSpan,
    pub kind: BuildDiagnosticLabelKind,
    pub message: Option<String>,
}

/// Label role used by the CLI renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum BuildDiagnosticLabelKind {
    Primary,
    Secondary,
}

impl fmt::Display for BuildResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.success {
            write!(f, "{}", self.message)?;
            if let Some(output_path) = &self.output_path {
                write!(f, "\noutput: {output_path}")?;
            }
            if self.cached {
                write!(f, "\ncache: hit")?;
            } else if self.duration_ms > 0 {
                write!(f, "\nduration_ms: {}", self.duration_ms)?;
            }
            return Ok(());
        }

        if let Some(failure) = &self.failure {
            return fmt::Display::fmt(failure, f);
        }

        if let Some(error) = &self.error {
            return f.write_str(error);
        }

        f.write_str(&self.message)
    }
}

impl fmt::Debug for BuildResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        for report in &self.reports {
            write!(
                f,
                "\n\n{}: {}",
                severity_name(report.severity),
                report.message
            )?;
            write!(
                f,
                "\n  at {}:{}-{}",
                report.primary_span.source_id, report.primary_span.start, report.primary_span.end
            )?;
            for message in report
                .labels
                .iter()
                .filter_map(|label| label.message.as_deref())
            {
                write!(f, "\n  {message}")?;
            }
            for note in &report.notes {
                write!(f, "\n  note: {}", indent_note(note))?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for BuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

fn severity_name(severity: BuildDiagnosticSeverity) -> &'static str {
    match severity {
        BuildDiagnosticSeverity::Error => "error",
        BuildDiagnosticSeverity::Advice => "advice",
    }
}

fn indent_note(note: &str) -> String {
    note.replace('\n', "\n        ")
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
    /// Running a generic tool
    Run(String), // label
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
            ActionType::Run(label) => format!("run {}", label),
            ActionType::IngestSource(name) => format!("ingest {}", name),
        }
    }
}

/// Result of unpublishing cache keys
#[derive(Debug, Clone, Facet)]
pub struct UnpublishResult {
    /// Number of cache entries that were successfully removed
    pub removed: u32,
    /// Number of cache keys that were not found (already absent)
    pub not_found: u32,
}

#[derive(Debug, Clone, Facet)]
pub struct PsEntry {
    pub id: u64,
    pub kind: String,
    pub label: String,
    pub executor: Option<String>,
    pub age_ms: u64,
    pub idle_ms: u64,
    pub last_message: Option<String>,
    pub remote_idle_ms: Option<u64>,
    pub last_remote_message: Option<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct PsResult {
    pub entries: Vec<PsEntry>,
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

    /// Remove cache key mappings from the store. Used by `vx clean --cache`
    /// to invalidate specific cache entries without deleting blobs or manifests.
    async fn unpublish_cache_keys(&self, cache_keys: Vec<CacheKey>) -> UnpublishResult;

    /// Snapshot daemon-side in-flight activity.
    async fn ps(&self) -> PsResult;
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
    fn build_failure_debug_uses_display_format() {
        let failure = BuildFailure {
            message: "build evaluation failed".to_string(),
            sources: Vec::new(),
            reports: vec![BuildDiagnosticReport {
                severity: BuildDiagnosticSeverity::Error,
                message: "scheduler stalled".to_string(),
                primary_span: BuildDiagnosticSpan {
                    source_id: "build.vx".to_string(),
                    start: 10,
                    end: 20,
                },
                labels: vec![BuildDiagnosticLabel {
                    span: BuildDiagnosticSpan {
                        source_id: "build.vx".to_string(),
                        start: 10,
                        end: 20,
                    },
                    kind: BuildDiagnosticLabelKind::Primary,
                    message: Some("this expression failed".to_string()),
                }],
                notes: vec!["build.vx exec[class:default]: cc -c foo.c".to_string()],
            }],
        };

        assert_eq!(format!("{failure}"), format!("{failure:?}"));
    }

    #[test]
    fn build_result_debug_uses_display_format() {
        let result = BuildResult {
            success: false,
            message: "build failed".to_string(),
            cached: false,
            duration_ms: 0,
            output_path: None,
            error: None,
            failure: Some(BuildFailure {
                message: "build evaluation failed".to_string(),
                sources: Vec::new(),
                reports: Vec::new(),
            }),
            total_actions: 1,
            cache_hits: 0,
            rebuilt: 1,
            cancelled: 0,
        };

        assert_eq!(format!("{result}"), format!("{result:?}"));
    }

    #[test]
    fn test_build_result_roundtrip() {
        let result = BuildResult {
            success: true,
            message: "done".to_string(),
            cached: false,
            duration_ms: 100,
            output_path: Some(Utf8PathBuf::from("/output/binary")),
            error: None,
            failure: None,
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
            build_file_path: Some(Utf8PathBuf::from("/home/user/project/lua.vx")),
            release: true,
        };

        let bytes = to_vec(&request).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: BuildRequest = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded.project_path, request.project_path);
        assert_eq!(decoded.build_file_path, request.build_file_path);
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
        output_path: Option<Utf8PathBuf>,
        error: Option<String>,
        failure: Option<BuildFailure>,
        total_actions: u64,
        cache_hits: u64,
        rebuilt: u64,
        cancelled: u64,
    }

    // With usize
    #[derive(Debug, Clone, PartialEq, Facet)]
    struct MirrorBuildResult {
        success: bool,
        message: String,
        cached: bool,
        duration_ms: u64,
        output_path: Option<Utf8PathBuf>,
        error: Option<String>,
        failure: Option<BuildFailure>,
        total_actions: usize,
        cache_hits: usize,
        rebuilt: usize,
        cancelled: usize,
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
            failure: None,
            total_actions: 5,
            cache_hits: 2,
            rebuilt: 3,
            cancelled: 0,
        };

        let bytes = to_vec(&value).expect("serialize");
        println!("Serialized {} bytes: {:?}", bytes.len(), bytes);

        let decoded: MirrorBuildResult = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, value);
    }

    #[test]
    fn test_build_failure_roundtrip() {
        let failure = BuildFailure {
            message: "build input not found".to_string(),
            sources: vec![BuildDiagnosticSource {
                source_id: "build.vx".to_string(),
                language: Some("vixen".to_string()),
                text: "fn build()".to_string(),
            }],
            reports: vec![BuildDiagnosticReport {
                severity: BuildDiagnosticSeverity::Error,
                message: "build input not found".to_string(),
                primary_span: BuildDiagnosticSpan {
                    source_id: "build.vx".to_string(),
                    start: 3,
                    end: 8,
                },
                labels: vec![BuildDiagnosticLabel {
                    span: BuildDiagnosticSpan {
                        source_id: "build.vx".to_string(),
                        start: 3,
                        end: 8,
                    },
                    kind: BuildDiagnosticLabelKind::Primary,
                    message: Some("missing input".to_string()),
                }],
                notes: vec!["resolved path: /workspace/input".to_string()],
            }],
        };

        let bytes = to_vec(&failure).expect("serialize");
        let decoded: BuildFailure = from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, failure);
    }
}
