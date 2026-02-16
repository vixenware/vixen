use std::process::Stdio;
use std::time::Duration;

use camino::Utf8PathBuf;
use peeps::Command;

use super::probe;
use super::process;
use super::{FsKitResult, Log, mount_path_candidates};

/// Unmount a VixenFS filesystem.
///
/// Kills processes with files open under the mount, force-unmounts,
/// and verifies the mount is gone.
pub async fn teardown(mount_path: &Utf8PathBuf) -> FsKitResult {
    let mut log = Log::new();
    let candidates = mount_path_candidates(mount_path).await;

    log.info(format!("teardown: mount_path={mount_path}"));

    // --- Probe ---

    let probe_result = probe::probe(mount_path, &candidates, &mut log, false).await;

    let mount_exists = peeps::fs::metadata(mount_path.as_std_path()).await.is_ok();

    if probe_result.is_clean() && !mount_exists {
        log.ok("nothing mounted and no stale directory, nothing to do");
        return FsKitResult::success(log.into_entries());
    }

    if probe_result.is_clean() && mount_exists {
        log.info("stale mountpoint directory, removing");
        match peeps::fs::remove_dir(mount_path.as_std_path()).await {
            Ok(()) => {
                log.ok(format!("rmdir {mount_path}: ok"));
                return FsKitResult::success(log.into_entries());
            }
            Err(e) => {
                log.fail(format!("rmdir {mount_path}: {e}"));
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
        log.info(format!("diskutil unmount force {candidate}"));

        let mut child = match Command::new("diskutil")
            .args(["unmount", "force", candidate])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                log.fail(format!("diskutil: failed to spawn: {e}"));
                continue;
            }
        };

        match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
            Ok(Ok(status)) => {
                let stdout = super::probe::read_pipe(child.take_stdout()).await;
                let stderr = super::probe::read_pipe(child.take_stderr()).await;
                let stdout = String::from_utf8_lossy(&stdout);
                let stderr = String::from_utf8_lossy(&stderr);
                if status.success() {
                    log.ok(format!("diskutil: ok — {}", stdout.trim()));
                } else {
                    log.fail(format!(
                        "diskutil: failed (exit={status}) — {}",
                        stderr.trim()
                    ));
                }
            }
            Ok(Err(e)) => {
                log.fail(format!("diskutil: error: {e}"));
            }
            Err(_) => {
                log.fail("diskutil: timed out after 1s, killing");
                let _ = child.kill();
            }
        }
    }

    // --- Kill stale VixenFS extension if still running ---

    if probe::probe_pgrep(&mut log).await {
        log.info("VixenFS extension still running after unmount, sending SIGTERM");
        let _ = Command::new("pkill").args(["-x", "VixenFS"]).output().await;
        // Give it a moment to exit
        tokio::time::sleep(Duration::from_millis(500)).await;
        if probe::probe_pgrep(&mut log).await {
            log.fail("VixenFS extension still running after SIGTERM");
        } else {
            log.ok("VixenFS extension exited");
        }
    }

    // --- Verify ---

    let after = probe::probe(mount_path, &candidates, &mut log, false).await;

    if after.is_clean() {
        // Clean up the stale directory if it exists
        if peeps::fs::metadata(mount_path.as_std_path()).await.is_ok() {
            let _ = peeps::fs::remove_dir(mount_path.as_std_path()).await;
        }
        log.ok("teardown complete: mount is clean");
        return FsKitResult::success(log.into_entries());
    }

    log.fail("mount is still present after unmount attempt");
    FsKitResult::failure(
        log.into_entries(),
        "failed to unmount — mount point is still active after diskutil unmount force",
        vec!["sudo killall fskitd fskit_agent".to_string()],
    )
}
