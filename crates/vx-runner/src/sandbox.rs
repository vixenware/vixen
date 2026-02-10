//! Platform-specific sandboxing for compiler subprocesses.
//!
//! ## macOS (Seatbelt)
//! Wraps compiler invocations with `sandbox-exec` to enforce:
//! - No network access
//! - File writes restricted to the action's output directory
//! - File reads restricted to VFS root + system paths
//!
//! ## Linux (Landlock)
//! Wraps compiler invocations with `landlock-exec` (custom binary) to enforce:
//! - No network access (no BindTcp/ConnectTcp rules)
//! - File writes restricted to the action's output directory + /tmp (rustc ignores TMPDIR)
//! - File reads restricted to VFS root + system paths (/usr, /lib, /etc)
//!
//! Also provides timeout enforcement and streamed output for all platforms.

#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use vx_store_proto::StoreProgress;

use crate::service_vfs::ActionContext;

/// Default timeout for compile actions (rustc, zig cc): 5 minutes.
const DEFAULT_COMPILE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Default timeout for link actions: 10 minutes (linking can be slow).
const DEFAULT_LINK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How often to send progress updates (if the last line changed).
const PROGRESS_TICK_INTERVAL: Duration = Duration::from_millis(250);

/// What kind of action is being executed (affects default timeout).
pub enum ActionKind {
    RustCompile,
    CcCompile,
    CcLink,
}

/// Get the timeout for an action kind, respecting environment variable overrides.
///
/// - `VX_ACTION_TIMEOUT_SECS` overrides compile timeouts (rustc, zig cc)
/// - `VX_LINK_TIMEOUT_SECS` overrides link timeouts
pub fn action_timeout(kind: &ActionKind) -> Duration {
    match kind {
        ActionKind::RustCompile | ActionKind::CcCompile => {
            env_timeout("VX_ACTION_TIMEOUT_SECS", DEFAULT_COMPILE_TIMEOUT)
        }
        ActionKind::CcLink => env_timeout("VX_LINK_TIMEOUT_SECS", DEFAULT_LINK_TIMEOUT),
    }
}

fn env_timeout(var: &str, default: Duration) -> Duration {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}

/// Result of running a sandboxed command with streaming and timeout.
pub struct CommandOutput {
    pub exit_status: ExitStatus,
    pub combined_output: String,
}

/// Error from `run_sandboxed_command`.
pub enum CommandError {
    /// Failed to spawn the child process.
    SpawnFailed(std::io::Error),
    /// The action exceeded its timeout and was killed.
    TimedOut {
        action_desc: String,
        timeout: Duration,
        partial_output: String,
    },
}

