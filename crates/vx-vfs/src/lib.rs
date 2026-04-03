//! VFS orchestration for vx.
//!
//! This crate owns VFS server startup, platform checks, mount, and verification.

use cfg_if::cfg_if;
use vx_services::{Vfs, VfsDiskNotify};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod types;
#[cfg(target_os = "windows")]
mod windows;

pub use types::{VfsConfig, VfsError, VfsStart};
#[cfg(target_os = "macos")]
pub use vx_vfs_types::{VIXEN_APP_PATH, VfsHealth};
pub use vx_vfs_types::{VfsAction, VfsDiagnostic, VfsIssueCode};

/// Mount VFS end-to-end (server startup, health checks, mount, verification).
///
/// Lifetime: once mounted, both the VFS server and mountpoint are intended to
/// remain active until process exit.
pub async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    cfg_if! {
        if #[cfg(target_os = "macos")] {
            tracing::debug!(mount_path = %config.mount_path, "starting vfs orchestration");
            macos::mount(config, vfs).await
        } else if #[cfg(target_os = "linux")] {
            tracing::debug!(mount_path = %config.mount_path, "starting FUSE vfs");
            linux::mount(config, vfs).await
        } else if #[cfg(target_os = "windows")] {
            tracing::debug!(mount_path = %config.mount_path, "starting ProjFS vfs");
            windows::mount(config, vfs).await
        } else {
            _ = (config, vfs);
            use std::env::consts::OS;
            tracing::debug!(platform = OS, "vfs unsupported on platform");
            Ok(VfsStart::Unsupported {
                platform: OS.to_string(),
            })
        }
    }
}
