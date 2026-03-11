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
use camino::Utf8Path;
use moire::process::Command;
use moire::task::FutureExt;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use vx_store_proto::StoreProgress;

use crate::service_vfs::ActionContext;

/// Default timeout for compile actions (rustc, zig cc): 5 minutes.
const DEFAULT_COMPILE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Default timeout for link actions: 10 minutes (linking can be slow).
const DEFAULT_LINK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How often to send progress updates (if the last line changed).
const PROGRESS_TICK_INTERVAL: Duration = Duration::from_millis(250);

/// What kind of action is being executed (affects default timeout).
#[derive(Debug)]
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
#[derive(Debug)]
pub struct CommandOutput {
    pub exit_status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub combined_output: String,
}

/// Error from `run_sandboxed_command`.
#[derive(Debug)]
pub enum CommandError {
    /// Failed to spawn the child process.
    SpawnFailed(std::io::Error),
    /// The action exceeded its timeout and was killed.
    TimedOut {
        action_desc: String,
        timeout: Duration,
        partial_output: String,
    },
    /// The action was cancelled via a cancellation token.
    Cancelled {
        action_desc: String,
        partial_output: String,
    },
}

/// Spawn a sandboxed command, stream merged stdout/stderr with 250ms progress
/// ticks, and enforce a timeout. Kills the process group on timeout or cancellation.
///
/// `action_desc` is a human-readable description like "rustc foo" for error messages.
/// `progress` receives the latest output line every 250ms (if changed).
/// `cancel` is an optional cancellation token — if provided and triggered, the process
/// group is gracefully terminated (SIGTERM, then SIGKILL after 2s).
pub async fn run_sandboxed_command(
    mut cmd: Command,
    kind: &ActionKind,
    action_desc: &str,
    progress: roam::Tx<StoreProgress>,
    cancel: Option<tokio_util::sync::CancellationToken>,
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
            // On Linux: if vx-runner dies, kernel sends SIGKILL to this child.
            // Survives exec() through landlock-exec → rustc.
            #[cfg(target_os = "linux")]
            {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(CommandError::SpawnFailed)?;

    // Tell the reaper about this child's process group (pgid == pid from setpgid(0,0))
    #[cfg(target_os = "macos")]
    if let Some(pid) = child.id() {
        crate::register_child_pgroup(pid);
    }

    // Take ownership of stdout/stderr handles
    let stdout = child.take_stdout().expect("stdout was piped");
    let stderr = child.take_stderr().expect("stderr was piped");

    // Merge stdout and stderr into a single combined output buffer.
    // Also track the latest line for progress reporting.
    let combined_output = std::sync::Arc::new(moire::sync::SyncMutex::new(
        "runner/combined_output",
        String::new(),
    ));
    let stdout_output = std::sync::Arc::new(moire::sync::SyncMutex::new(
        "runner/stdout_output",
        String::new(),
    ));
    let stderr_output = std::sync::Arc::new(moire::sync::SyncMutex::new(
        "runner/stderr_output",
        String::new(),
    ));
    let latest_line = std::sync::Arc::new(moire::sync::SyncMutex::new(
        "runner/latest_line",
        String::new(),
    ));

    // Spawn readers for both streams
    let combined_for_stdout = combined_output.clone();
    let stdout_for_stdout = stdout_output.clone();
    let latest_for_stdout = latest_line.clone();
    let stdout_task = moire::task::spawn(
        async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end().to_string();
                        combined_for_stdout.lock().push_str(&line);
                        stdout_for_stdout.lock().push_str(&line);
                        if !trimmed.is_empty() {
                            *latest_for_stdout.lock() = trimmed;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        .named("sandbox_stdout"),
    );

    let combined_for_stderr = combined_output.clone();
    let stderr_for_stderr = stderr_output.clone();
    let latest_for_stderr = latest_line.clone();
    let stderr_task = moire::task::spawn(
        async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end().to_string();
                        combined_for_stderr.lock().push_str(&line);
                        stderr_for_stderr.lock().push_str(&line);
                        if !trimmed.is_empty() {
                            *latest_for_stderr.lock() = trimmed;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        .named("sandbox_stderr"),
    );

    // Progress tick task: every 250ms, send the latest line if it changed
    let latest_for_tick = latest_line.clone();
    let action_desc_owned = action_desc.to_string();
    let tick_task = moire::task::spawn(
        async move {
            let mut last_sent = String::new();
            let mut interval = tokio::time::interval(PROGRESS_TICK_INTERVAL);
            loop {
                interval.tick().await;
                let current = latest_for_tick.lock().clone();
                if current != last_sent && !current.is_empty() {
                    let _ = progress
                        .send(StoreProgress {
                            message: format!("{action_desc_owned}: {current}"),
                            current: None,
                            total: None,
                        })
                        .await;
                    last_sent = current;
                }
            }
        }
        .named("sandbox_tick"),
    );

    // Grab the PID before wait (child.id() returns None after exit)
    let pid = child.id();

    // Wait for child with timeout, plus optional cancellation
    enum Outcome {
        Exited(Result<ExitStatus, std::io::Error>),
        TimedOut,
        Cancelled,
    }

    let outcome = if let Some(ref cancel_token) = cancel {
        tokio::select! {
            biased;
            _ = cancel_token.cancelled() => Outcome::Cancelled,
            result = tokio::time::timeout(timeout, child.wait()) => {
                match result {
                    Ok(r) => Outcome::Exited(r),
                    Err(_) => Outcome::TimedOut,
                }
            }
        }
    } else {
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(r) => Outcome::Exited(r),
            Err(_) => Outcome::TimedOut,
        }
    };

    // Stop the tick task
    tick_task.abort();

    match outcome {
        Outcome::Exited(Ok(exit_status)) => {
            // Child exited — kill any orphaned children in the process group.
            // No-op if all children already exited.
            if let Some(pid) = pid {
                reap_process_group(pid);
            }
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            let stdout = stdout_output.lock().clone();
            let stderr = stderr_output.lock().clone();
            let combined = combined_output.lock().clone();
            Ok(CommandOutput {
                exit_status,
                stdout,
                stderr,
                combined_output: combined,
            })
        }
        Outcome::Exited(Err(io_err)) => {
            if let Some(pid) = pid {
                reap_process_group(pid);
            }
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            Err(CommandError::SpawnFailed(io_err))
        }
        Outcome::TimedOut => {
            // Timeout — gracefully terminate the process group (SIGTERM → SIGKILL)
            tracing::error!(
                action_desc,
                ?pid,
                timeout_secs = timeout.as_secs(),
                "action timed out, terminating process group"
            );

            if let Some(pid) = pid {
                terminate_process_group(pid).await;
            }
            let _ = child.kill();

            let _ = stdout_task.await;
            let _ = stderr_task.await;
            let partial = combined_output.lock().clone();

            Err(CommandError::TimedOut {
                action_desc: action_desc.to_string(),
                timeout,
                partial_output: partial,
            })
        }
        Outcome::Cancelled => {
            // Cancellation — graceful shutdown: SIGTERM → grace period → SIGKILL
            tracing::info!(
                action_desc,
                ?pid,
                "action cancelled, terminating process group"
            );

            if let Some(pid) = pid {
                terminate_process_group(pid).await;
            }
            let _ = child.kill();

            let _ = stdout_task.await;
            let _ = stderr_task.await;
            let partial = combined_output.lock().clone();

            Err(CommandError::Cancelled {
                action_desc: action_desc.to_string(),
                partial_output: partial,
            })
        }
    }
}

/// How long to wait between SIGTERM and SIGKILL when terminating a process group.
#[cfg(unix)]
const SIGTERM_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// Gracefully terminate a process group: SIGTERM first, then SIGKILL after a grace period.
#[cfg(unix)]
async fn terminate_process_group(pid: u32) {
    let pgid = pid as libc::pid_t;

    // Phase 1: SIGTERM — give the process group a chance to clean up
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }

    // Wait for the grace period, checking if the group leader is still alive
    let deadline = tokio::time::Instant::now() + SIGTERM_GRACE_PERIOD;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let alive = unsafe { libc::kill(pgid, 0) };
        if alive != 0 {
            // Process is gone (ESRCH), no need for SIGKILL
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }

    // Phase 2: SIGKILL — force kill the entire group
    tracing::warn!(
        pgid,
        "process group did not exit after SIGTERM, sending SIGKILL"
    );
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// Send SIGKILL to a process group immediately (no grace period).
/// Used after normal exit to reap any orphaned children.
#[cfg(unix)]
fn reap_process_group(pid: u32) {
    let pgid = pid as libc::pid_t;
    // Best-effort: kill any lingering children. Returns ESRCH if already gone.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
async fn terminate_process_group(_pid: u32) {}

#[cfg(not(unix))]
fn reap_process_group(_pid: u32) {}

/// The seatbelt profile (SBPL) used for all sandboxed subprocesses.
///
/// Security model:
/// - Base: inherits system.sb for process startup, dyld, system libraries
/// - Process exec: VFS_ROOT plus system tool directories (`/bin`, `/usr/bin`,
///   `/Library/Developer/CommandLineTools`)
/// - File reads/maps: restricted to VFS_ROOT plus system tool directories
/// - File writes: WRITE_DIR (action output) + /private/var/folders (temp files)
/// - Network: denied entirely — actions must be hermetic.
///
/// TODO: Replace system linker with hermetic Zig lld + mount system libraries in VFS
#[cfg(target_os = "macos")]
const SEATBELT_PROFILE_BASE: &str = r#"(version 1)
(debug deny)
(deny default)
(import "system.sb")
(allow process-fork)
(allow process-exec (subpath (param "VFS_ROOT")))
(allow process-exec (subpath (param "VFS_ROOT_ALT")))
(allow process-exec (subpath "/bin"))
(allow process-exec (subpath "/usr/bin"))
(allow process-exec (subpath "/Library/Developer/CommandLineTools"))
(allow file-read* (subpath (param "VFS_ROOT")))
(allow file-read* (subpath (param "VFS_ROOT_ALT")))
(allow file-read* (subpath "/bin"))
(allow file-read* (subpath "/usr/bin"))
(allow file-read* (subpath "/Library/Developer/CommandLineTools"))
(allow file-read* (subpath "/usr"))
(allow file-read* (subpath "/System"))
(allow file-read* (subpath "/Library"))
(allow file-read* (subpath "/private"))
(allow file-write* (subpath (param "WRITE_DIR")))
(allow file-write* (subpath (param "WRITE_DIR_ALT")))
(allow file-write* (subpath "/private/var/folders"))
(deny network*)"#;

#[cfg(target_os = "macos")]
fn sbpl_literal(path: &str) -> String {
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(target_os = "macos")]
fn host_exec_path_variants(path: &Utf8Path) -> Vec<String> {
    let original = path.as_str().to_string();
    let mut variants = vec![original.clone()];

    let private_tmp = original.replace("/tmp/", "/private/tmp/");
    if private_tmp != original {
        variants.push(private_tmp);
    }

    let private_var = original.replace("/var/", "/private/var/");
    if private_var != original {
        variants.push(private_var);
    }

    variants.sort();
    variants.dedup();
    variants
}

#[cfg(target_os = "macos")]
fn build_seatbelt_profile(action: &ActionContext) -> String {
    let mut profile = String::from(SEATBELT_PROFILE_BASE);
    for path in &action.allowed_host_exec_paths {
        for variant in host_exec_path_variants(path) {
            profile.push('\n');
            profile.push_str("(allow process-exec (literal ");
            profile.push_str(&sbpl_literal(&variant));
            profile.push_str("))");
            profile.push('\n');
            profile.push_str("(allow file-read* (literal ");
            profile.push_str(&sbpl_literal(&variant));
            profile.push_str("))");
        }
    }
    profile
}

/// Create a `Command` that runs `program` under the seatbelt sandbox.
///
/// On non-macOS platforms, returns `Command::new(program)` directly.
#[cfg(target_os = "macos")]
pub fn sandboxed_command(program: &str, action: &ActionContext) -> Command {
    let profile = build_seatbelt_profile(action);
    tracing::trace!(
        program,
        vfs_root = %action.mount_base,
        write_dir = %action.out_path,
        profile_len = profile.len(),
        "sandboxed_command: wrapping with sandbox-exec"
    );
    tracing::trace!(profile = %profile, "seatbelt profile");

    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(&profile);

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

    tracing::trace!(
        sandbox_exec = "/usr/bin/sandbox-exec",
        write_dir = %action.out_path,
        vfs_root = %action.mount_base,
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
        .and_then(|exe| exe.parent().map(|dir| dir.join("landlock-exec")))
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
    tracing::trace!(
        program,
        "sandboxed_command: no sandbox (unsupported platform)"
    );
    Command::new(program)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::{ActionContext, SEATBELT_PROFILE_BASE, build_seatbelt_profile};
    use super::{ActionKind, run_sandboxed_command};
    #[cfg(target_os = "macos")]
    use camino::Utf8PathBuf;
    use moire::process::Command;
    use roam::Tx;
    use vx_store_proto::StoreProgress;

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_allows_bin_execs() {
        assert!(SEATBELT_PROFILE_BASE.contains("(allow process-exec (subpath \"/bin\"))"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_allows_bin_reads() {
        assert!(SEATBELT_PROFILE_BASE.contains("(allow file-read* (subpath \"/bin\"))"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_allows_verified_host_tool_execs() {
        let action = ActionContext {
            prefix_id: "build-123".into(),
            mount_base: Utf8PathBuf::from("/tmp/vx/vfs"),
            prefix_path: Utf8PathBuf::from("/tmp/vx/vfs/build-123"),
            out_path: Utf8PathBuf::from("/tmp/vx/vfs/build-123/out"),
            allowed_host_exec_paths: vec![Utf8PathBuf::from("/tmp/tools/cc")],
        };
        let profile = build_seatbelt_profile(&action);
        assert!(profile.contains("(allow process-exec (literal \"/tmp/tools/cc\"))"));
        assert!(profile.contains("(allow file-read* (literal \"/tmp/tools/cc\"))"));
        assert!(profile.contains("(allow process-exec (literal \"/private/tmp/tools/cc\"))"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stdout_and_stderr_separately() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'stdout line\\n'; printf 'stderr line\\n' >&2");

        let output = run_sandboxed_command(
            cmd,
            &ActionKind::CcCompile,
            "capture-test",
            Tx::<StoreProgress>::unbound(),
            None,
        )
        .await
        .expect("command should run");

        assert_eq!(output.stdout, "stdout line\n");
        assert_eq!(output.stderr, "stderr line\n");
        assert!(output.combined_output.contains("stdout line\n"));
        assert!(output.combined_output.contains("stderr line\n"));
    }
}
