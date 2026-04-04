use camino::Utf8PathBuf;

use crate::Result;

// Platform-specific modules
cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod linux;
        use linux as os;
    } else if #[cfg(target_os = "macos")] {
        mod macos;
        use macos as os;
    } else {
        mod windows;
        use windows as os;
    }
}

/// Pre-mount: clean up stuck mounts from previous crashes.
pub async fn cleanup_stuck_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    os::cleanup_stuck_mount(mount_path).await
}

/// Whether we should create the mount directory (false for system-managed paths).
pub fn should_create_mount_dir(mount_path: &Utf8PathBuf) -> bool {
    os::should_create_mount_dir(mount_path)
}

/// Collect comprehensive VFS diagnostics when any mount operation fails.
/// Returns all diagnostics as a single string for atomic output.
pub async fn collect_vfs_diagnostics(mount_path: &Utf8PathBuf) -> String {
    os::collect_vfs_diagnostics(mount_path).await
}
