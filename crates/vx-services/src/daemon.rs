//! vxd protocol definitions
//!
//! This defines the daemon RPC surface only.
//!
//! During the evaluator purge, `vxd` is intentionally a reduced daemon shell.
//! The protocol still carries build-shaped requests and progress/reporting
//! structures because the CLI depends on them, but the daemon no longer claims
//! to own the old picante/eval/runtime architecture.

use std::fmt;

use camino::Utf8PathBuf;
use facet::Facet;

/// Daemon protocol version.
/// Bump this when making breaking changes to the daemon RPC interface.
pub const VXD_PROTOCOL_VERSION: u32 = 6;

/// Request to build a project.
///
/// This remains the CLI-facing build entrypoint even while the daemon-side
/// build implementation is being rebuilt.
#[derive(Debug, Clone, Facet)]
pub struct BuildRequest {
    /// Path to the project directory (containing Cargo.toml)
    pub project_path: Utf8PathBuf,
    /// Optional explicit build file to evaluate instead of `build.vx`
    pub build_file_path: Option<Utf8PathBuf>,
    /// Build in release mode
    pub release: bool,
    /// Extra arguments passed to the build entry function
    pub args: Vec<String>,
    /// Explicit environment variables passed to the build entry function
    pub env: Vec<(String, String)>,
}

impl BuildRequest {
    pub fn new(project_path: impl Into<Utf8PathBuf>) -> Self {
        Self {
            project_path: project_path.into(),
            build_file_path: None,
            release: false,
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    pub fn build_file_path(mut self, path: impl Into<Utf8PathBuf>) -> Self {
        self.build_file_path = Some(path.into());
        self
    }

    pub fn release(mut self, release: bool) -> Self {
        self.release = release;
        self
    }

    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }
}

/// Result of a build request.
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

/// The daemon service trait.
#[vox::service]
pub trait VxDaemon {
    // TODO: eval?
}
