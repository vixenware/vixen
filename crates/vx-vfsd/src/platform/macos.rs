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
        let result = canonical_parent.join(name);
        tracing::debug!(%mount_path, %parent, %canonical_parent, %name, %result, "canonicalized via parent");
        return result;
    }
    tracing::debug!(%mount_path, "canonicalize fallback: returning mount_path as-is");
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
/// Build the set of candidate paths for a mount point.
///
/// Dead FSKit mounts don't always show up in `df`, so we try all plausible
/// path variants: canonical, raw, and /private-prefixed (macOS /tmp symlink).
fn mount_path_candidates(mount_path: &Utf8PathBuf) -> Vec<String> {
    let canonical = canonicalize_without_stat(mount_path);
    tracing::debug!(%mount_path, %canonical, "resolved canonical mount path");
    let mut candidates: Vec<String> = vec![canonical.to_string()];
    let raw = mount_path.to_string();
    if raw != candidates[0] {
        tracing::debug!(raw, "adding raw path as candidate");
        candidates.push(raw);
    }
    // /tmp → /private/tmp on macOS
    let private_prefixed = format!("/private{mount_path}");
    if !candidates.contains(&private_prefixed) {
        tracing::debug!(private_prefixed, "adding /private-prefixed candidate");
        candidates.push(private_prefixed);
    }
    tracing::debug!(?candidates, "mount path candidates");
    candidates
}

