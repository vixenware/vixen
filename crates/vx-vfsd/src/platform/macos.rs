use camino::Utf8PathBuf;

use crate::Result;

pub async fn cleanup_stuck_mount(_mount_path: &Utf8PathBuf) -> Result<()> {
    // Noop on macOS — vx_fskit::setup() handles all pre-mount cleanup.
    Ok(())
}

/// Diagnostics now come from vx-fskit's structured log entries.
pub async fn collect_vfs_diagnostics(_mount_path: &Utf8PathBuf) -> String {
    String::new()
}

/// Whether we should create the mount directory.
/// On macOS, /Volumes is system-managed — don't create it ourselves.
pub fn should_create_mount_dir(_mount_path: &Utf8PathBuf) -> bool {
    // vx_fskit::setup() handles mkdir on macOS.
    false
}
