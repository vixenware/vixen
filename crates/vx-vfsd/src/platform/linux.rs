use camino::Utf8PathBuf;

use crate::Result;

/// Clean up stuck FUSE mount if needed.
pub async fn cleanup_stuck_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    // Check if mount exists in /proc/mounts
    let mounts = match std::fs::read_to_string("/proc/mounts") {
        Ok(content) => content,
        Err(_) => return Ok(()), // Can't read mounts, skip cleanup
    };

    if !mounts.contains(mount_path.as_str()) {
        // Not mounted, nothing to clean up
        return Ok(());
    }

    // Try to stat the mount point to see if it's stuck
    match std::fs::metadata(mount_path.as_std_path()) {
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
    if let Ok(entries) = std::fs::read_dir("/sys/fs/fuse/connections") {
        for entry in entries.flatten() {
            let abort_path = entry.path().join("abort");
            if abort_path.exists() {
                let _ = std::fs::write(&abort_path, b"1");
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

/// Dump kernel FUSE state and thread stacks to stderr.
pub fn dump_platform_diagnostics() {
    eprintln!("\n--- Kernel FUSE state ---");

    if let Ok(entries) = std::fs::read_dir("/sys/fs/fuse/connections") {
        for entry in entries.flatten() {
            let waiting_path = entry.path().join("waiting");
            if let Ok(val) = std::fs::read_to_string(&waiting_path) {
                eprintln!("  {}: waiting={}", entry.path().display(), val.trim());
            }
        }
    }

    eprintln!("\n--- Thread stacks ---");
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            let tid = task.file_name();
            let comm = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
            let stack = std::fs::read_to_string(task.path().join("stack")).unwrap_or_default();
            if !stack.trim().is_empty() {
                eprintln!("  Thread {} ({}):", tid.to_string_lossy(), comm.trim());
                for line in stack.lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }
}

/// Whether we should create the mount directory.
pub fn should_create_mount_dir(_mount_path: &Utf8PathBuf) -> bool {
    true
}
