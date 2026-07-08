//! macOS VFS startup and mount orchestration.
//!
//! This module relies on the Vixen.app FSKit host in this repository (`apps/mac`).
//!
//! `VixenFS` owns a stable Vox `local://` listener inside the app group container.
//! `vx-vfs` mounts the extension, resolves the socket path from status.json,
//! then connects to that listener and serves `Vfs` on a prefixed provider lane.

use std::time::Duration;

use camino::Utf8Path;
use moire::sync::watch;
use tokio::process::Command;
use vox::{ConnectionSettings, Driver, Parity, ServerLogging, ServerLoggingOptions};
use vx_services::{Vfs, VfsClient, VfsDiskNotify, VfsDispatcher};

use crate::types::{VfsConfig, VfsError, VfsStart};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const WAIT_FOR_SERVICE: Duration = Duration::from_secs(10);
const STATUS_READ_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    let mount_path = &config.mount_path;

    // 1. Try reading status.json — if healthy, the extension is already mounted.
    //    If not, attempt a mount and read again.
    let status = match read_status(mount_path).await {
        Ok(status) => status,
        Err(read_err) => {
            // No live mount to reuse — bring it up via the Management daemon.
            do_mount(mount_path)
                .await
                .map_err(|msg| VfsError::MountFailed {
                    message: format!("{msg} (read_status first returned: {read_err})"),
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

    // 3. Connect to the extension's Vox listener, then claim the daemon provider prefix.
    let connection = vox::connect(&addr)
        .connect_timeout(CONNECT_TIMEOUT)
        .wait_for_service(WAIT_FOR_SERVICE)
        .establish()
        .await
        .map_err(|e| VfsError::MountFailed {
            message: format!("failed to connect to VixenFS at {addr}: {e}"),
            _diagnostics: vec![],
        })?;

    let provider_prefix = vx_vfs_types::MACOS_VFS_DAEMON_PROVIDER_PREFIX;
    let provider_metadata = vox::metadata()
        .str("vox-service", <VfsClient as vox::FromVoxLane>::SERVICE_NAME)
        .str("prefix", provider_prefix)
        .build();
    let provider_lane = connection
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            provider_metadata,
        )
        .await
        .map_err(|e| VfsError::MountFailed {
            message: format!(
                "failed to claim VixenFS provider prefix {provider_prefix:?} at {addr}: {e}"
            ),
            _diagnostics: vec![],
        })?;
    let mut driver = Driver::new(
        provider_lane,
        VfsDispatcher::new(vfs).with_middleware(ServerLogging::new(ServerLoggingOptions {
            log_metadata: true,
            ..Default::default()
        })),
    );
    tokio::spawn(async move {
        driver.run().await;
    });

    // 4. Spawn a task that watches for the session closing
    let (vfs_dead_tx, vfs_dead_rx) = watch::channel("vfs_dead", false);
    tokio::spawn(async move {
        connection.closed().await;
        tracing::warn!("VixenFS Vox session closed");
        let _ = vfs_dead_tx.send(true);
    });

    let access_path = config.mount_path.join(provider_prefix);
    Ok(VfsStart::Mounted {
        mount_path: config.mount_path,
        access_path,
        vfs_dead: vfs_dead_rx,
    })
}

/// Bring up the block-backed VixenFS volume by mounting it directly. The daemon runs as
/// the logged-in user, so it attaches the user-owned backing image and `/sbin/mount`s the
/// volume itself — there is no privileged management daemon anymore. The one-time
/// mountpoint creation under root-owned `/Volumes` must already have been done (the app
/// does it once via an admin prompt); if it hasn't, `establish` fails with an actionable
/// message. (When the volume is already mounted, `mount()` reuses it via `read_status`
/// and never calls this.)
async fn do_mount(_mount_path: &Utf8Path) -> Result<(), String> {
    use vx_management::mount as block;

    tokio::task::spawn_blocking(block::establish)
        .await
        .map_err(|e| format!("VixenFS mount task failed: {e}"))?
        .map_err(|e| format!("mounting VixenFS: {e:#}"))
}

async fn read_status(mount_path: &Utf8Path) -> Result<vx_fskit_types::StatusPayload, String> {
    let status_path = format!(
        "{mount_path}/{}",
        vx_vfs_types::VIXENFS_STATUS_RELATIVE_PATH
    );
    let output = tokio::time::timeout(STATUS_READ_TIMEOUT, {
        let mut command = Command::new("/bin/cat");
        command
            .arg(&status_path)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command.output()
    })
    .await
    .map_err(|_| {
        format!(
            "timed out after {STATUS_READ_TIMEOUT:?} reading {status_path}; child was killed, mount is likely zombied"
        )
    })?
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
