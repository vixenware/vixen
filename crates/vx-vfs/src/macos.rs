//! macOS VFS startup and mount orchestration.
//!
//! This module relies on the Vixen.app FSKit host in this repository (`apps/mac`).
//!
//! `Vixen` implements a `roam` bridge between:
//! - an FSKit extension running in a sandboxed process, and
//! - `vx-vfs`, which serves VFS connections over SHM.

use crate::types::{VfsConfig, VfsError, VfsStart};
use moire::process::Command;
use moire::{Sender, WatchSender};
use roam_shm::bootstrap::{SessionId, SessionPaths, unix as shm_bootstrap};
use roam_shm::driver::MultiPeerHostDriver;
use roam_shm::{AddPeerOptions, SegmentConfig, ShmHost};
use std::path::{Path, PathBuf};
use std::time::Duration;
use vixenfs_proto::{Vfs, VfsDispatcher};
use vx_vfs_types::{VIXEN_MOUNT_HELPER_PATH, VfsAction, VfsDiagnostic, VfsIssueCode};
const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_GC_AGE: Duration = Duration::from_secs(6 * 60 * 60);
const moire_CX: moire::moireContext = moire::moireContext::new(env!("CARGO_MANIFEST_DIR"));

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tracing::debug!(mount_path = %config.mount_path, "entering macos vfs startup");

    let (mount_ready_tx, mut mount_ready_rx) = moire::channel!("mount_ready", 2);
    let (vfs_dead_tx, vfs_dead_rx) = moire::watch!("vfs_dead", false);
    let sid = spawn_shm_loop(vfs, mount_ready_tx, vfs_dead_tx)?;

    let debug_mode = std::env::var("VIXENFS_DEBUG").as_deref() == Ok("1");

    tracing::debug!(%sid, mount_path = %config.mount_path, debug_mode, "calling vx_fskit::setup");

    // vx_fskit::setup handles: prerequisites, stale mount cleanup, mkdir,
    // /sbin/mount -F -t vixenfs, and health.json verification.
    let fskit_result = vx_fskit::setup(&sid, &config.mount_path, debug_mode).await;

    if let Some(ref error) = fskit_result.error {
        cleanup_session_dir(&sid).await;
        let log_detail = fskit_result
            .log
            .iter()
            .map(|entry| {
                let prefix = match entry.level {
                    vx_fskit::LogLevel::Ok => "\u{2713}", // ✓
                    vx_fskit::LogLevel::Info => "",
                    vx_fskit::LogLevel::Fail => "\u{2717}", // ✗
                };
                format!("{prefix} {}", entry.message)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(VfsError::MountFailed {
            message: error.summary.clone(),
            diagnostics: vec![VfsDiagnostic {
                code: VfsIssueCode::MountFailed,
                summary: error.summary.clone(),
                detail: Some(log_detail),
                actions: error
                    .remediation
                    .iter()
                    .map(|r| VfsAction::RunCommand(r.clone()))
                    .collect(),
            }],
        });
    }

    // Mount syscall succeeded and health.json confirms the extension is up.
    // Now wait for the SHM connection (extension connecting to our SHM segment).
    tracing::debug!("fskit mount ok, waiting for SHM readiness");

    match tokio::time::timeout(MOUNT_TIMEOUT, mount_ready_rx.recv_with_cx(moire_CX)).await {
        Ok(Some(())) => {
            tracing::info!(mount_path = %config.mount_path, "VFS mounted and SHM connected");
            Ok(VfsStart::Mounted {
                mount_path: config.mount_path,
                vfs_dead: vfs_dead_rx,
            })
        }
        Ok(None) => {
            cleanup_session_dir(&sid).await;
            Err(VfsError::CommandFailed {
                program: "vx-vfs",
                message: "SHM readiness channel closed before extension connected".to_string(),
                _diagnostics: vec![],
            })
        }
        Err(_) => {
            cleanup_session_dir(&sid).await;
            Err(VfsError::CommandFailed {
                program: "vx-vfs",
                message: format!(
                    "FSKit extension did not connect via SHM within {MOUNT_TIMEOUT:?}"
                ),
                _diagnostics: vec![],
            })
        }
    }
}

fn spawn_shm_loop<T>(
    vfs: T,
    mount_ready_tx: Sender<()>,
    vfs_dead_tx: WatchSender<bool>,
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

                if let Err(error) = gc_stale_sessions(&app_group_root, session_id.as_str(), SESSION_GC_AGE).await {
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
                if moire::fs::try_exists(&shm_path).await.unwrap_or(false)
                    && let Err(error) = moire::fs::remove_file(&shm_path).await
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
                if moire::fs::try_exists(&control_sock_path)
                    .await
                    .unwrap_or(false)
                    && let Err(error) = moire::fs::remove_file(&control_sock_path).await
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

                moire::spawn_tracked!("shm_host_driver", async move {
                    if let Err(error) = driver.run().await {
                        tracing::debug!(%error, "SHM host driver exited with error");
                    } else {
                        tracing::debug!("SHM host driver exited cleanly");
                    }
                    let _ = vfs_dead_tx.send_with_cx(true, moire_CX);
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

async fn gc_stale_sessions(root: &Path, keep_sid: &str, max_age: Duration) -> Result<(), String> {
    let sessions_root = root.join("sessions");
    if !moire::fs::try_exists(&sessions_root).await.unwrap_or(false) {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&sessions_root)
        .await
        .map_err(|error| format!("failed to list {}: {error}", sessions_root.display()))?;
    let now = std::time::SystemTime::now();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("failed to read sessions entry: {error}"))?
    {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == keep_sid {
            continue;
        }
        let metadata = match tokio::fs::symlink_metadata(&path).await {
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
        if let Err(error) = moire::fs::remove_dir_all(&path).await {
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
    match moire::fs::remove_file(&control_sock_path).await {
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
    match moire::fs::remove_dir_all(&session_dir).await {
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

/// Resolve the App Group container path via the `vixenfs-mount` helper.
///
/// The helper is codesigned with the App Group entitlement, so it can call
/// `FileManager.containerURL(forSecurityApplicationGroupIdentifier:)` and
/// return the resolved path. This lets VXRunner (a plain Rust binary) discover
/// the shared container without needing its own App Group entitlement.
pub async fn resolve_app_group_path() -> Result<PathBuf, String> {
    let output = Command::new(VIXEN_MOUNT_HELPER_PATH)
        .arg("--app-group-path")
        .output_with_cx(moire_CX)
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
