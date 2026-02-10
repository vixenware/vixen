use camino::Utf8PathBuf;

use crate::Result;

/// Clean up stuck FSKit mount if needed.
pub async fn cleanup_stuck_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    use std::process::Command;

    tracing::debug!(mount_path = %mount_path, "checking for stuck FSKit mount");

    // Canonicalize the mount path
    // If it doesn't exist, canonicalize the parent and append the final component
    let canonical_mount_path = match tokio::fs::canonicalize(mount_path.as_std_path()).await {
        Ok(p) => {
            let canonical = Utf8PathBuf::try_from(p).unwrap_or_else(|_| mount_path.clone());
            tracing::debug!(
                mount_path = %mount_path,
                canonical_mount_path = %canonical,
                "canonicalized mount path directly"
            );
            canonical
        }
        Err(_) => {
            // Path doesn't exist, try canonicalizing parent
            if let Some(parent) = mount_path.parent() {
                match tokio::fs::canonicalize(parent.as_std_path()).await {
                    Ok(canonical_parent) => {
                        let canonical_parent = Utf8PathBuf::try_from(canonical_parent)
                            .unwrap_or_else(|_| parent.to_path_buf());
                        let canonical = canonical_parent.join(mount_path.file_name().unwrap());
                        tracing::debug!(
                            mount_path = %mount_path,
                            canonical_mount_path = %canonical,
                            "canonicalized via parent directory"
                        );
                        canonical
                    }
                    Err(e) => {
                        tracing::debug!(
                            mount_path = %mount_path,
                            error = %e,
                            "parent does not exist, using mount path as-is"
                        );
                        mount_path.clone()
                    }
                }
            } else {
                tracing::debug!(
                    mount_path = %mount_path,
                    "no parent directory, using mount path as-is"
                );
                mount_path.clone()
            }
        }
    };

    // Run `df` to check for existing mounts
    tracing::debug!("running df to check for existing mounts");
    let output = Command::new("df")
        .output()
        .map_err(|e| format!("failed to run df: {}", e))?;

    if !output.status.success() {
        tracing::warn!("df command failed, skipping mount cleanup");
        return Ok(()); // df failed, skip cleanup
    }

    let df_output = String::from_utf8_lossy(&output.stdout);
    tracing::trace!(df_output = %df_output, "df output");

    // Look for a mount at our target path
    let mut is_mounted = false;
    let mut found_mount_at: Option<String> = None;
    for line in df_output.lines().skip(1) {
        // Last field is the mount point
        if let Some(mounted_on) = line.split_whitespace().last() {
            // Canonicalize both paths for comparison (handles /tmp -> /private/tmp, symlinks, etc.)
            let canonical_df_path = match std::fs::canonicalize(mounted_on) {
                Ok(p) => Utf8PathBuf::try_from(p).ok(),
                Err(_) => None,
            };

            if let Some(df_path) = canonical_df_path {
                if df_path == canonical_mount_path {
                    tracing::info!(
                        mount_path = %mount_path,
                        canonical_mount_path = %canonical_mount_path,
                        df_entry = %mounted_on,
                        "found existing mount (matched via canonicalization)"
                    );
                    is_mounted = true;
                    found_mount_at = Some(mounted_on.to_string());
                    break;
                }
            } else {
                // If canonicalization fails, fall back to string comparison
                if mounted_on == mount_path.as_str() || mounted_on == canonical_mount_path.as_str()
                {
                    tracing::info!(
                        mount_path = %mount_path,
                        df_entry = %mounted_on,
                        "found existing mount (matched via string comparison)"
                    );
                    is_mounted = true;
                    found_mount_at = Some(mounted_on.to_string());
                    break;
                }
            }
        }
    }

    if !is_mounted {
        tracing::debug!(mount_path = %mount_path, "no existing mount found");
        return Ok(()); // Not mounted, nothing to clean up
    }

    let found_at = found_mount_at.as_deref().unwrap_or("unknown");
    tracing::info!(mount_path = %mount_path, found_at = %found_at, "proceeding with unmount");

    // Try to stat the mount point to see if it's responsive
    match tokio::fs::metadata(mount_path.as_std_path()).await {
        Ok(_) => {
            // Mount is responsive, but we still need to unmount to re-mount
            tracing::warn!(
                mount_path = %mount_path,
                "found existing mount, attempting unmount"
            );
        }
        Err(_) => {
            // Mount is stuck/unresponsive
            tracing::warn!(
                mount_path = %mount_path,
                "detected stuck FSKit mount, attempting cleanup"
            );
        }
    }

    // Use diskutil to unmount
    tracing::info!(mount_path = %mount_path, "attempting unmount: diskutil unmount");
    let status = Command::new("diskutil")
        .arg("unmount")
        .arg(canonical_mount_path.as_str())
        .status()
        .map_err(|e| format!("failed to run diskutil: {}", e))?;

    if status.success() {
        tracing::info!("successfully unmounted existing mount");
        Ok(())
    } else {
        Err(format!(
            "failed to unmount existing mount at {}",
            canonical_mount_path
        )
        .into())
    }
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
