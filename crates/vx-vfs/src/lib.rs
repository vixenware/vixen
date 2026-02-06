//! VFS orchestration for vx.
//!
//! This crate owns VFS server startup, platform checks, mount, and verification.

use cfg_if::cfg_if;
use vixenfs_proto::Vfs;

#[cfg(target_os = "macos")]
mod macos;
mod types;
pub use types::{VfsConfig, VfsError, VfsStart};

/// Mount VFS end-to-end (server startup, health checks, mount, verification).
///
/// Lifetime: once mounted, both the VFS server and mountpoint are intended to
/// remain active until process exit.
pub async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    tracing::debug!(mount_path = %config.mount_path, "starting vfs orchestration");

    cfg_if! {
        if #[cfg(target_os = "macos")] {
            tracing::debug!("dispatching to macos vfs startup");
            macos::mount(config, vfs).await
        } else {
            use std::env::consts::OS;
            tracing::debug!(platform = OS, "vfs unsupported on platform");
            Ok(VfsStart::Unsupported {
                platform: OS.to_string(),
            })
        }
    }
}
