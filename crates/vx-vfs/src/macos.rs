//! macOS VFS startup and mount orchestration.
//!
//! This module relies on the Vixen.app FSKit host in this repository (`apps/mac`).
//!
//! `Vixen` implements a `roam` bridge between:
//! - an FSKit extension running in a sandboxed process, and
//! - `vx-vfs`, which serves VFS connections over SHM.

use crate::types::{VfsConfig, VfsError, VfsStart};
use roam_shm::bootstrap::{SessionId, SessionPaths, unix as shm_bootstrap};
use roam_shm::driver::MultiPeerHostDriver;
use roam_shm::{AddPeerOptions, SegmentConfig, ShmHost};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, watch};
use vixenfs_proto::{Vfs, VfsDispatcher};
use vx_vfs_types::{
    VIXEN_APP_PATH, VIXEN_DOWNLOAD_URL, VIXEN_EXTENSION_IDENTIFIER, VIXEN_MOUNT_HELPER_PATH,
    VfsAction, VfsDiagnostic, VfsHealth, VfsIssueCode,
};
const LOG_STREAM_TIMEOUT: Duration = Duration::from_secs(2);
const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_GC_AGE: Duration = Duration::from_secs(6 * 60 * 60);

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tracing::debug!(mount_path = %config.mount_path, "entering macos vfs startup");

    // Skip creating mount directory for /Volumes - FSKit/diskarbitrationd creates it
    if !config.mount_path.starts_with("/Volumes/") {
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
    } else {
        tracing::debug!(mount_path = %config.mount_path, "skipping mount directory creation for /Volumes (system creates it)");
    }

    let (mount_ready_tx, mut mount_ready_rx) = mpsc::channel::<()>(2);
    let (vfs_dead_tx, vfs_dead_rx) = watch::channel(false);
    let sid = spawn_shm_loop(vfs, mount_ready_tx, vfs_dead_tx)?;

    tracing::debug!(%sid, mount_path = %config.mount_path, "starting vixenfs-mount helper");

    let helper_output = Arc::new(Mutex::new(VecDeque::new()));
    let mut helper_command = Command::new(VIXEN_MOUNT_HELPER_PATH);
    helper_command
        .arg("--sid")
        .arg(&sid)
        .arg("--mount-point")
        .arg(config.mount_path.as_str());

    if std::env::var("VIXENFS_PRELOAD").as_deref() == Ok("1") {
        helper_command.arg("--preload");
    }
    if std::env::var("VIXENFS_DEBUG").as_deref() == Ok("1") {
        helper_command.arg("--debug");
    }

    helper_command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);

    let mut helper = match helper_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            cleanup_session_dir(&sid).await;
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

    // Stream helper stdout/stderr to tracing and capture for error reporting
    let stdout_task = if let Some(stdout) = helper.stdout.take() {
        let output_clone = helper_output.clone();
        Some(tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "vixenfs-mount", "stdout: {}", line);
                output_clone
                    .lock()
                    .await
                    .push_back(format!("stdout: {}", line));
            }
        }))
    } else {
        None
    };

    let stderr_task = if let Some(stderr) = helper.stderr.take() {
        let output_clone = helper_output.clone();
        Some(tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "vixenfs-mount", "stderr: {}", line);
                output_clone
                    .lock()
                    .await
                    .push_back(format!("stderr: {}", line));
            }
        }))
    } else {
        None
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
            cleanup_session_dir(&sid).await;
            let diagnostics = collect_mount_failure_diagnostics().await;
            let _ = helper.kill().await;
            let _ = helper.wait().await;

            // Wait for stdout/stderr readers to finish draining
            if let Some(task) = stdout_task {
                let _ = task.await;
            }
            if let Some(task) = stderr_task {
                let _ = task.await;
            }

            let helper_output = snapshot_lines(&helper_output).await;

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

fn spawn_shm_loop<T>(
    vfs: T,
    mount_ready_tx: mpsc::Sender<()>,
    vfs_dead_tx: watch::Sender<bool>,
) -> Result<String, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    let (sid_tx, sid_rx) = std::sync::mpsc::channel::<Result<String, String>>();

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
                let app_group_root = match resolve_app_group_path().await {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!("failed to resolve App Group path: {error}")));
                        return;
                    }
                };

                let sid = random_sid_hex();
                let session_id = match SessionId::parse(&sid) {
                    Ok(session_id) => session_id,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!("generated invalid sid {sid}: {error}")));
                        return;
                    }
                };
                let session_paths = match SessionPaths::new(&app_group_root, session_id.clone()) {
                    Ok(paths) => paths,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!("failed to build session paths: {error}")));
                        return;
                    }
                };

                if let Err(error) = gc_stale_sessions(&app_group_root, session_id.as_str(), SESSION_GC_AGE) {
                    tracing::warn!(error = %error, "failed to garbage-collect stale SHM sessions");
                }

                if let Err(error) = session_paths.validate_containment() {
                    let _ = sid_tx.send(Err(format!("invalid SHM session path containment: {error}")));
                    return;
                }

                if let Err(error) = session_paths.ensure_dirs() {
                    let _ = sid_tx.send(Err(format!("failed to create session directory: {error}")));
                    return;
                }

                let shm_path = session_paths.shm_path();
                if shm_path.exists()
                    && let Err(error) = std::fs::remove_file(&shm_path)
                {
                    let _ = sid_tx.send(Err(format!(
                        "failed to remove stale shm file {}: {error}",
                        shm_path.display()
                    )));
                    return;
                }

                let host = match ShmHost::create(&shm_path, SegmentConfig::default()) {
                    Ok(host) => host,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!(
                            "failed to create SHM hub {}: {error}",
                            shm_path.display()
                        )));
                        return;
                    }
                };

                let (driver, _handles, _incoming, driver_handle) =
                    MultiPeerHostDriver::builder(host).build();

                let control_sock_path =
                    control_socket_path_for_sid(&app_group_root, session_paths.sid().as_str());
                if control_sock_path.exists()
                    && let Err(error) = std::fs::remove_file(&control_sock_path)
                {
                    let _ = sid_tx.send(Err(format!(
                        "failed to remove stale SHM control socket {}: {error}",
                        control_sock_path.display()
                    )));
                    return;
                }

                let control_listener = match tokio::net::UnixListener::bind(&control_sock_path) {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = sid_tx.send(Err(format!(
                            "failed to bind SHM control socket {}: {error}",
                            control_sock_path.display()
                        )));
                        return;
                    }
                };

                let _ = sid_tx.send(Ok(sid.clone()));

                let vfs_dead_tx_for_driver = vfs_dead_tx.clone();
                tokio::spawn(async move {
                    if let Err(error) = driver.run().await {
                        tracing::debug!(%error, "SHM host driver exited with error");
                    } else {
                        tracing::debug!("SHM host driver exited cleanly");
                    }
                    let _ = vfs_dead_tx_for_driver.send(true);
                });

                loop {
                    let ticket = match driver_handle.create_peer(AddPeerOptions::default()).await {
                        Ok(ticket) => ticket,
                        Err(error) => {
                            tracing::warn!(%error, "failed to reserve SHM peer ticket");
                            continue;
                        }
                    };

                    let peer_id = ticket.peer_id;
                    let hub_path = ticket.hub_path.clone();
                    let doorbell_fd = ticket.doorbell_handle().as_raw_fd();

                    if let Err(error) = driver_handle
                        .add_peer(peer_id, VfsDispatcher::new(vfs.clone()))
                        .await
                    {
                        tracing::warn!(%error, peer = %peer_id.get(), "failed to add SHM peer to driver");
                        if let Err(release_error) = driver_handle.release_peer(peer_id).await {
                            tracing::warn!(%release_error, peer = %peer_id.get(), "failed to release SHM peer after add-peer failure");
                        }
                        continue;
                    }

                    match shm_bootstrap::accept_and_send_ticket(
                        &control_listener,
                        session_paths.sid(),
                        peer_id,
                        &hub_path,
                        doorbell_fd,
                    )
                    .await
                    {
                        Ok(()) => {
                            let _ = mount_ready_tx.try_send(());
                            tracing::debug!(sid = %session_paths.sid(), peer = %peer_id.get(), "issued SHM bootstrap ticket");
                        }
                        Err(error) => {
                            tracing::warn!(%error, peer = %peer_id.get(), "failed SHM bootstrap ticket exchange");
                            if let Err(release_error) = driver_handle.release_peer(peer_id).await {
                                tracing::warn!(%release_error, peer = %peer_id.get(), "failed to release SHM peer after bootstrap failure");
                            }
                        }
                    }
                }
            });
        })
        .expect("failed to spawn VFS runtime thread");

    sid_rx
        .recv()
        .unwrap_or(Err("VFS runtime thread died before sending sid".to_string()))
        .map_err(|msg| VfsError::CommandFailed {
            program: "mount",
            message: msg,
            _diagnostics: vec![],
        })
}

