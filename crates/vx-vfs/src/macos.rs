//! macOS VFS startup and mount orchestration.
//!
//! This module relies on the Vixen.app FSKit host in this repository (`apps/mac`).
//!
//! `Vixen` implements a `roam` bridge between:
//! - an FSKit extension running in a sandboxed process, and
//! - `vx-vfs`, which accepts VFS connections over TCP.

use crate::types::{VfsConfig, VfsError, VfsStart};
use roam_stream::{HandshakeConfig, accept};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, watch};
use vixenfs_proto::{Vfs, VfsDispatcher};
use vx_io::net::{Endpoint, Listener};
use vx_vfs_types::{
    VIXEN_APP_PATH, VIXEN_DOWNLOAD_URL, VIXEN_EXTENSION_IDENTIFIER, VIXEN_MOUNT_HELPER_PATH,
    VfsAction, VfsDiagnostic, VfsHealth, VfsIssueCode,
};
const LOG_STREAM_TIMEOUT: Duration = Duration::from_secs(2);
const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tracing::debug!(mount_path = %config.mount_path, "entering macos vfs startup");

    tokio::fs::create_dir_all(config.mount_path.as_std_path())
        .await
        .map_err(|error| VfsError::CommandFailed {
            program: "mount",
            message: format!(
                "failed to create mount directory {}: {}",
                config.mount_path, error
            ),
            _diagnostics: vec![],
        })?;
    tracing::debug!(mount_path = %config.mount_path, "ensured mount directory exists");

    let (mount_ready_tx, mut mount_ready_rx) = mpsc::channel::<()>(1);
    let (vfs_dead_tx, vfs_dead_rx) = watch::channel(false);
    let port = spawn_accept_loop(vfs, mount_ready_tx, vfs_dead_tx)?;

    tracing::debug!(%port, mount_path = %config.mount_path, "starting vixenfs-mount helper");

    let helper_output = Arc::new(Mutex::new(VecDeque::new()));
    let mut helper_command = Command::new(VIXEN_MOUNT_HELPER_PATH);
    helper_command
        .arg("--port")
        .arg(port.to_string())
        .arg("--mount-point")
        .arg(config.mount_path.as_str());

    if std::env::var("VIXENFS_PRELOAD").as_deref() == Ok("1") {
        helper_command.arg("--preload");
    }

    helper_command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);

    let mut helper = match helper_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let diagnostics = collect_mount_failure_diagnostics().await;
            return Err(VfsError::CommandFailed {
                program: "vixenfs-mount",
                message: format!("failed to spawn {}: {}", VIXEN_MOUNT_HELPER_PATH, error),
                _diagnostics: with_mount_context_diagnostic(
                    diagnostics,
                    "mount helper failed to start",
                    format!("failed to spawn {}: {}", VIXEN_MOUNT_HELPER_PATH, error),
                ),
            });
        }
    };

    match wait_for_mount_ready(&mut helper, &mut mount_ready_rx, MOUNT_TIMEOUT).await {
        Ok(()) => {
            tracing::info!(mount_path = %config.mount_path, "vixenfs-mount became ready");
            Ok(VfsStart::Mounted {
                mount_path: config.mount_path,
                vfs_dead: vfs_dead_rx,
            })
        }
        Err(readiness_error) => {
            let diagnostics = collect_mount_failure_diagnostics().await;
            let helper_output = snapshot_lines(&helper_output).await;
            let _ = helper.kill().await;
            let _ = helper.wait().await;
            let mount_detail = if helper_output.is_empty() {
                readiness_error.clone()
            } else {
                format!("{}\nhelper output:\n{}", readiness_error, helper_output)
            };
            Err(VfsError::CommandFailed {
                program: "vixenfs-mount",
                message: readiness_error,
                _diagnostics: with_mount_context_diagnostic(
                    diagnostics,
                    "mount helper did not become ready",
                    mount_detail,
                ),
            })
        }
    }
}

