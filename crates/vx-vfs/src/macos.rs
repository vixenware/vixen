//! macOS VFS startup and mount orchestration.
//!
//! This module relies on `fs-kitty`:
//! <https://github.com/bearcove/fs-kitty>
//!
//! `fs-kitty` implements a `roam` bridge between:
//! - an FSKit extension running in a sandboxed process, and
//! - `vx-vfs`, which accepts VFS connections over TCP.

use crate::types::{VfsConfig, VfsError, VfsStart};
use fs_kitty_proto::{Vfs, VfsDispatcher};
use roam_stream::{HandshakeConfig, accept};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};
use vx_io::net::{Endpoint, Listener};

const FSKITTY_APP_IDENTIFIER: &str = "me.amos.fs-kitty";
const FSKITTY_APP_PATH: &str = "/Applications/FsKitty.app";
const FSKITTY_MOUNT_HELPER_PATH: &str = "/Applications/FsKitty.app/Contents/Helpers/fskittymount";
const LOG_STREAM_TIMEOUT: Duration = Duration::from_secs(2);
const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LOG_CAPTURE_LINES: usize = 200;
const LOG_PREDICATE: &str = r#"subsystem == "com.apple.FSKit" OR subsystem == "me.amos.fs-kitty.ext" OR composedMessage CONTAINS "fskitty" OR composedMessage CONTAINS "FsKittyExt""#;

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tracing::debug!(mount_path = %config.mount_path, "entering macos vfs startup");

    let mut log_stream = start_log_stream().await?;

    tokio::fs::create_dir_all(config.mount_path.as_std_path())
        .await
        .map_err(|error| VfsError::CommandFailed {
            program: "mount",
            message: format!(
                "failed to create mount directory {}: {}",
                config.mount_path, error
            ),
        })?;
    tracing::debug!(mount_path = %config.mount_path, "ensured mount directory exists");

    let listener = Listener::bind(&Endpoint::tcp("127.0.0.1", 0))
        .await
        .map_err(|error| VfsError::CommandFailed {
            program: "mount",
            message: format!("failed to bind internal vfs listener: {}", error),
        })?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| VfsError::CommandFailed {
            program: "mount",
            message: format!("failed to query internal vfs listener address: {}", error),
        })?;
    tracing::debug!(%local_addr, "bound internal VFS listener");

    let (mount_ready_tx, mut mount_ready_rx) = mpsc::channel::<()>(1);
    spawn_accept_loop(listener, vfs, mount_ready_tx);

    let Endpoint::Tcp { port, .. } = local_addr else {
        let log_output = log_stream.snapshot().await;
        log_stream.stop().await;
        return Err(VfsError::CommandFailed {
            program: "fskittymount",
            message: format!(
                "internal listener did not bind to TCP; cannot extract TCP port for fskittymount\nlog stream output:\n{}",
                log_output
            ),
        });
    };

    tracing::debug!(%port, mount_path = %config.mount_path, "starting fskittymount helper");

    let helper_output = Arc::new(Mutex::new(VecDeque::new()));
    let mut helper_command = Command::new(FSKITTY_MOUNT_HELPER_PATH);
    helper_command
        .arg("--port")
        .arg(port.to_string())
        .arg("--mount-point")
        .arg(config.mount_path.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);

    let mut helper = match helper_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let diagnostics = collect_mount_failure_diagnostics().await;
            let log_output = log_stream.snapshot().await;
            log_stream.stop().await;
            return Err(VfsError::CommandFailed {
                program: "fskittymount",
                message: format!(
                    "failed to spawn {}: {}\n{}\nlog stream output:\n{}",
                    FSKITTY_MOUNT_HELPER_PATH, error, diagnostics, log_output
                ),
            });
        }
    };

    if let Some(stdout) = helper.stdout.take() {
        tokio::spawn(capture_log_stream(
            "fskittymount stdout",
            stdout,
            Arc::clone(&helper_output),
        ));
    }
    if let Some(stderr) = helper.stderr.take() {
        tokio::spawn(capture_log_stream(
            "fskittymount stderr",
            stderr,
            Arc::clone(&helper_output),
        ));
    }

    match wait_for_mount_ready(&mut helper, &mut mount_ready_rx, MOUNT_TIMEOUT).await {
        Ok(()) => {
            tracing::debug!(mount_path = %config.mount_path, "fskittymount became ready");
            log_stream.stop().await;
            Ok(VfsStart::Mounted {
                mount_path: config.mount_path,
            })
        }
        Err(readiness_error) => {
            let diagnostics = collect_mount_failure_diagnostics().await;
            let helper_output = snapshot_lines(&helper_output).await;
            let log_output = log_stream.snapshot().await;
            let _ = helper.kill().await;
            let _ = helper.wait().await;
            log_stream.stop().await;
            Err(VfsError::CommandFailed {
                program: "fskittymount",
                message: format!(
                    "{}\n{}\nhelper output:\n{}\nlog stream output:\n{}",
                    readiness_error, diagnostics, helper_output, log_output
                ),
            })
        }
    }
}

