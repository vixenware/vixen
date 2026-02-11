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

/// Collect comprehensive diagnostics when the VFS smoke test fails.
///
/// Runs every check regardless of earlier failures — we want the full picture.
/// Returns all diagnostics as a single string so the caller can print it atomically.
pub async fn collect_smoke_test_diagnostics(mount_path: &Utf8PathBuf) -> String {
    use owo_colors::OwoColorize;
    use std::fmt::Write;
    use tokio::process::Command;

    let mut out = String::new();

    writeln!(
        out,
        "\n{}\n",
        "=== VFS SMOKE TEST DIAGNOSTICS ===".red().bold()
    )
    .unwrap();

    // 1. App bundle integrity
    writeln!(out, "{}", "--- App bundle integrity ---".yellow().bold()).unwrap();
    let app_path = std::path::Path::new("/Applications/Vixen.app");
    let extension_path = app_path.join("Contents/Extensions/VixenFS.appex");
    let helper_path = app_path.join("Contents/Helpers/vixenfs-mount");

    writeln!(out, "  Vixen.app exists: {}", app_path.exists()).unwrap();
    writeln!(out, "  VixenFS.appex exists: {}", extension_path.exists()).unwrap();
    writeln!(out, "  vixenfs-mount exists: {}", helper_path.exists()).unwrap();

    if app_path.exists() {
        match Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app_path)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                writeln!(out, "  codesign --verify: OK").unwrap();
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                writeln!(out, "  codesign --verify: {}", "FAILED".red().bold()).unwrap();
                writeln!(out, "    {}", stderr.trim()).unwrap();
            }
            Err(e) => writeln!(out, "  codesign --verify: could not run: {e}").unwrap(),
        }
    }

    // 2. Plugin registration
    writeln!(out, "\n{}", "--- Plugin registration ---".yellow().bold()).unwrap();
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
                writeln!(out, "  No vixen-related FSKit plugins registered").unwrap();
            } else {
                for line in &vixen_lines {
                    writeln!(out, "  {line}").unwrap();
                }
            }
        }
        Err(e) => writeln!(out, "  pluginkit failed: {e}").unwrap(),
    }

    // 3. Mount status
    writeln!(out, "\n{}", "--- Mount status ---".yellow().bold()).unwrap();
    match Command::new("df").output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let vixen_lines: Vec<&str> = stdout
                .lines()
                .filter(|l| l.to_lowercase().contains("vixen"))
                .collect();
            if vixen_lines.is_empty() {
                writeln!(out, "  No vixen mounts found in df output").unwrap();
            } else {
                for line in &vixen_lines {
                    writeln!(out, "  {line}").unwrap();
                }
            }
        }
        Err(e) => writeln!(out, "  df failed: {e}").unwrap(),
    }

    // 4. Process inventory
    writeln!(out, "\n{}", "--- Vixen processes ---".yellow().bold()).unwrap();
    match Command::new("pgrep")
        .args(["-la", "(vx-|vixenfs-mount|VixenFS|Vixen)"])
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

    // 5. Mount helper log
    writeln!(out, "\n{}", "--- vixenfs-mount.log ---".yellow().bold()).unwrap();
    let log_path = mount_path
        .as_std_path()
        .parent()
        .map(|p| p.join("vixenfs-mount.log"));
    if let Some(log_path) = log_path {
        match tokio::fs::read_to_string(&log_path).await {
            Ok(content) => {
                if content.is_empty() {
                    writeln!(out, "  (empty)").unwrap();
                } else {
                    for line in content.lines() {
                        writeln!(out, "  {line}").unwrap();
                    }
                }
            }
            Err(e) => writeln!(out, "  Could not read {}: {e}", log_path.display()).unwrap(),
        }
    } else {
        writeln!(out, "  Could not determine log path").unwrap();
    }

    // 6. Process samples (vixenfs-mount and VixenFS)
    for process_name in &["vixenfs-mount", "VixenFS"] {
        writeln!(
            out,
            "\n{}",
            format!("--- sample {process_name} ---").yellow().bold()
        )
        .unwrap();
        match Command::new("pgrep")
            .arg("-x")
            .arg(process_name)
            .output()
            .await
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for pid in stdout.lines().filter(|l| !l.is_empty()) {
                    writeln!(out, "  Sampling PID {pid}...").unwrap();
                    match Command::new("sample").args([pid, "1"]).output().await {
                        Ok(sample_output) => {
                            let stdout = String::from_utf8_lossy(&sample_output.stdout);
                            for line in stdout.lines() {
                                writeln!(out, "  {line}").unwrap();
                            }
                        }
                        Err(e) => writeln!(out, "  sample failed: {e}").unwrap(),
                    }
                }
                if stdout.trim().is_empty() {
                    writeln!(out, "  Process not found").unwrap();
                }
            }
            Err(e) => writeln!(out, "  pgrep failed: {e}").unwrap(),
        }
    }

    // 7. System log stream (FSKit + vixenfs, last 2 minutes)
    writeln!(out, "\n{}", "--- System logs (last 2m) ---".yellow().bold()).unwrap();
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
                writeln!(out, "  No matching log entries").unwrap();
            } else {
                for line in stdout.lines() {
                    writeln!(out, "  {line}").unwrap();
                }
            }
        }
        Err(e) => writeln!(out, "  log show failed: {e}").unwrap(),
    }

    writeln!(out, "\n{}", "=== END DIAGNOSTICS ===".red().bold()).unwrap();
    out
}

/// Whether we should create the mount directory.
/// On macOS, /Volumes is system-managed — don't create it ourselves.
pub fn should_create_mount_dir(mount_path: &Utf8PathBuf) -> bool {
    !mount_path.starts_with("/Volumes/")
}
