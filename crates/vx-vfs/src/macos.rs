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
use tokio::sync::Mutex;
use vx_io::net::{Endpoint, Listener};

const FSKITTY_APP_IDENTIFIER: &str = "me.amos.fs-kitty";
const FSKITTY_APP_PATH: &str = "/Applications/FsKitty.app";
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

    let listener = Listener::bind(&Endpoint::Tcp("127.0.0.1:0".to_string()))
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

    spawn_accept_loop(listener, vfs);

    let Endpoint::Tcp(addr) = local_addr else {
        let log_output = log_stream.snapshot().await;
        log_stream.stop().await;
        return Err(VfsError::CommandFailed {
            program: "mount",
            message: format!(
                "internal listener did not bind to TCP; cannot build fskitty:// URL\nlog stream output:\n{}",
                log_output
            ),
        });
    };

    let url = format!("fskitty://{}", addr);
    tracing::debug!(%url, mount_path = %config.mount_path, "running mount command");

    let mount_output = {
        let mut command = Command::new("mount");
        command
            .arg("-t")
            .arg("fskitty")
            .arg(&url)
            .arg(config.mount_path.as_str());

        match tokio::time::timeout(MOUNT_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(VfsError::CommandFailed {
                program: "mount",
                message: error.to_string(),
            }),
            Err(_) => Err(VfsError::CommandTimedOut {
                program: "mount",
                timeout: MOUNT_TIMEOUT,
            }),
        }
    };
    let mount_result = match mount_output {
        Ok(output) => output,
        Err(VfsError::CommandTimedOut { .. }) => {
            let log_output = log_stream.snapshot().await;
            log_stream.stop().await;
            return Err(VfsError::CommandFailed {
                program: "mount",
                message: format!(
                    "mount command timed out after {:?}\nlog stream output:\n{}",
                    MOUNT_TIMEOUT, log_output
                ),
            });
        }
        Err(other) => {
            let log_output = log_stream.snapshot().await;
            log_stream.stop().await;
            return Err(VfsError::CommandFailed {
                program: "mount",
                message: format!("{}\nlog stream output:\n{}", other, log_output),
            });
        }
    };

    if mount_result.status.success() {
        tracing::debug!(mount_path = %config.mount_path, "mount command succeeded");
        log_stream.stop().await;
        return Ok(VfsStart::Mounted {
            mount_path: config.mount_path,
        });
    }

    let stderr = String::from_utf8_lossy(&mount_result.stderr)
        .trim()
        .to_string();
    let stdout = String::from_utf8_lossy(&mount_result.stdout)
        .trim()
        .to_string();
    let diagnostics = collect_mount_failure_diagnostics().await;
    let log_output = log_stream.snapshot().await;
    log_stream.stop().await;

    Err(VfsError::CommandFailed {
        program: "mount",
        message: format!(
            "mount exited with status {:?}\nstdout: {}\nstderr: {}\n{}\nlog stream output:\n{}",
            mount_result.status.code(),
            stdout,
            stderr,
            diagnostics,
            log_output
        ),
    })
}

fn spawn_accept_loop<T>(listener: Listener, vfs: T)
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
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
        self.lines
            .lock()
            .await
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
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
