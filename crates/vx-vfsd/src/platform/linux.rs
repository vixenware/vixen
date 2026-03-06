use camino::Utf8PathBuf;

use crate::Result;

/// Clean up stuck FUSE mount if needed.
pub async fn cleanup_stuck_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    // Check if mount exists in /proc/mounts
    let mounts = match moire::fs::read_to_string("/proc/mounts").await {
        Ok(content) => content,
        Err(_) => return Ok(()), // Can't read mounts, skip cleanup
    };

    if !mounts.contains(mount_path.as_str()) {
        // Not mounted, nothing to clean up
        return Ok(());
    }

    // Try to stat the mount point to see if it's stuck
    match moire::fs::metadata(mount_path.as_std_path()).await {
        Ok(_) => {
            // stat succeeded, mount is healthy
            return Ok(());
        }
        Err(e) if e.raw_os_error() == Some(107) => {
            // ENOTCONN - Transport endpoint is not connected
            // This is a stuck mount, proceed with cleanup
            tracing::warn!(
                mount_path = %mount_path,
                "detected stuck FUSE mount, attempting cleanup"
            );
        }
        Err(_) => {
            // Some other error, let it fail naturally
            return Ok(());
        }
    }

    // Level 1: Try gentle unmount
    tracing::info!(mount_path = %mount_path, "attempting gentle unmount: fusermount3 -u");
    let status = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(mount_path.as_str())
        .status()
        .map_err(|e| format!("failed to run fusermount3: {}", e))?;

    if status.success() {
        tracing::info!("successfully unmounted stuck FUSE mount");
        return Ok(());
    }

    // Level 2: Try lazy unmount
    tracing::warn!(mount_path = %mount_path, "gentle unmount failed, trying lazy unmount: fusermount3 -uz");
    let status = std::process::Command::new("fusermount3")
        .arg("-uz")
        .arg(mount_path.as_str())
        .status()
        .map_err(|e| format!("failed to run fusermount3: {}", e))?;

    if status.success() {
        tracing::info!("successfully unmounted with lazy unmount");
        return Ok(());
    }

    // Level 3: Nuclear option - abort FUSE connections then unmount
    tracing::error!("lazy unmount failed, attempting nuclear cleanup");

    // Abort all FUSE connections
    if let Ok(mut entries) = tokio::fs::read_dir("/sys/fs/fuse/connections").await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let abort_path = entry.path().join("abort");
            if moire::fs::try_exists(&abort_path).await.unwrap_or(false) {
                let _ = moire::fs::write(&abort_path, b"1").await;
            }
        }
    }

    // Try unmount again after abort
    let status = std::process::Command::new("fusermount3")
        .arg("-uz")
        .arg(mount_path.as_str())
        .status()
        .map_err(|e| format!("failed to run fusermount3: {}", e))?;

    if status.success() {
        tracing::warn!("nuclear cleanup succeeded");
        Ok(())
    } else {
        Err(format!(
            "failed to clean up stuck FUSE mount at {} after all attempts",
            mount_path
        )
        .into())
    }
}

/// Unmount a FUSE filesystem using fusermount3.
pub fn unmount(mount_path: &Utf8PathBuf) {
    let status = std::process::Command::new("fusermount3")
        .arg("-uz")
        .arg(mount_path.as_str())
        .status();
    match status {
        Ok(s) if s.success() => tracing::info!("unmounted successfully"),
        Ok(s) => tracing::error!("unmount failed with status: {}", s),
        Err(e) => tracing::error!("failed to run fusermount3: {}", e),
    }
}

/// Whether we should create the mount directory.
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

    // Check /proc/mounts for FUSE entries
    writeln!(out, "{}", "--- FUSE mounts ---".yellow().bold()).unwrap();
    match moire::fs::read_to_string("/proc/mounts").await {
        Ok(mounts) => {
            let fuse_lines: Vec<&str> = mounts
                .lines()
                .filter(|l| l.contains("fuse") || l.contains("vixen"))
                .collect();
            if fuse_lines.is_empty() {
                writeln!(out, "  No FUSE/vixen mounts found").unwrap();
            } else {
                for line in &fuse_lines {
                    writeln!(out, "  {line}").unwrap();
                }
            }
        }
        Err(e) => writeln!(out, "  Could not read /proc/mounts: {e}").unwrap(),
    }

    // FUSE connection state
    writeln!(out, "\n{}", "--- FUSE connections ---".yellow().bold()).unwrap();
    if let Ok(mut entries) = tokio::fs::read_dir("/sys/fs/fuse/connections").await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let waiting_path = entry.path().join("waiting");
            if let Ok(val) = moire::fs::read_to_string(&waiting_path).await {
                writeln!(out, "  {}: waiting={}", entry.path().display(), val.trim()).unwrap();
            }
        }
    }

    // Process inventory
    writeln!(out, "\n{}", "--- Vixen processes ---".yellow().bold()).unwrap();
    match moire::process::Command::new("pgrep")
        .args(["-la", "(vx-|vixen)"])
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.trim().is_empty() {
                writeln!(out, "  No vixen-related processes found").unwrap();
            } else {
                for line in stdout.lines() {
                    writeln!(out, "  {line}").unwrap();
                }
            }
        }
        Err(e) => writeln!(out, "  pgrep failed: {e}").unwrap(),
    }

    // Mount point stat
    writeln!(out, "\n{}", "--- Mount point ---".yellow().bold()).unwrap();
    writeln!(out, "  Expected: {mount_path}").unwrap();
    match tokio::fs::symlink_metadata(mount_path.as_std_path()).await {
        Ok(meta) => writeln!(out, "  stat: exists, is_dir={}", meta.is_dir()).unwrap(),
        Err(e) => writeln!(out, "  stat: {e}").unwrap(),
    }

    writeln!(out, "\n{}", "=== END DIAGNOSTICS ===".red().bold()).unwrap();
    out
}
