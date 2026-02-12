use camino::Utf8PathBuf;

use crate::Result;

pub async fn cleanup_stuck_mount(_mount_path: &Utf8PathBuf) -> Result<()> {
    // Noop on macOS — vx_fskit::setup() handles all pre-mount cleanup.
    Ok(())
}

/// Unmount — on macOS, vx-fskit::teardown() handles this.
pub fn unmount(_mount_path: &Utf8PathBuf) {
    // Noop — vx-reaper calls vx_fskit::teardown() on macOS.
}

/// Dump platform diagnostics to stderr.
pub async fn dump_platform_diagnostics(_mount_path: &Utf8PathBuf) {
    // vx-fskit returns structured logs; diagnostics live there now.
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
