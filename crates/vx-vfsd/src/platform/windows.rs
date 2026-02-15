use camino::Utf8PathBuf;

use crate::Result;

/// No stuck mount cleanup on Windows (yet).
pub async fn cleanup_stuck_mount(_mount_path: &Utf8PathBuf) -> Result<()> {
    Ok(())
}

/// Sentinel mode is not supported on Windows.
pub async fn run_sentinel(_mount_path: &Utf8PathBuf) -> Result<()> {
    tracing::error!("sentinel mode is not supported on Windows");
    Ok(())
}

/// No platform-specific diagnostics on Windows.
pub async fn dump_platform_diagnostics(_mount_path: &Utf8PathBuf) {}

/// Always create the mount directory on Windows.
pub fn should_create_mount_dir(_mount_path: &Utf8PathBuf) -> bool {
    true
}

/// Collect comprehensive VFS diagnostics when any mount operation fails.
/// Returns all diagnostics as a single string so the caller can print it atomically.
pub async fn collect_vfs_diagnostics(mount_path: &Utf8PathBuf) -> String {
    use owo_colors::OwoColorize;
    use std::fmt::Write;

    let mut out = String::new();
    writeln!(
        out,
        "\n{}\n",
        "=== VFS SMOKE TEST DIAGNOSTICS ===".red().bold()
    )
    .unwrap();
    writeln!(out, "  Mount path: {mount_path}").unwrap();
    writeln!(out, "  (Windows diagnostics not yet implemented)").unwrap();
    writeln!(out, "\n{}", "=== END DIAGNOSTICS ===".red().bold()).unwrap();
    out
}
