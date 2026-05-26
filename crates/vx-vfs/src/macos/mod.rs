//! macOS VFS startup and mount orchestration.
//!
//! This module relies on the Vixen.app FSKit host in this repository (`apps/mac`).
//!
//! `VixenFS` owns a stable Vox `local://` listener inside the app group container.
//! `vx-vfs` mounts the extension, resolves the socket path from status.json,
//! then connects to that listener and serves `Vfs` on one root connection.

use std::{io::ErrorKind, time::Duration};

use camino::Utf8Path;
use moire::process::Command;
use moire::sync::watch;
use vox::{NoopClient, ServerLogging, ServerLoggingOptions};
use vx_services::{Vfs, VfsDiskNotify, VfsDispatcher};

use crate::types::{VfsConfig, VfsError, VfsStart};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const WAIT_FOR_SERVICE: Duration = Duration::from_secs(10);

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    let mount_path = &config.mount_path;

    // 1. Try reading status.json — if healthy, the extension is already mounted.
    //    If not, attempt a mount and read again.
    let status = match read_status(mount_path).await {
        Ok(status) => status,
        Err(_) => {
            // Not mounted yet — try mounting
            do_mount(mount_path)
                .await
                .map_err(|msg| VfsError::MountFailed {
                    message: msg.clone(),
                    _diagnostics: vec![],
                })?;

            // Read status.json after mount
            read_status(mount_path)
                .await
                .map_err(|msg| VfsError::MountFailed {
                    message: format!("mounted successfully but cannot read status.json: {msg}"),
                    _diagnostics: vec![],
                })?
        }
    };

    // 2. Extract the socket path
    let socket_path = status
        .socket_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| VfsError::MountFailed {
            message: "status.json missing socket_path".to_string(),
            _diagnostics: vec![],
        })?;

    let addr = format!("local://{socket_path}");

    // 3. Connect to the extension's Vox listener
    let client = vox::connect::<NoopClient>(&addr)
        .on_connection(VfsDispatcher::new(vfs).with_middleware(ServerLogging::new(
            ServerLoggingOptions {
                log_metadata: true,
                ..Default::default()
            },
        )))
        .connect_timeout(CONNECT_TIMEOUT)
        .wait_for_service(WAIT_FOR_SERVICE)
        .establish()
        .await
        .map_err(|e| VfsError::MountFailed {
            message: format!("failed to connect to VixenFS at {addr}: {e}"),
            _diagnostics: vec![],
        })?;

    // 4. Spawn a task that watches for the session closing
    let (vfs_dead_tx, vfs_dead_rx) = watch::channel("vfs_dead", false);
    tokio::spawn(async move {
        client.caller.closed().await;
        tracing::warn!("VixenFS Vox session closed");
        let _ = vfs_dead_tx.send(true);
    });

    let access_path = config.mount_path.clone();
    Ok(VfsStart::Mounted {
        mount_path: config.mount_path,
        access_path,
        vfs_dead: vfs_dead_rx,
    })
}

async fn do_mount(mount_path: &Utf8Path) -> Result<(), String> {
    ensure_mount_directory(mount_path).await?;

    let source = "vixen://session";
    let output = Command::new("/sbin/mount")
        .args(["-F", "-t", "vixenfs", source, mount_path.as_str()])
        .output()
        .await
        .map_err(|e| format!("failed to run /sbin/mount: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "mount failed (exit {}): {}",
            output
                .status
                .code()
                .map_or("?".to_string(), |c| c.to_string()),
            stderr.trim()
        ));
    }
    Ok(())
}

async fn ensure_mount_directory(mount_path: &Utf8Path) -> Result<(), String> {
    if let Some(parent) = mount_path
        .parent()
        .filter(|parent| !parent.as_str().is_empty())
    {
        moire::fs::create_dir_all(parent.as_std_path())
            .await
            .map_err(|e| format!("failed to create mount parent directory {parent}: {e}"))?;
    }

    // `mkdir` on the final mountpoint is a safe probe for FSKit ghost mounts:
    // it returns EEXIST/EBUSY without issuing I/O through the mounted filesystem.
    match std::fs::create_dir(mount_path.as_std_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(format!(
            "failed to create mount directory {mount_path}: {error}"
        )),
    }
}

async fn read_status(mount_path: &Utf8Path) -> Result<vx_fskit_types::StatusPayload, String> {
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

    let json = String::from_utf8(output.stdout)
        .map_err(|e| format!("{status_path} returned invalid UTF-8: {e}"))?;

    facet_json::from_str(&json).map_err(|e| format!("failed to parse {status_path}: {e}"))
}
