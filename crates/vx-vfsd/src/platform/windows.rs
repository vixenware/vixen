use std::sync::Arc;

use camino::Utf8PathBuf;

use crate::Result;
use crate::vfs::CasVfs;

/// No stuck mount cleanup on Windows (yet).
pub async fn cleanup_stuck_mount(_mount_path: &Utf8PathBuf) -> Result<()> {
    Ok(())
}

/// Sentinel mode is not supported on Windows.
pub async fn run_sentinel(_mount_path: &Utf8PathBuf) -> Result<()> {
    tracing::error!("sentinel mode is not supported on Windows");
    Ok(())
}

/// No platform signal handler on Windows.
pub fn setup_diagnostics_signal(_vfs: Arc<CasVfs>) {}

/// No platform-specific diagnostics on Windows.
#[allow(dead_code)]
pub fn dump_platform_diagnostics() {}

/// Always create the mount directory on Windows.
pub fn should_create_mount_dir(_mount_path: &Utf8PathBuf) -> bool {
    true
}

/// Collect diagnostics when the VFS smoke test fails.
/// Returns all diagnostics as a single string so the caller can print it atomically.
pub async fn collect_smoke_test_diagnostics(mount_path: &Utf8PathBuf) -> String {
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
