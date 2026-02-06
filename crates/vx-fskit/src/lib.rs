//! Health-checking and management for fs-kitty mounts.
//!
//! Shared by `vx doctor` and `vx fskit *` subcommands.

use eyre::{Result, bail};
use std::time::Duration;

/// Overall fs-kitty health status.
pub struct FskitHealth {
    /// Whether /Applications/FsKitty.app exists.
    pub app_installed: bool,

    /// Whether pluginkit shows the extension registered.
    pub plugin_registered: bool,

    /// All fskitty mounts discovered via `df`.
    pub mounts: Vec<FskittyMount>,
}

/// A single fskitty mount discovered via `df`.
pub struct FskittyMount {
    pub mount_point: String,
    pub host: String,
    pub port: u16,
    pub state: MountState,
}

/// Whether a mount's backing server is alive or dead.
pub enum MountState {
    Alive { pid: u32, process_name: String },
    Ghost,
}

impl FskitHealth {
    /// Number of ghost mounts.
    pub fn ghost_count(&self) -> usize {
        self.mounts
            .iter()
            .filter(|m| matches!(m.state, MountState::Ghost))
            .count()
    }
}

/// Run `df` with a timeout and parse all fskitty mounts.
fn discover_mounts() -> Vec<(String, String, u16)> {
    let Some(output) = run_command_with_timeout("df", &[], Duration::from_secs(2)) else {
        return vec![];
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries = vx_df::parse_df(&stdout);

    entries
        .into_iter()
        .filter_map(|entry| {
            let url = entry.as_fskitty_url()?;
            Some((entry.mount_point, url.host, url.port))
        })
        .collect()
}

/// TCP connect with 500ms timeout to check if something is listening on the port.
pub async fn is_port_alive(host: &str, port: u16) -> bool {
    let addr = format!("{}:{}", host, port);
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Use `lsof` to find which process is listening on a given TCP port.
/// Returns (pid, process_name) if found.
pub fn find_listener_process(port: u16) -> Option<(u32, String)> {
    let output = run_command_with_timeout(
        "lsof",
        &["-iTCP", &format!(":{}", port), "-sTCP:LISTEN", "-nP", "-Fp"],
        Duration::from_secs(2),
    )?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // lsof -Fp outputs lines like "p12345\n" for PID
    let mut pid: Option<u32> = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse().ok();
        }
    }

    let pid = pid?;

    // Get the process name via ps
    let ps_output = run_command_with_timeout("ps", &["-p", &pid.to_string(), "-o", "comm="], Duration::from_secs(2))?;
    let name = String::from_utf8_lossy(&ps_output.stdout).trim().to_string();

    Some((pid, name))
}

/// Run the full health check: discover mounts, probe ports, identify ghosts.
pub async fn check_health() -> FskitHealth {
    let app_installed = std::path::Path::new("/Applications/FsKitty.app").exists();

    let plugin_registered = if app_installed {
        check_plugin_registered()
    } else {
        false
    };

    let raw_mounts = discover_mounts();
    let mut mounts = Vec::with_capacity(raw_mounts.len());

    for (mount_point, host, port) in raw_mounts {
        let state = if is_port_alive(&host, port).await {
            match find_listener_process(port) {
                Some((pid, process_name)) => MountState::Alive { pid, process_name },
                // Port is alive but lsof didn't find it — still alive, just can't identify
                None => MountState::Alive {
                    pid: 0,
                    process_name: "unknown".to_string(),
                },
            }
        } else {
            MountState::Ghost
        };

        mounts.push(FskittyMount {
            mount_point,
            host,
            port,
            state,
        });
    }

    FskitHealth {
        app_installed,
        plugin_registered,
        mounts,
    }
}

