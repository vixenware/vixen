use camino::Utf8PathBuf;

use crate::Result;

/// Resolve the canonical form of a mount path without touching the mount point
/// itself — which may be a dead FSKit mount that hangs on any access (stat, readdir).
///
/// Strategy: canonicalize the parent directory and append the final component.
fn canonicalize_without_stat(mount_path: &Utf8PathBuf) -> Utf8PathBuf {
    if let Some(parent) = mount_path.parent()
        && let Ok(canonical_parent) = std::fs::canonicalize(parent.as_std_path())
        && let Ok(canonical_parent) = Utf8PathBuf::try_from(canonical_parent)
        && let Some(name) = mount_path.file_name()
    {
        return canonical_parent.join(name);
    }
    mount_path.clone()
}

/// Clean up stuck FSKit mount if needed.
///
/// IMPORTANT: This function must never call stat/canonicalize/metadata on the
/// mount path itself. If the previous VFS session died, the mount point becomes
/// a dead FSKit mount — any filesystem access (stat, readdir, canonicalize) to
/// it hangs forever because the kernel waits for a reply from the (dead) FSKit
/// extension.
///
/// Dead FSKit mounts may not appear in `df` output, so we always attempt
/// `diskutil unmount force` on all candidate paths unconditionally. If nothing
/// is mounted, diskutil fails and we move on.
pub async fn cleanup_stuck_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    use std::process::Command;

    tracing::debug!(mount_path = %mount_path, "cleaning up any stale FSKit mount");

    let canonical = canonicalize_without_stat(mount_path);

    // Collect all candidate paths to try unmounting. Dead FSKit mounts don't
    // always show up in `df`, so we just try all plausible paths.
    let mut candidates: Vec<String> = vec![canonical.to_string()];
    let raw = mount_path.to_string();
    if raw != candidates[0] {
        candidates.push(raw.clone());
    }
    // /tmp → /private/tmp on macOS
    let private_prefixed = format!("/private{}", mount_path);
    if !candidates.contains(&private_prefixed) {
        candidates.push(private_prefixed);
    }

    for path in &candidates {
        let status = Command::new("diskutil")
            .arg("unmount")
            .arg("force")
            .arg(path)
            .output();

        match status {
            Ok(output) if output.status.success() => {
                tracing::info!(path = %path, "force-unmounted stale FSKit mount");
                return Ok(());
            }
            Ok(_) => {
                tracing::debug!(path = %path, "no mount to clean up at this path");
            }
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "failed to run diskutil");
            }
        }
    }

    tracing::debug!(mount_path = %mount_path, "no stale mount found (tried all candidates)");
    Ok(())
}

/// Unmount an FSKit filesystem using diskutil.
pub fn unmount(mount_path: &Utf8PathBuf) {
    let status = std::process::Command::new("diskutil")
        .arg("unmount")
        .arg(mount_path.as_str())
        .status();
    match status {
        Ok(s) if s.success() => tracing::info!("unmounted successfully"),
        Ok(s) => tracing::error!("unmount failed with status: {}", s),
        Err(e) => tracing::error!("failed to run diskutil: {}", e),
    }
}

/// No additional platform diagnostics on macOS.
pub fn dump_platform_diagnostics() {}

/// Whether we should create the mount directory.
/// On macOS, /Volumes is system-managed — don't create it ourselves.
pub fn should_create_mount_dir(mount_path: &Utf8PathBuf) -> bool {
    !mount_path.starts_with("/Volumes/")
}