/// Run the VFS accept loop on a dedicated tokio runtime.
///
/// Compilation tasks block tokio worker threads in `posix_spawn` (launching
/// rustc/zig). If the VFS accept loop shares that runtime, there may be no
/// workers left to service VFS read requests — which rustc needs to proceed.
/// A dedicated runtime guarantees VFS responsiveness regardless of how many
/// compilations are in flight.
///
/// The listener is bound on the VFS runtime (not the caller's runtime) so the
/// underlying `TcpListener` is registered with the correct reactor.
fn spawn_accept_loop<T>(
    vfs: T,
    mount_ready_tx: mpsc::Sender<()>,
    vfs_dead_tx: watch::Sender<bool>,
) -> Result<u16, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    // Use a oneshot to get the bound port (or error) back from the VFS runtime.
    let (port_tx, port_rx) = std::sync::mpsc::channel::<Result<u16, String>>();

    std::thread::Builder::new()
        .name("vfs-runtime".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_name("vfs-worker")
                .enable_all()
                .build()
                .expect("failed to create VFS tokio runtime");
            rt.block_on(async move {
                let listener = match Listener::bind(&Endpoint::tcp("127.0.0.1", 0)).await {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = port_tx.send(Err(format!("failed to bind VFS listener: {e}")));
                        return;
                    }
                };
                let local_addr = match listener.local_addr() {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = port_tx.send(Err(format!("failed to query VFS listener address: {e}")));
                        return;
                    }
                };
                let Endpoint::Tcp { port, .. } = local_addr else {
                    let _ = port_tx.send(Err("VFS listener did not bind to TCP".to_string()));
                    return;
                };
                tracing::debug!(%port, "VFS listener bound on dedicated runtime");
                let _ = port_tx.send(Ok(port));

                loop {
                    match listener.accept().await {
                        Ok((stream, peer_addr)) => {
                            let _ = mount_ready_tx.try_send(());
                            let vfs = vfs.clone();
                            let vfs_dead_tx = vfs_dead_tx.clone();
                            tokio::spawn(async move {
                                tracing::debug!(%peer_addr, "accepted Vixen VFS connection");
                                let dispatcher = VfsDispatcher::new(vfs);
                                let config = HandshakeConfig {
                                    max_payload_size: 256 * 1024 * 1024, // 256 MiB — large enough for librustc_driver
                                    ..Default::default()
                                };
                                let accepted = accept(stream, config, dispatcher).await;
                                let Ok((_handle, _incoming, driver)) = accepted else {
                                    tracing::debug!(%peer_addr, "VFS handshake failed");
                                    let _ = vfs_dead_tx.send(true);
                                    return;
                                };
                                if let Err(error) = driver.run().await {
                                    tracing::debug!(%peer_addr, %error, "VFS session ended with error");
                                } else {
                                    tracing::debug!(%peer_addr, "VFS session closed");
                                }
                                let _ = vfs_dead_tx.send(true);
                            });
                        }
                        Err(error) => {
                            tracing::debug!(%error, "VFS accept failed");
                        }
                    }
                }
            });
        })
        .expect("failed to spawn VFS runtime thread");

    port_rx
        .recv()
        .unwrap_or(Err(
            "VFS runtime thread died before sending port".to_string()
        ))
        .map_err(|msg| VfsError::CommandFailed {
            program: "mount",
            message: msg,
            _diagnostics: vec![],
        })
}

async fn wait_for_mount_ready(
    helper: &mut Child,
    mount_ready_rx: &mut mpsc::Receiver<()>,
    timeout: Duration,
) -> Result<(), String> {
    tokio::select! {
        wait_result = helper.wait() => {
            let status = wait_result
                .map_err(|e| format!("failed to wait for vixenfs-mount process: {e}"))?;
            Err(format!(
                "failed to mount (status={:?})",
                status.code()
            ))
        }
        ready = mount_ready_rx.recv() => {
            match ready {
                Some(()) => Ok(()),
                None => Err("mount readiness channel closed before mount became ready".to_string()),
            }
        }
        _ = tokio::time::sleep(timeout) => {
            Err(format!(
                "timed out waiting for mount readiness after {:?}",
                timeout
            ))
        }
    }
}