/// Kill processes that have files open under any of the given mount path candidates.
///
/// Runs `lsof -n -P -Fpcn` and parses the structured output to find PIDs with
/// open files under the mount prefix. Each matching process is killed with SIGKILL
/// (these are typically stuck linker/debug tool processes like `dsymutil` or `ld`
/// that prevent unmounting).
async fn kill_processes_under_mount(candidates: &[String]) -> crate::Result<()> {
    use std::collections::HashMap;
    use tokio::process::Command;

    tracing::debug!(
        ?candidates,
        "scanning for processes with files open under mount"
    );

    let output = match Command::new("lsof")
        .args(["-n", "-P", "-Fpcn"])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            tracing::debug!(error = %e, "lsof not available, skipping stuck process cleanup");
            return Ok(());
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    tracing::debug!(lsof_output_lines = stdout.lines().count(), "lsof completed");

    // Parse lsof -F output: p<pid>, c<command>, n<path> fields
    let mut current_pid: Option<u32> = None;
    let mut current_cmd: Option<String> = None;
    // pid → (command name, first matching path)
    let mut to_kill: HashMap<u32, (String, String)> = HashMap::new();

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            current_pid = rest.parse().ok();
            current_cmd = None;
        } else if let Some(rest) = line.strip_prefix('c') {
            current_cmd = Some(rest.to_string());
        } else if let Some(path) = line.strip_prefix('n')
            && let (Some(pid), Some(cmd)) = (current_pid, current_cmd.as_deref())
            && candidates
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
        {
            tracing::debug!(pid, cmd, path, "found process with file open under mount");
            to_kill
                .entry(pid)
                .or_insert_with(|| (cmd.to_string(), path.to_string()));
        }
    }

    if to_kill.is_empty() {
        tracing::debug!("no processes found with files open under mount");
        return Ok(());
    }

    tracing::debug!(
        count = to_kill.len(),
        "killing processes with files open under mount"
    );

    let mut pids_to_wait = Vec::new();
    for (&pid, (cmd, path)) in &to_kill {
        tracing::warn!(
            pid,
            cmd,
            path,
            "killing process with files open on stale mount"
        );
        // SAFETY: just sending a signal, no memory unsafety
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        pids_to_wait.push(pid);
    }

    // Wait for killed processes to actually disappear. Once kill(pid, 0)
    // returns ESRCH, the process is gone and its file descriptors are released.
    tracing::debug!(
        count = pids_to_wait.len(),
        "waiting for killed processes to exit"
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !pids_to_wait.is_empty() && tokio::time::Instant::now() < deadline {
        pids_to_wait.retain(|&pid| {
            // SAFETY: signal 0 just checks if the process exists
            let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
            ret == 0 // keep if still alive
        });
        if !pids_to_wait.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    if pids_to_wait.is_empty() {
        tracing::debug!("all killed processes have exited");
        return Ok(());
    }

    // These processes survived SIGKILL — they're in uninterruptible sleep (D/U state),
    // stuck on syscalls into the dead FSKit mount. Nothing can kill them short of a
    // reboot. The mount point is permanently unusable.
    let unkillable: Vec<String> = pids_to_wait
        .iter()
        .filter_map(|&pid| {
            to_kill
                .get(&pid)
                .map(|(cmd, path)| format!("  pid {pid} ({cmd}) stuck on {path}"))
        })
        .collect();
    tracing::error!(
        count = unkillable.len(),
        "processes survived SIGKILL (uninterruptible sleep)"
    );
    Err(format!(
        "mount point is unusable: {} process(es) are stuck in uninterruptible sleep \
         on the dead FSKit mount and cannot be killed:\n{}\n\
         A reboot is required to clear this state.",
        unkillable.len(),
        unkillable.join("\n"),
    )
    .into())
}

pub async fn cleanup_stuck_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    tracing::debug!(mount_path = %mount_path, "cleaning up any stale FSKit mount");

    let candidates = mount_path_candidates(mount_path);

    // Kill any processes with files open under the mount point.
    // These (typically dsymutil, ld) prevent force-unmount from succeeding.
    kill_processes_under_mount(&candidates).await?;

    // TODO: diskutil unmount force has never actually worked in practice —
    // the stuck processes were always the real blocker. Revisit once we have
    // evidence that killing the stragglers alone isn't sufficient.

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

/// Dump platform diagnostics to stderr.
pub async fn dump_platform_diagnostics(mount_path: &Utf8PathBuf) {
    eprint!("{}", collect_vfs_diagnostics(mount_path).await);
}

/// Collect comprehensive diagnostics when the VFS smoke test fails.
///
/// Runs every check regardless of earlier failures — we want the full picture.
/// Returns all diagnostics as a single string so the caller can print it atomically.
pub async fn collect_vfs_diagnostics(mount_path: &Utf8PathBuf) -> String {
    use owo_colors::OwoColorize;
    use std::fmt::Write;
    use std::time::Instant;
    use tokio::process::Command;

    let total_start = Instant::now();

    // Compute log_path upfront (no async needed)
    let log_path = mount_path
        .as_std_path()
        .parent()
        .map(|p| p.join("vixenfs-mount.log"));

    // Run all diagnostics in parallel
    let (
        app_bundle,
        plugin_reg,
        mount_status,
        process_inv,
        mount_log,
        sample_mount_helper,
        sample_vixenfs,
        system_logs,
    ) = tokio::join!(
        // 1. App bundle integrity
        async {
            let start = Instant::now();
            let mut section = String::new();
            let app_path = std::path::Path::new("/Applications/Vixen.app");
            let extension_path = app_path.join("Contents/Extensions/VixenFS.appex");
            let helper_path = app_path.join("Contents/Helpers/vixenfs-mount");

            writeln!(section, "  Vixen.app exists: {}", app_path.exists()).unwrap();
            writeln!(
                section,
                "  VixenFS.appex exists: {}",
                extension_path.exists()
            )
            .unwrap();
            writeln!(section, "  vixenfs-mount exists: {}", helper_path.exists()).unwrap();

            if app_path.exists() {
                match Command::new("codesign")
                    .args(["--verify", "--deep", "--strict"])
                    .arg(app_path)
                    .output()
                    .await
                {
                    Ok(output) if output.status.success() => {
                        writeln!(section, "  codesign --verify: OK").unwrap();
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        writeln!(section, "  codesign --verify: {}", "FAILED".red().bold())
                            .unwrap();
                        writeln!(section, "    {}", stderr.trim()).unwrap();
                    }
                    Err(e) => writeln!(section, "  codesign --verify: could not run: {e}").unwrap(),
                }
            }

            ("App bundle integrity", section, start.elapsed())
        },
        // 2. Plugin registration
        async {
            let start = Instant::now();
            let mut section = String::new();
            match Command::new("pluginkit")
                .args(["-m", "-p", "com.apple.fskit.fsmodule"])
                .output()
                .await
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let vixen_lines: Vec<&str> = stdout
                        .lines()
                        .filter(|l| l.to_lowercase().contains("vixen") || l.contains("eu.bearcove"))
                        .collect();
                    if vixen_lines.is_empty() {
                        writeln!(section, "  No vixen-related FSKit plugins registered").unwrap();
                    } else {
                        for line in &vixen_lines {
                            writeln!(section, "  {line}").unwrap();
                        }
                    }
                }
                Err(e) => writeln!(section, "  pluginkit failed: {e}").unwrap(),
            }
            ("Plugin registration", section, start.elapsed())
        },
        // 3. Mount status
        async {
            let start = Instant::now();
            let mut section = String::new();
            match Command::new("df").output().await {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let vixen_lines: Vec<&str> = stdout
                        .lines()
                        .filter(|l| l.to_lowercase().contains("vixen"))
                        .collect();
                    if vixen_lines.is_empty() {
                        writeln!(section, "  No vixen mounts found in df output").unwrap();
                    } else {
                        for line in &vixen_lines {
                            writeln!(section, "  {line}").unwrap();
                        }
                    }
                }
                Err(e) => writeln!(section, "  df failed: {e}").unwrap(),
            }
            ("Mount status", section, start.elapsed())
        },
        // 4. Process inventory
        async {
            let start = Instant::now();
            let mut section = String::new();
            match Command::new("pgrep")
                .args(["-la", "(vx-|vixenfs-mount|VixenFS|Vixen)"])
                .output()
                .await
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if stdout.trim().is_empty() {
                        writeln!(section, "  No vixen-related processes found").unwrap();
                    } else {
                        for line in stdout.lines() {
                            writeln!(section, "  {line}").unwrap();
                        }
                    }
                }
                Err(e) => writeln!(section, "  pgrep failed: {e}").unwrap(),
            }
            ("Vixen processes", section, start.elapsed())
        },
        // 5. Mount helper log
        async {
            let start = Instant::now();
            let mut section = String::new();
            if let Some(log_path) = &log_path {
                match tokio::fs::read_to_string(log_path).await {
                    Ok(content) => {
                        if content.is_empty() {
                            writeln!(section, "  (empty)").unwrap();
                        } else {
                            for line in content.lines() {
                                writeln!(section, "  {line}").unwrap();
                            }
                        }
                    }
                    Err(e) => {
                        writeln!(section, "  Could not read {}: {e}", log_path.display()).unwrap()
                    }
                }
            } else {
                writeln!(section, "  Could not determine log path").unwrap();
            }
            ("vixenfs-mount.log", section, start.elapsed())
        },
        // 6a. Sample vixenfs-mount
        async {
            let start = Instant::now();
            let section = sample_process("vixenfs-mount").await;
            ("sample vixenfs-mount", section, start.elapsed())
        },
        // 6b. Sample VixenFS
        async {
            let start = Instant::now();
            let section = sample_process("VixenFS").await;
            ("sample VixenFS", section, start.elapsed())
        },
        // 7. System logs
        async {
            let start = Instant::now();
            let mut section = String::new();
            match Command::new("log")
                .args([
                    "show",
                    "--last",
                    "2m",
                    "--predicate",
                    r#"subsystem == "com.apple.FSKit" OR subsystem == "eu.bearcove.vixenfs""#,
                    "--info",
                    "--debug",
                ])
                .output()
                .await
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if stdout.trim().is_empty() {
                        writeln!(section, "  No matching log entries").unwrap();
                    } else {
                        for line in stdout.lines() {
                            writeln!(section, "  {line}").unwrap();
                        }
                    }
                }
                Err(e) => writeln!(section, "  log show failed: {e}").unwrap(),
            }
            ("System logs (last 2m)", section, start.elapsed())
        },
    );

    // Assemble output in order
    let mut out = String::new();
    writeln!(out, "\n{}\n", "=== VFS DIAGNOSTICS ===".red().bold()).unwrap();

    for (name, section, elapsed) in [
        app_bundle,
        plugin_reg,
        mount_status,
        process_inv,
        mount_log,
        sample_mount_helper,
        sample_vixenfs,
        system_logs,
    ] {
        writeln!(
            out,
            "{} {}",
            format!("--- {name} ---").yellow().bold(),
            format!("({elapsed:.1?})").dimmed(),
        )
        .unwrap();
        out.push_str(&section);
    }

    let total = total_start.elapsed();
    writeln!(
        out,
        "\n{} {}",
        "=== END DIAGNOSTICS ===".red().bold(),
        format!("(total: {total:.1?})").dimmed(),
    )
    .unwrap();
    out
}

async fn sample_process(process_name: &str) -> String {
    use std::fmt::Write;
    use tokio::process::Command;

    let mut section = String::new();
    match Command::new("pgrep")
        .arg("-x")
        .arg(process_name)
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for pid in stdout.lines().filter(|l| !l.is_empty()) {
                writeln!(section, "  Sampling PID {pid}...").unwrap();
                match Command::new("sample").args([pid, "1"]).output().await {
                    Ok(sample_output) => {
                        let stdout = String::from_utf8_lossy(&sample_output.stdout);
                        for line in stdout.lines() {
                            writeln!(section, "  {line}").unwrap();
                        }
                    }
                    Err(e) => writeln!(section, "  sample failed: {e}").unwrap(),
                }
            }
            if stdout.trim().is_empty() {
                writeln!(section, "  Process not found").unwrap();
            }
        }
        Err(e) => writeln!(section, "  pgrep failed: {e}").unwrap(),
    }
    section
}

/// Whether we should create the mount directory.
/// On macOS, /Volumes is system-managed — don't create it ourselves.
pub fn should_create_mount_dir(mount_path: &Utf8PathBuf) -> bool {
    !mount_path.starts_with("/Volumes/")
}
