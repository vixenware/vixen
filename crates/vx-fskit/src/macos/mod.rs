mod extension_runtime;
mod probe;
mod process;
mod setup_impl;
mod teardown_impl;

pub use setup_impl::setup;
pub use teardown_impl::teardown;

use camino::Utf8PathBuf;
use jiff::Zoned;

/// Result of a setup() or teardown() operation.
pub struct FsKitResult {
    /// Chronological log of everything that happened.
    pub log: Vec<LogEntry>,

    /// None on success, Some on failure with suggested remediation.
    pub error: Option<FsKitError>,
}

impl FsKitResult {
    fn success(log: Vec<LogEntry>) -> Self {
        Self { log, error: None }
    }

    fn failure(log: Vec<LogEntry>, summary: impl Into<String>, remediation: Vec<String>) -> Self {
        Self {
            log,
            error: Some(FsKitError {
                summary: summary.into(),
                remediation,
            }),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

pub struct LogEntry {
    pub timestamp: Zoned,
    pub message: String,
    pub level: LogLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// A check passed successfully.
    Ok,
    /// Neutral/informational step.
    Info,
    /// Something failed.
    Fail,
}

pub struct FsKitError {
    /// What went wrong.
    pub summary: String,
    /// What the user can try manually.
    pub remediation: Vec<String>,
}

/// Status payload served by the extension at `/.vixenfs/status.json`.
#[derive(Debug, facet::Facet)]
#[facet(rename_all = "snake_case")]
pub struct StatusPayload {
    pub connected: bool,

    #[facet(default)]
    pub bundle_version: Option<String>,

    #[facet(default)]
    pub ever_connected: bool,

    #[facet(default)]
    pub app_group_path: Option<String>,

    #[facet(default)]
    pub connected_at: Option<String>,

    #[facet(default)]
    pub disconnected_at: Option<String>,

    #[facet(default)]
    pub uptime_seconds: Option<f64>,

    #[facet(default)]
    pub disconnect_reason: Option<String>,
}

/// Accumulates log entries with timestamps.
pub(crate) struct Log {
    pub entries: Vec<LogEntry>,
}

impl Log {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn ok(&mut self, message: impl Into<String>) {
        self.push(message, LogLevel::Ok);
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.push(message, LogLevel::Info);
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.push(message, LogLevel::Fail);
    }

    fn push(&mut self, message: impl Into<String>, level: LogLevel) {
        let msg = message.into();
        tracing::debug!("{msg}");
        self.entries.push(LogEntry {
            timestamp: Zoned::now(),
            message: msg,
            level,
        });
    }

    pub fn into_entries(self) -> Vec<LogEntry> {
        self.entries
    }
}

const VIXEN_APP_PATH: &str = "/Applications/Vixen.app";
const VIXEN_EXTENSION_IDENTIFIER: &str = "eu.bearcove.vixenfs";

/// Build the set of candidate paths for a mount point.
///
/// Dead FSKit mounts don't always show up under the same path. macOS has
/// `/tmp` → `/private/tmp` symlinks, and `lsof`/`df` may report either form.
async fn mount_path_candidates(mount_path: &Utf8PathBuf) -> Vec<String> {
    let canonical = canonicalize_without_stat(mount_path).await;
    let mut candidates: Vec<String> = vec![canonical.to_string()];
    let raw = mount_path.to_string();
    if raw != candidates[0] {
        candidates.push(raw);
    }
    // /tmp → /private/tmp on macOS
    let private_prefixed = format!("/private{mount_path}");
    if !candidates.contains(&private_prefixed) {
        candidates.push(private_prefixed);
    }
    candidates
}

/// Resolve the canonical form of a mount path without touching the mount point
/// itself — which may be a dead FSKit mount that hangs on any access.
async fn canonicalize_without_stat(mount_path: &Utf8PathBuf) -> Utf8PathBuf {
    if let Some(parent) = mount_path.parent()
        && let Ok(canonical_parent) = moire::fs::canonicalize(parent.as_std_path()).await
        && let Ok(canonical_parent) = Utf8PathBuf::try_from(canonical_parent)
        && let Some(name) = mount_path.file_name()
    {
        return canonical_parent.join(name);
    }
    mount_path.clone()
}
