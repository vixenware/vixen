use camino::Utf8PathBuf;
use facet::Facet;
use std::time::Duration;

/// Configuration for starting a VFS mount session.
#[derive(Debug, Clone)]
pub struct VfsConfig {
    /// Filesystem path where the VFS should be mounted.
    pub mount_path: Utf8PathBuf,
}

/// Successful VFS start outcomes.
#[derive(Debug, Clone)]
pub enum VfsStart {
    /// VFS is mounted and ready at the given path.
    Mounted { mount_path: Utf8PathBuf },
    /// VFS is unavailable on this platform.
    Unsupported { platform: String },
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
    },
}
