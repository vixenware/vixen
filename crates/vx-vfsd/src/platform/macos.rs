use camino::Utf8PathBuf;

use crate::Result;

pub async fn cleanup_stuck_mount(_mount_path: &Utf8PathBuf) -> Result<()> {
    // Noop on macOS — vx_fskit::setup() handles all pre-mount cleanup.
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
    let (app_bundle, plugin_reg, mount_status, process_inv, mount_log, reaper_log) = tokio::join!(
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
        // 7. vx-reaper log
        async {
            let start = Instant::now();
            let mut section = String::new();
            let reaper_log = mount_path
                .as_std_path()
                .parent()
                .map(|p| p.join("vx-reaper.log"));
            if let Some(reaper_log) = &reaper_log {
                match tokio::fs::read_to_string(reaper_log).await {
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
                        writeln!(section, "  Could not read {}: {e}", reaper_log.display()).unwrap()
                    }
                }
            } else {
                writeln!(section, "  Could not determine log path").unwrap();
            }
            ("vx-reaper.log", section, start.elapsed())
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
        reaper_log,
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

/// Whether we should create the mount directory.
/// On macOS, /Volumes is system-managed — don't create it ourselves.
pub fn should_create_mount_dir(mount_path: &Utf8PathBuf) -> bool {
    !mount_path.starts_with("/Volumes/")
}
