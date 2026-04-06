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
use vox::{NoopClient, ServerLogging, ServerLoggingOptions};
use vx_services::{Vfs, VfsDiskNotify, VfsDispatcher};

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    // TODO: check `{mount_path}/.vixenfs/status.json`
    // no good? try `mount -F -t vixenfs vixen://session /tmp/vx/vfs` once
    // fails? print everything we know
    // Now try reading `{mount_path}/.vixenfs/status.json` again =>
    // it has the unix socket we have to connect to.

    // now use vox::connect with its built in connect/retry, no need for
    // loops or channels or anything. the address is `local://{whatever_is_in_status_json}`
    vox::connect::<NoopClient>(&addr)
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