fn spawn_accept_loop<T>(listener: Listener, vfs: T, mount_ready_tx: mpsc::Sender<()>)
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    let _ = mount_ready_tx.try_send(());
                    let vfs = vfs.clone();
                    tokio::spawn(async move {
                        tracing::debug!(%peer_addr, "accepted fs-kitty VFS connection");
                        let dispatcher = VfsDispatcher::new(vfs);
                        let accepted = accept(stream, HandshakeConfig::default(), dispatcher).await;
                        let Ok((_handle, _incoming, driver)) = accepted else {
                            tracing::debug!(%peer_addr, "VFS handshake failed");
                            return;
                        };
                        if let Err(error) = driver.run().await {
                            tracing::debug!(%peer_addr, %error, "VFS session ended with error");
                        } else {
                            tracing::debug!(%peer_addr, "VFS session closed");
                        }
                    });
                }
                Err(error) => {
                    tracing::debug!(%error, "VFS accept failed");
                }
            }
        }
    });
}

async fn wait_for_mount_ready(
    helper: &mut Child,
    mount_ready_rx: &mut mpsc::Receiver<()>,
    timeout: Duration,
) -> Result<(), String> {
    let start = tokio::time::Instant::now();
    loop {
        if let Some(status) = helper
            .try_wait()
            .map_err(|e| format!("failed to poll fskittymount process: {e}"))?
        {
            return Err(format!(
                "fskittymount exited before mount became ready (status={:?})",
                status.code()
            ));
        }
        if mount_ready_rx.try_recv().is_ok() {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(format!(
                "timed out waiting for mount readiness after {:?}",
                timeout
            ));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn collect_mount_failure_diagnostics() -> String {
    let app_installed = tokio::fs::metadata(FSKITTY_APP_PATH).await.is_ok();
    let plugin_registered = check_plugin_registered().await;

    let mut notes = Vec::new();
    if !app_installed {
        notes.push(format!("diagnostic: {} is missing", FSKITTY_APP_PATH));
    }
    if app_installed && !plugin_registered {
        notes.push(format!(
            "diagnostic: plugin {} is not registered",
            FSKITTY_APP_IDENTIFIER
        ));
    }
    if notes.is_empty() {
        notes.push("diagnostic: no obvious app/plugin prerequisite issue found".to_string());
    }
    notes.join("\n")
}

async fn check_plugin_registered() -> bool {
    let mut command = Command::new("pluginkit");
    command.arg("-m").arg("-p").arg("com.apple.fskit.fsmodule");

    let output = match tokio::time::timeout(LOG_STREAM_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        _ => return false,
    };

    if !output.status.success() {
        return false;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains(FSKITTY_APP_IDENTIFIER)
}

struct LogStreamCapture {
    child: Child,
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl LogStreamCapture {
    async fn snapshot(&self) -> String {
        snapshot_lines(&self.lines).await
    }

    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

async fn start_log_stream() -> Result<LogStreamCapture, VfsError> {
    let mut command = Command::new("log");
    command
        .arg("stream")
        .arg("--info")
        .arg("--debug")
        .arg("--style")
        .arg("syslog")
        .arg("--predicate")
        .arg(LOG_PREDICATE)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| VfsError::CommandFailed {
        program: "log",
        message: error.to_string(),
    })?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let lines = Arc::new(Mutex::new(VecDeque::new()));

    tokio::spawn(capture_log_stream("stdout", stdout, Arc::clone(&lines)));
    tokio::spawn(capture_log_stream("stderr", stderr, Arc::clone(&lines)));

    tracing::debug!("started log stream capture");

    Ok(LogStreamCapture { child, lines })
}

async fn snapshot_lines(lines: &Arc<Mutex<VecDeque<String>>>) -> String {
    lines
        .lock()
        .await
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

async fn capture_log_stream<R>(source: &str, reader: R, lines: Arc<Mutex<VecDeque<String>>>)
where
    R: AsyncRead + Unpin,
{
    let mut reader_lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = reader_lines.next_line().await {
        let mut output = lines.lock().await;
        append_line_capped(&mut output, format!("[{}] {}", source, line));
    }
}

fn append_line_capped(output: &mut VecDeque<String>, line: String) {
    if output.len() >= MAX_LOG_CAPTURE_LINES {
        output.pop_front();
    }
    output.push_back(line);
}
