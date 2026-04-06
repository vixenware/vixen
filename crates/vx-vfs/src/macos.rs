//! macOS VFS startup and mount orchestration.
//!
//! This module relies on the Vixen.app FSKit host in this repository (`apps/mac`).
//!
//! `VixenFS` owns a stable Vox `local://` listener inside the app group container.
//! `vx-vfs` mounts the extension, resolves the app group path from status.json,
//! then connects to that listener and serves `Vfs` on one root connection.

use crate::types::{VfsConfig, VfsError, VfsStart};
use camino::Utf8Path;
use moire::process::Command;
use moire::sync::{mpsc, watch};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use vox::{NoopClient, ServerLogging, ServerLoggingOptions};
use vx_services::{Vfs, VfsDiskNotify, VfsDispatcher};
use vx_vfs_types::{VfsAction, VfsDiagnostic, VfsIssueCode};

const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
const VIXENFS_SOCKET_NAME: &str = "vixenfs.sock";
pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    tracing::debug!(mount_path = %config.mount_path, "entering macos vfs startup");

    let debug_mode = std::env::var("VIXENFS_DEBUG").as_deref() == Ok("1");
    let fskit_result = vx_fskit::setup(&config.mount_path, debug_mode).await;

    if let Some(ref error) = fskit_result.error {
        let log_detail = fskit_result
            .log
            .iter()
            .map(|entry| {
                let prefix = match entry.level {
                    vx_fskit::LogLevel::Ok => "\u{2713}",
                    vx_fskit::LogLevel::Info => "",
                    vx_fskit::LogLevel::Fail => "\u{2717}",
                };
                format!("{prefix} {}", entry.message)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(VfsError::MountFailed {
            message: error.summary.clone(),
            _diagnostics: vec![VfsDiagnostic {
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

    let app_group_root = resolve_app_group_path(&config.mount_path)
        .await
        .map_err(|message| VfsError::CommandFailed {
            program: "vx-vfs",
            message,
            _diagnostics: vec![],
        })?;

    let (mount_ready_tx, mut mount_ready_rx) = mpsc::channel("mount_ready", 1);
    let (vfs_dead_tx, vfs_dead_rx) = watch::channel("vfs_dead", false);

    tokio::spawn({
        let app_group_root = app_group_root.clone();
        let vfs = vfs.clone();
        async move {
            if let Err(error) = run_vox_session(vfs, app_group_root, mount_ready_tx).await {
                tracing::error!(%error, "macos vfs session failed");
            }
            let _ = vfs_dead_tx.send(true);
        }
    });

    match tokio::time::timeout(MOUNT_TIMEOUT, mount_ready_rx.recv()).await {
        Ok(Some(())) => {
            let mount_path = config.mount_path;
            let access_path = mount_path.clone();
            Ok(VfsStart::Mounted {
                mount_path,
                access_path,
                vfs_dead: vfs_dead_rx,
            })
        }
        Ok(None) => Err(VfsError::CommandFailed {
            program: "vx-vfs",
            message: "Vox readiness channel closed before extension accepted the VFS session"
                .to_string(),
            _diagnostics: vec![],
        }),
        Err(_) => Err(VfsError::CommandFailed {
            program: "vx-vfs",
            message: format!(
                "FSKit extension did not accept the VFS session within {MOUNT_TIMEOUT:?}"
            ),
            _diagnostics: vec![],
        }),
    }
}

async fn run_vox_session<T>(
    vfs: T,
    app_group_root: PathBuf,
    mount_ready_tx: mpsc::Sender<()>,
) -> Result<(), String>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    let addr = listener_addr(&app_group_root)?;
    let connect_deadline = Instant::now() + MOUNT_TIMEOUT;

    let client = loop {
        match vox::connect::<NoopClient>(&addr)
            .on_connection(
                VfsDispatcher::new(vfs.clone()).with_middleware(ServerLogging::new(
                    ServerLoggingOptions {
                        log_metadata: true,
                        ..Default::default()
                    },
                )),
            )
            .connect_timeout(CONNECT_ATTEMPT_TIMEOUT)
            .establish()
            .await
        {
            Ok(client) => {
                tracing::info!(%addr, "Vox root VFS session established to VixenFS listener");
                break client;
            }
            Err(error) if Instant::now() < connect_deadline => {
                tracing::debug!(%addr, %error, "waiting for VixenFS listener");
                sleep(CONNECT_RETRY_INTERVAL).await;
            }
            Err(error) => {
                return Err(format!(
                    "failed to connect to VixenFS listener {addr}: {error}"
                ));
            }
        }
    };

    tracing::info!(%addr, "VFS session established to VixenFS listener");
    let _ = mount_ready_tx.try_send(());
    client.caller.closed().await;
    tracing::warn!(%addr, "VFS session closed");
    Ok(())
}

fn listener_socket_path(app_group_root: &Path) -> Result<String, String> {
    let socket_path = app_group_root.join(VIXENFS_SOCKET_NAME);
    socket_path
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("non-utf8 app group socket path: {}", socket_path.display()))
}

fn listener_addr(app_group_root: &Path) -> Result<String, String> {
    Ok(format!("local://{}", listener_socket_path(app_group_root)?))
}

/// Resolve the App Group container path from the mounted VixenFS status payload.
pub async fn resolve_app_group_path(mount_path: &Utf8Path) -> Result<PathBuf, String> {
    let status_json = read_status_json(mount_path).await?;
    parse_app_group_path_from_status_json(&status_json)
}

fn parse_app_group_path_from_status_json(status_json: &str) -> Result<PathBuf, String> {
    let payload = facet_json::from_str::<vx_fskit::StatusPayload>(status_json)
        .map_err(|e| format!("failed to parse .vixenfs/status.json: {e}"))?;

    let path = payload
        .app_group_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            "status.json missing non-empty app_group_path after mount verification".to_string()
        })?;

    Ok(PathBuf::from(path))
}

async fn read_status_json(mount_path: &Utf8Path) -> Result<String, String> {
    let status_path = format!("{mount_path}/.vixenfs/status.json");
    let output = Command::new("/bin/cat")
        .arg(&status_path)
        .output()
        .await
        .map_err(|e| format!("failed to read {status_path}: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "reading {status_path} failed (status={:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    String::from_utf8(output.stdout)
        .map_err(|e| format!("{status_path} returned invalid UTF-8: {e}"))
}