async fn collect_mount_failure_diagnostics() -> Vec<VfsDiagnostic> {
    let app_installed = is_app_installed().await;
    let health = check_extension_health().await;

    let mut diagnostics = Vec::new();
    if !app_installed {
        diagnostics.push(VfsDiagnostic {
            code: VfsIssueCode::AppMissing,
            summary: format!("{} is missing", VIXEN_APP_PATH),
            detail: None,
            actions: vec![
                VfsAction::OpenDownloadUrl(VIXEN_DOWNLOAD_URL.to_string()),
                VfsAction::RetryBuild,
            ],
        });
    }

    if app_installed {
        match health {
            Some(health) if !health.extension_installed => {
                diagnostics.push(VfsDiagnostic {
                    code: VfsIssueCode::ExtensionNotInstalled,
                    summary: format!("plugin {} is not registered", VIXEN_EXTENSION_IDENTIFIER),
                    detail: None,
                    actions: vec![VfsAction::RunVxApp, VfsAction::RetryBuild],
                });
            }
            Some(health) if !health.extension_enabled => {
                diagnostics.push(VfsDiagnostic {
                    code: VfsIssueCode::ExtensionDisabled,
                    summary: format!(
                        "plugin {} is installed but disabled",
                        VIXEN_EXTENSION_IDENTIFIER
                    ),
                    detail: None,
                    actions: vec![VfsAction::RunVxApp, VfsAction::RetryBuild],
                });
            }
            Some(_) => {}
            None => {
                diagnostics.push(VfsDiagnostic {
                    code: VfsIssueCode::HealthCheckFailed,
                    summary: "failed to read extension health from vixenfs-mount --health --json"
                        .to_string(),
                    detail: None,
                    actions: vec![VfsAction::RetryBuild],
                });
            }
        }
    }

    if diagnostics.is_empty() {
        diagnostics.push(VfsDiagnostic {
            code: VfsIssueCode::MountFailed,
            summary: r#"App here, extension enabled, but mounting failed. You can try the following:

 $ log stream --info --debug --predicate 'subsystem == "com.apple.FSKit" OR message CONTAINS "vixenfs"'

 And sometimes, killing fskit fixes it

 $ sudo killall fskitd fskit_agent

 Yeah, FSKit is wonky. File an issue against http://github.com/bearcove/vixen if necessary.
                "#.to_string(),
            detail: None,
            actions: vec![VfsAction::RetryBuild],
        });
    }
    diagnostics
}

pub(super) async fn is_app_installed() -> bool {
    tokio::fs::metadata(VIXEN_APP_PATH).await.is_ok()
}

pub(super) async fn check_extension_health() -> Option<VfsHealth> {
    let mut command = Command::new(VIXEN_MOUNT_HELPER_PATH);
    command.arg("--health").arg("--json");

    let output = match tokio::time::timeout(LOG_STREAM_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        _ => return None,
    };

    let stdout = std::str::from_utf8(&output.stdout).ok()?.trim();
    facet_json::from_str(stdout).ok()
}

/// Resolve the App Group container path via the `vixenfs-mount` helper.
///
/// The helper is codesigned with the App Group entitlement, so it can call
/// `FileManager.containerURL(forSecurityApplicationGroupIdentifier:)` and
/// return the resolved path. This lets VXRunner (a plain Rust binary) discover
/// the shared container without needing its own App Group entitlement.
#[allow(dead_code)]
pub async fn resolve_app_group_path() -> Result<PathBuf, String> {
    let output = Command::new(VIXEN_MOUNT_HELPER_PATH)
        .arg("--app-group-path")
        .output()
        .await
        .map_err(|e| format!("failed to run {VIXEN_MOUNT_HELPER_PATH}: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "vixenfs-mount --app-group-path failed (status={:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let path = String::from_utf8(output.stdout)
        .map_err(|e| format!("vixenfs-mount --app-group-path returned invalid UTF-8: {e}"))?;
    let path = path.trim();
    if path.is_empty() {
        return Err("vixenfs-mount --app-group-path returned empty path".to_string());
    }

    Ok(PathBuf::from(path))
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

fn with_mount_context_diagnostic(
    mut diagnostics: Vec<VfsDiagnostic>,
    summary: &str,
    detail: String,
) -> Vec<VfsDiagnostic> {
    if let Some(existing) = diagnostics
        .iter_mut()
        .find(|diagnostic| diagnostic.code == VfsIssueCode::MountFailed)
    {
        existing.detail = Some(match existing.detail.take() {
            Some(existing_detail) if !existing_detail.is_empty() => {
                format!("{}\n\n{}", existing_detail, detail)
            }
            _ => detail,
        });
        return diagnostics;
    }

    diagnostics.push(VfsDiagnostic {
        code: VfsIssueCode::MountFailed,
        summary: summary.to_string(),
        detail: Some(detail),
        actions: vec![VfsAction::RetryBuild],
    });
    diagnostics
}