/// Spawn a sandboxed command, stream merged stdout/stderr with 250ms progress
/// ticks, and enforce a timeout. Kills the process group on timeout.
///
/// `action_desc` is a human-readable description like "rustc foo" for error messages.
/// `progress` receives the latest output line every 250ms (if changed).
pub async fn run_sandboxed_command(
    mut cmd: Command,
    kind: &ActionKind,
    action_desc: &str,
    progress: roam::Tx<StoreProgress>,
) -> Result<CommandOutput, CommandError> {
    let timeout = action_timeout(kind);

    // Pipe stdout and stderr so we can stream them
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // Put the child in its own process group so we can kill the whole tree
    // without killing the runner itself.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(CommandError::SpawnFailed)?;

    // Take ownership of stdout/stderr handles
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Merge stdout and stderr into a single combined output buffer.
    // Also track the latest line for progress reporting.
    let combined_output = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
    let latest_line = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));

    // Spawn readers for both streams
    let combined_for_stdout = combined_output.clone();
    let latest_for_stdout = latest_line.clone();
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    combined_for_stdout.lock().await.push_str(&line);
                    if !trimmed.is_empty() {
                        *latest_for_stdout.lock().await = trimmed;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let combined_for_stderr = combined_output.clone();
    let latest_for_stderr = latest_line.clone();
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    combined_for_stderr.lock().await.push_str(&line);
                    if !trimmed.is_empty() {
                        *latest_for_stderr.lock().await = trimmed;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Progress tick task: every 250ms, send the latest line if it changed
    let latest_for_tick = latest_line.clone();
    let action_desc_owned = action_desc.to_string();
    let tick_task = tokio::spawn(async move {
        let mut last_sent = String::new();
        let mut interval = tokio::time::interval(PROGRESS_TICK_INTERVAL);
        loop {
            interval.tick().await;
            let current = latest_for_tick.lock().await.clone();
            if current != last_sent && !current.is_empty() {
                let _ = progress
                    .send(&StoreProgress {
                        message: format!("{action_desc_owned}: {current}"),
                        current: None,
                        total: None,
                    })
                    .await;
                last_sent = current;
            }
        }
    });

    // Wait for child with timeout
    let result = tokio::time::timeout(timeout, child.wait()).await;

    // Stop the tick task
    tick_task.abort();

    match result {
        Ok(Ok(exit_status)) => {
            // Child exited normally (possibly with non-zero exit code).
            // Wait for readers to drain.
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            let output = combined_output.lock().await.clone();
            Ok(CommandOutput {
                exit_status,
                combined_output: output,
            })
        }
        Ok(Err(io_err)) => {
            // IO error waiting for child — treat like spawn failure
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            Err(CommandError::SpawnFailed(io_err))
        }
        Err(_elapsed) => {
            // Timeout — kill the process group
            let pid = child.id();
            tracing::error!(
                action_desc,
                ?pid,
                timeout_secs = timeout.as_secs(),
                "action timed out, killing process group"
            );

            if let Some(pid) = pid {
                kill_process_group(pid);
            }
            // Also try the tokio kill as a fallback
            let _ = child.kill().await;

            // Drain whatever output we got
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            let partial = combined_output.lock().await.clone();

            Err(CommandError::TimedOut {
                action_desc: action_desc.to_string(),
                timeout,
                partial_output: partial,
            })
        }
    }
}

/// Kill an entire process group by sending SIGKILL to the negative PID.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let pgid = pid as libc::pid_t;
    // Kill the entire process group (negative PID)
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    // Also kill the process itself in case setpgid didn't take effect
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {
    // On non-unix, rely on tokio's child.kill() fallback
}

/// The seatbelt profile (SBPL) used for all sandboxed subprocesses.
///
/// Security model:
/// - Base: inherits system.sb for process startup, dyld, system libraries
/// - Process exec: VFS_ROOT + /usr/bin (for linker tools - TODO: use hermetic Zig lld)
/// - File reads/maps: restricted to VFS_ROOT only
/// - File writes: WRITE_DIR (action output) + /private/var/folders (temp files)
/// - Network: denied entirely — actions must be hermetic.
///
/// TODO: Replace system linker with hermetic Zig lld + mount system libraries in VFS
#[cfg(target_os = "macos")]
const SEATBELT_PROFILE: &str = r#"(version 1)
(debug deny)
(deny default)
(import "system.sb")
(allow process-fork)
(allow process-exec (subpath (param "VFS_ROOT")))
(allow process-exec (subpath (param "VFS_ROOT_ALT")))
(allow process-exec (subpath "/usr/bin"))
(allow process-exec (subpath "/Library/Developer/CommandLineTools"))
(allow file-read* (subpath (param "VFS_ROOT")))
(allow file-read* (subpath (param "VFS_ROOT_ALT")))
(allow file-read* (subpath "/usr"))
(allow file-read* (subpath "/System"))
(allow file-read* (subpath "/Library"))
(allow file-read* (subpath "/private"))
(allow file-write* (subpath (param "WRITE_DIR")))
(allow file-write* (subpath (param "WRITE_DIR_ALT")))
(allow file-write* (subpath "/private/var/folders"))
(deny network*)"#;

/// Create a `Command` that runs `program` under the seatbelt sandbox.
///
/// On non-macOS platforms, returns `Command::new(program)` directly.
#[cfg(target_os = "macos")]
pub fn sandboxed_command(program: &str, action: &ActionContext) -> Command {
    tracing::trace!(
        program,
        vfs_root = %action.mount_base,
        write_dir = %action.out_path,
        profile_len = SEATBELT_PROFILE.len(),
        "sandboxed_command: wrapping with sandbox-exec"
    );
    tracing::trace!(profile = %SEATBELT_PROFILE, "seatbelt profile");

    // Check if the program binary exists on disk before we try to run it.
    // This helps distinguish "sandbox-exec not found" from "program not found".
    let program_exists = std::path::Path::new(program).exists();
    let program_is_file = std::path::Path::new(program)
        .metadata()
        .map(|m| m.is_file())
        .unwrap_or(false);
    let program_permissions = std::fs::metadata(program)
        .map(|m| format!("{:o}", m.permissions().mode()))
        .unwrap_or_else(|e| format!("error: {e}"));

    tracing::trace!(
        program,
        program_exists,
        program_is_file,
        program_permissions,
        "sandboxed_command: program binary check"
    );

    let sandbox_exec_exists = std::path::Path::new("/usr/bin/sandbox-exec").exists();
    tracing::trace!(
        sandbox_exec_exists,
        "sandboxed_command: sandbox-exec binary check"
    );

    let write_dir_exists = action.out_path.as_std_path().exists();
    tracing::trace!(
        write_dir = %action.out_path,
        write_dir_exists,
        "sandboxed_command: write_dir check"
    );

    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(SEATBELT_PROFILE);

    // Pass both /tmp and /private/tmp variants since macOS may resolve either way
    let write_dir = action.out_path.to_string();
    let write_dir_alt = write_dir.replace("/tmp", "/private/tmp");
    let vfs_root = action.mount_base.to_string();
    let vfs_root_alt = vfs_root.replace("/tmp", "/private/tmp");

    cmd.arg("-D").arg(format!("WRITE_DIR={}", write_dir));
    cmd.arg("-D")
        .arg(format!("WRITE_DIR_ALT={}", write_dir_alt));
    cmd.arg("-D").arg(format!("VFS_ROOT={}", vfs_root));
    cmd.arg("-D").arg(format!("VFS_ROOT_ALT={}", vfs_root_alt));
    cmd.arg(program);

    // Set all temp-related env vars to the action's output directory so rustc uses it
    // for temp files instead of the system temp dir (which the sandbox blocks)
    cmd.env("TMPDIR", action.out_path.as_str());
    cmd.env("TMP", action.out_path.as_str());
    cmd.env("TEMP", action.out_path.as_str());
    cmd.env("TEMPDIR", action.out_path.as_str());

    tracing::trace!(
        sandbox_exec = "/usr/bin/sandbox-exec",
        write_dir = %action.out_path,
        vfs_root = %action.mount_base,
        tmpdir = %action.out_path,
        program,
        "spawning sandboxed command"
    );

    cmd
}

#[cfg(target_os = "linux")]
pub fn sandboxed_command(program: &str, action: &ActionContext) -> Command {
    tracing::trace!(
        program,
        vfs_root = %action.mount_base,
        write_dir = %action.out_path,
        "sandboxed_command: wrapping with landlock-exec"
    );

    // Try to find landlock-exec in the same directory as the current executable
    let landlock_exec = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent().map(|dir| dir.join("landlock-exec"))
        })
        .filter(|path| path.exists())
        .unwrap_or_else(|| "landlock-exec".into());

    tracing::trace!(
        landlock_exec = %landlock_exec.display(),
        "using landlock-exec binary"
    );

    let mut cmd = Command::new(landlock_exec);
    cmd.arg("--vfs-root")
        .arg(action.mount_base.as_str())
        .arg("--write-dir")
        .arg(action.out_path.as_str())
        .arg("--")
        .arg(program);

    tracing::trace!(
        landlock_exec = "landlock-exec",
        write_dir = %action.out_path,
        vfs_root = %action.mount_base,
        tmpdir = %action.out_path,
        program,
        "spawning sandboxed command"
    );

    cmd
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn sandboxed_command(program: &str, _action: &ActionContext) -> Command {
    tracing::trace!(program, "sandboxed_command: no sandbox (unsupported platform)");
    Command::new(program)
}
