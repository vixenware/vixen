use camino::Utf8PathBuf;
use tokio::process::Command;

use crate::probe;
use crate::process;
use crate::{FsKitResult, Log, mount_path_candidates};

/// Unmount a VixenFS filesystem.
///
/// Kills processes with files open under the mount, force-unmounts,
/// and verifies the mount is gone.
pub async fn teardown(mount_path: &Utf8PathBuf) -> FsKitResult {
    let mut log = Log::new();
    let candidates = mount_path_candidates(mount_path);

    log.record(format!("teardown: mount_path={mount_path}"));

    // --- Probe ---

    let probe_result = probe::probe(mount_path, &candidates, &mut log, false).await;

    if probe_result.is_clean() && !mount_path.exists() {
        log.record("nothing mounted and no stale directory, nothing to do");
        return FsKitResult::success(log.into_entries());
    }

    if probe_result.is_clean() && mount_path.exists() {
        log.record("stale mountpoint directory, removing");
        match std::fs::remove_dir(mount_path.as_std_path()) {
            Ok(()) => {
                log.record(format!("rmdir {mount_path}: ok"));
                return FsKitResult::success(log.into_entries());
            }
            Err(e) => {
                log.record(format!("rmdir {mount_path}: {e}"));
                // Not fatal — might have files in it, proceed anyway
            }
        }
    }

    // --- Kill open file holders ---

    let open_processes = process::find_open_processes(&candidates, &mut log).await;
    if !open_processes.is_empty() {
        let survivors = process::kill_and_wait(&open_processes, &mut log).await;
        if !survivors.is_empty() {
            let unkillable: Vec<String> = survivors
                .iter()
                .filter_map(|&pid| {
                    open_processes
                        .iter()
                        .find(|p| p.pid == pid)
                        .map(|p| format!("pid {} ({}) stuck on {}", p.pid, p.command, p.path))
                })
                .collect();
            return FsKitResult::failure(
                log.into_entries(),
                format!(
                    "{} process(es) are stuck in uninterruptible sleep and cannot be killed. \
                     A reboot is required.",
                    unkillable.len()
                ),
                vec!["reboot".to_string()],
            );
        }
    }

    // --- Unmount ---

    // Try diskutil unmount force on all candidates (dead mounts may only
    // respond to one specific path variant).
    for candidate in &candidates {
        log.record(format!("diskutil unmount force {candidate}"));

        let mut child = match Command::new("diskutil")
            .args(["unmount", "force", candidate])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                log.record(format!("diskutil: failed to spawn: {e}"));
                continue;
            }
        };

        match tokio::time::timeout(std::time::Duration::from_secs(1), child.wait_with_output())
            .await
        {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.success() {
                    log.record(format!("diskutil: ok — {}", stdout.trim()));
                } else {
                    log.record(format!(
                        "diskutil: failed (exit={}) — {}",
                        output.status,
                        stderr.trim()
                    ));
                }
            }
            Ok(Err(e)) => {
                log.record(format!("diskutil: error: {e}"));
            }
            Err(_) => {
                log.record("diskutil: timed out after 1s, killing");
                let _ = child.kill().await;
            }
        }
    }

    // --- Verify ---

    let after = probe::probe(mount_path, &candidates, &mut log, false).await;

    if after.is_clean() {
        // Clean up the stale directory if it exists
        if mount_path.exists() {
            let _ = std::fs::remove_dir(mount_path.as_std_path());
        }
        log.record("teardown complete: mount is clean");
        return FsKitResult::success(log.into_entries());
    }

    log.record("mount is still present after unmount attempt");
    FsKitResult::failure(
        log.into_entries(),
        "failed to unmount — mount point is still active after diskutil unmount force",
        vec!["sudo killall fskitd fskit_agent".to_string()],
    )
}
