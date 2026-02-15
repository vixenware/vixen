use camino::Utf8PathBuf;
use facet::Facet;
use std::time::Duration;
use vx_vfs_types::VfsDiagnostic;

/// Configuration for starting a VFS mount session.
#[derive(Debug, Clone)]
pub struct VfsConfig {
    /// Filesystem path where the VFS should be mounted.
    pub mount_path: Utf8PathBuf,
}

/// Successful VFS start outcomes.
pub enum VfsStart {
    /// VFS is mounted and ready at the given path.
    Mounted {
        mount_path: Utf8PathBuf,
        /// Becomes `true` when the VFS session dies unexpectedly.
        vfs_dead: peeps::WatchReceiver<bool>,
        /// FUSE telemetry state for diagnostics (Linux only).
        #[cfg(target_os = "linux")]
        telemetry: fuse3::telemetry::TelemetryState,
    },
    /// VFS is unavailable on this platform.
    Unsupported { platform: String },
}

impl std::fmt::Debug for VfsStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mounted { mount_path, .. } => f
                .debug_struct("Mounted")
                .field("mount_path", mount_path)
                .finish_non_exhaustive(),
            Self::Unsupported { platform } => f
                .debug_struct("Unsupported")
                .field("platform", platform)
                .finish(),
        }
    }
}

/// Errors returned while starting or managing VFS.
#[derive(Debug, Facet)]
#[facet(derive(Error))]
#[repr(u8)]
pub enum VfsError {
    /// command timed out: {program} after {timeout:?}
    CommandTimedOut {
        program: &'static str,
        timeout: Duration,
    },
    /// failed to execute {program}: {message}
    CommandFailed {
        program: &'static str,
        message: String,
        _diagnostics: Vec<VfsDiagnostic>,
    },
    /// VFS mount failed: {message}
    MountFailed {
        message: String,
        diagnostics: Vec<VfsDiagnostic>,
    },
}

impl VfsError {
    pub fn diagnostics(&self) -> &[VfsDiagnostic] {
        match self {
            VfsError::CommandTimedOut { .. } => &[],
            VfsError::CommandFailed { _diagnostics, .. } => _diagnostics,
            VfsError::MountFailed { diagnostics, .. } => diagnostics,
        }
    }
}