fn random_sid_hex() -> String {
    let mut sid = String::with_capacity(32);
    for _ in 0..32 {
        let nibble = fastrand::u8(0..16);
        let ch = match nibble {
            0..=9 => (b'0' + nibble) as char,
            _ => (b'a' + (nibble - 10)) as char,
        };
        sid.push(ch);
    }
    sid
}

fn gc_stale_sessions(root: &Path, keep_sid: &str, max_age: Duration) -> Result<(), String> {
    let sessions_root = root.join("sessions");
    if !sessions_root.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(&sessions_root)
        .map_err(|error| format!("failed to list {}: {error}", sessions_root.display()))?;
    let now = std::time::SystemTime::now();

    for entry in entries {
        let entry = entry.map_err(|error| format!("failed to read sessions entry: {error}"))?;
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == keep_sid {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < max_age {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(&path) {
            tracing::warn!(path = %path.display(), %error, "failed to remove stale SHM session");
        }
    }
    Ok(())
}

fn control_socket_path_for_sid(root: &Path, sid: &str) -> PathBuf {
    root.join(format!("c-{sid}.sock"))
}

async fn cleanup_session_dir(sid: &str) {
    let app_group_root = match resolve_app_group_path().await {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%sid, %error, "failed to resolve App Group path for SHM cleanup");
            return;
        }
    };
    let control_sock_path = control_socket_path_for_sid(&app_group_root, sid);
    match tokio::fs::remove_file(&control_sock_path).await {
        Ok(()) => tracing::debug!(
            %sid,
            path = %control_sock_path.display(),
            "removed SHM control socket"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            %sid,
            path = %control_sock_path.display(),
            %error,
            "failed to remove SHM control socket"
        ),
    }

    let session_id = match SessionId::parse(sid) {
        Ok(session_id) => session_id,
        Err(error) => {
            tracing::warn!(%sid, %error, "invalid sid during SHM cleanup");
            return;
        }
    };
    let session_paths = match SessionPaths::new(&app_group_root, session_id) {
        Ok(paths) => paths,
        Err(error) => {
            tracing::warn!(%sid, %error, "failed to construct SHM cleanup paths");
            return;
        }
    };
    if let Err(error) = session_paths.validate_containment() {
        tracing::warn!(%sid, %error, "refusing SHM cleanup outside App Group root");
        return;
    }
    let session_dir = session_paths.session_dir();
    match tokio::fs::remove_dir_all(&session_dir).await {
        Ok(()) => {
            tracing::debug!(%sid, path = %session_dir.display(), "removed SHM session directory");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                %sid,
                path = %session_dir.display(),
                %error,
                "failed to remove SHM session directory"
            );
        }
    }
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