/// Check if pluginkit shows the fs-kitty extension.
fn check_plugin_registered() -> bool {
    let Some(output) = run_command_with_timeout(
        "pluginkit",
        &["-m", "-p", "com.apple.fskit.fsmodule"],
        Duration::from_secs(2),
    ) else {
        return false;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains("me.amos.fs-kitty") || stdout.contains("fskitty")
}

/// Unmount a single fskitty mount. For alive mounts, diskutil unmount force.
/// For ghost mounts, kill the fs-kitty extension process (which auto-unmounts).
pub async fn unmount(mount: &FskittyMount) -> Result<()> {
    match &mount.state {
        MountState::Alive { .. } => unmount_direct(&mount.mount_point).await,
        MountState::Ghost => unmount_ghost(&mount.mount_point).await,
    }
}

/// Direct unmount via diskutil (works when the server is alive).
async fn unmount_direct(mount_point: &str) -> Result<()> {
    let mp = mount_point.to_string();
    tokio::task::spawn_blocking(move || {
        let Some(output) = run_command_with_timeout(
            "diskutil",
            &["unmount", "force", &mp],
            Duration::from_secs(10),
        ) else {
            bail!("diskutil unmount force timed out after 10s for {}", mp);
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("diskutil unmount force failed for {}: {}", mp, stderr.trim());
        }
        Ok(())
    })
    .await?
}

/// Ghost unmount: kill all fs-kitty extension processes, which causes fskitd to
/// auto-unmount. We kill all of them because there's no way to match a specific
/// extension process to a specific mount (the launch args don't contain mount info).
async fn unmount_ghost(mount_point: &str) -> Result<()> {
    let mp = mount_point.to_string();
    tokio::task::spawn_blocking(move || {
        kill_fskit_extensions();

        // Give fskitd a moment to process the deaths and unmount
        std::thread::sleep(Duration::from_millis(500));

        // Verify the mount is gone — if not, fall back to diskutil
        let Some(df_output) = run_command_with_timeout("df", &[], Duration::from_secs(2)) else {
            return Ok(());
        };
        let stdout = String::from_utf8_lossy(&df_output.stdout);
        if stdout.contains(&mp) {
            tracing::info!("Mount {} still present after killing extensions, trying diskutil", mp);
            let Some(output) = run_command_with_timeout(
                "diskutil",
                &["unmount", "force", &mp],
                Duration::from_secs(10),
            ) else {
                bail!("diskutil unmount force timed out after 10s for {}", mp);
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("diskutil unmount force failed for {}: {}", mp, stderr.trim());
            }
        }
        Ok(())
    })
    .await?
}

/// Kill all fs-kitty extension processes.
fn kill_fskit_extensions() {
    let Some(output) = run_command_with_timeout(
        "pkill",
        &["-f", "FsKitty.app/Contents/Extensions/fs-kitty.appex"],
        Duration::from_secs(2),
    ) else {
        tracing::warn!("pkill timed out trying to kill fs-kitty extensions");
        return;
    };
    // pkill returns 0 if processes were killed, 1 if none found — both are fine
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        if code != 1 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("pkill returned {}: {}", code, stderr.trim());
        }
    }
}

/// Reset all fs-kitty mounts by killing extension processes.
/// This is a bulk operation — kills all extensions, waits, then verifies.
pub async fn reset_all() -> Result<u32> {
    let health = check_health().await;
    if health.mounts.is_empty() {
        return Ok(0);
    }

    let total = health.mounts.len() as u32;

    // Kill all extension processes first (handles ghosts)
    tokio::task::spawn_blocking(kill_fskit_extensions).await?;

    // Give fskitd time to process
    tokio::time::sleep(Duration::from_millis(500)).await;

    // For any remaining mounts (alive ones whose server is still running),
    // use diskutil unmount force
    let remaining = discover_mounts();
    for (mount_point, _, _) in &remaining {
        let mp = mount_point.clone();
        let _ = tokio::task::spawn_blocking(move || {
            run_command_with_timeout(
                "diskutil",
                &["unmount", "force", &mp],
                Duration::from_secs(10),
            );
        })
        .await;
    }

    Ok(total)
}

/// Run a command with a timeout (blocking). Returns None on timeout.
fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Option<std::process::Output> {
    use std::process::Command;
    use std::time::Instant;

    let mut child = Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    let start = Instant::now();
    loop {
        if let Some(_status) = child.try_wait().ok()? {
            return child.wait_with_output().ok();
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
