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

/// Unmount a single fskitty mount. For alive mounts, just diskutil unmount force.
/// For ghost mounts, we need to first listen on the dead port so FSKit can talk to us.
pub async fn unmount(mount: &FskittyMount) -> Result<()> {
    match &mount.state {
        MountState::Alive { .. } => unmount_direct(&mount.mount_point).await,
        MountState::Ghost => unmount_ghost(&mount.host, mount.port, &mount.mount_point).await,
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

/// Ghost unmount: bind the dead port, accept connections (so FSKit can talk to us),
/// then run diskutil unmount force.
pub async fn unmount_ghost(host: &str, port: u16, mount_point: &str) -> Result<()> {
    let addr = format!("{}:{}", host, port);

    // Bind the dead port
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        eyre::eyre!(
            "failed to bind {}:{} for ghost unmount of {}: {}",
            host,
            port,
            mount_point,
            e
        )
    })?;

    tracing::info!(
        port,
        mount_point,
        "listening on ghost port, running diskutil unmount force"
    );

    // Spawn accept loop that accepts and immediately drops connections
    let accept_handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((_stream, _addr)) => {
                    // Drop the connection immediately — just enough to unblock FSKit
                }
                Err(_) => break,
            }
        }
    });

    // Run diskutil unmount force with timeout
    let mp = mount_point.to_string();
    let unmount_result = tokio::task::spawn_blocking(move || {
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
    .await?;

    // Abort the accept loop
    accept_handle.abort();

    unmount_result
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
