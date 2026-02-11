//! Windows VFS backend using ProjFS (Projected File System).
//!
//! Maps the `Vfs` trait to the `windows_projfs::ProjectedFileSystemSource` trait,
//! providing a ProjFS mount backed by any `Vfs` implementation.

use std::io::{self, Cursor, Read};
use std::ops::ControlFlow;
use std::path::Path;

use vixenfs_proto::{ItemType, Vfs};
use windows_projfs::{
    DirectoryEntry, DirectoryInfo, FileInfo, Notification, ProjectedFileSystem,
    ProjectedFileSystemSource,
};

use crate::types::{VfsConfig, VfsError, VfsStart};

/// Offset in seconds between the Windows FILETIME epoch (1601-01-01)
/// and the Unix epoch (1970-01-01).
const UNIX_TO_FILETIME_EPOCH_OFFSET: u64 = 11_644_473_600;

/// FILETIME ticks per second (100-nanosecond intervals).
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;

/// Convert a Unix timestamp (seconds since epoch) to a Windows FILETIME value.
fn unix_to_filetime(unix_seconds: u64) -> u64 {
    (unix_seconds + UNIX_TO_FILETIME_EPOCH_OFFSET) * FILETIME_TICKS_PER_SECOND
}

fn dummy_context() -> roam::Context {
    roam::Context::new(
        roam::wire::ConnectionId(0),
        roam::wire::RequestId(0),
        roam::wire::MethodId(0),
        Default::default(),
        vec![],
    )
}

struct ProjFsBridge<T> {
    vfs: T,
    handle: tokio::runtime::Handle,
}

impl<T> ProjFsBridge<T> {
    fn new(vfs: T, handle: tokio::runtime::Handle) -> Self {
        Self { vfs, handle }
    }
}

impl<T: Vfs + Send + Sync + 'static> ProjFsBridge<T> {
    /// Resolve a relative path (from ProjFS) to a VFS ItemId by walking
    /// from the root (ItemId 1) through each path component via `lookup()`.
    fn resolve_path(&self, path: &Path) -> Option<u64> {
        let cx = dummy_context();
        let mut current_id: u64 = 1; // root

        for component in path.components() {
            let name = component.as_os_str().to_string_lossy().to_string();
            let result = self.handle.block_on(self.vfs.lookup(&cx, current_id, name.clone()));
            if result.error != 0 {
                tracing::debug!(?path, %name, current_id, error = result.error, "resolve_path: lookup failed");
                return None;
            }
            current_id = result.attrs.item_id;
        }

        Some(current_id)
    }
}

impl<T: Vfs + Send + Sync + 'static> ProjectedFileSystemSource for ProjFsBridge<T> {
    fn list_directory(&self, path: &Path) -> Vec<DirectoryEntry> {
        tracing::debug!(?path, "ProjFS list_directory");
        let Some(dir_id) = self.resolve_path(path) else {
            tracing::warn!(?path, "ProjFS list_directory: resolve_path failed");
            return vec![];
        };

        let cx = dummy_context();
        let mut entries = Vec::new();
        let mut cursor: u64 = 0;

        loop {
            let result = self.handle.block_on(self.vfs.read_dir(&cx, dir_id, cursor));
            if result.error != 0 {
                tracing::warn!(error = result.error, ?path, "read_dir failed");
                return vec![];
            }

            for entry in &result.entries {
                let ft_created = unix_to_filetime(entry.created_time);
                let ft_modified = unix_to_filetime(entry.modified_time);

                match entry.item_type {
                    ItemType::File | ItemType::Symlink => {
                        entries.push(DirectoryEntry::File(FileInfo {
                            file_name: entry.name.clone(),
                            file_size: entry.size,
                            file_attributes: 0,
                            creation_time: ft_created,
                            last_access_time: ft_modified,
                            last_write_time: ft_modified,
                        }));
                    }
                    ItemType::Directory => {
                        entries.push(DirectoryEntry::Directory(DirectoryInfo {
                            directory_name: entry.name.clone(),
                            directory_attributes: 0,
                            creation_time: ft_created,
                            last_access_time: ft_modified,
                            last_write_time: ft_modified,
                        }));
                    }
                }
            }

            if result.next_cursor == 0 {
                break;
            }
            cursor = result.next_cursor;
        }

        // ProjFS requires entries sorted by name
        entries.sort();
        entries
    }

    fn get_directory_entry(&self, path: &Path) -> Option<DirectoryEntry> {
        tracing::debug!(?path, "ProjFS get_directory_entry");
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let file_name = path.file_name()?.to_string_lossy().to_string();

        let parent_id = match self.resolve_path(parent) {
            Some(id) => id,
            None => {
                tracing::warn!(?path, ?parent, "ProjFS get_directory_entry: parent resolve failed");
                return None;
            }
        };
        let cx = dummy_context();
        let result = self
            .handle
            .block_on(self.vfs.lookup(&cx, parent_id, file_name.clone()));
        if result.error != 0 {
            return None;
        }

        let attrs = &result.attrs;
        let ft_created = unix_to_filetime(attrs.created_time);
        let ft_modified = unix_to_filetime(attrs.modified_time);

        Some(match attrs.item_type {
            ItemType::File | ItemType::Symlink => DirectoryEntry::File(FileInfo {
                file_name,
                file_size: attrs.size,
                file_attributes: 0,
                creation_time: ft_created,
                last_access_time: ft_modified,
                last_write_time: ft_modified,
            }),
            ItemType::Directory => DirectoryEntry::Directory(DirectoryInfo {
                directory_name: file_name,
                directory_attributes: 0,
                creation_time: ft_created,
                last_access_time: ft_modified,
                last_write_time: ft_modified,
            }),
        })
    }

    fn stream_file_content(
        &self,
        path: &Path,
        byte_offset: usize,
        length: usize,
    ) -> io::Result<Box<dyn Read>> {
        tracing::debug!(?path, byte_offset, length, "ProjFS stream_file_content");
        let Some(file_id) = self.resolve_path(path) else {
            tracing::error!(?path, "stream_file_content: path resolution failed");
            // Return zero-filled buffer — constructing a proper windows_projfs::Error
            // requires the `windows` crate as a direct dependency. CAS read failures
            // should be extremely rare.
            return Ok(Box::new(Cursor::new(vec![0u8; length])));
        };

        let cx = dummy_context();
        let result = self
            .handle
            .block_on(self.vfs.read(&cx, file_id, byte_offset as u64, length as u64));
        if result.error != 0 {
            tracing::error!(error = result.error, ?path, "stream_file_content: read failed");
            return Ok(Box::new(Cursor::new(vec![0u8; length])));
        }

        Ok(Box::new(Cursor::new(result.data)))
    }

    fn handle_notification(&self, notification: &Notification) -> ControlFlow<()> {
        tracing::debug!(?notification, "ProjFS notification");
        ControlFlow::Continue(())
    }
}

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + Clone + Send + Sync + 'static,
{
    // Clean up stale ProjFS reparse points from a previous (unclean) shutdown.
    cleanup_stale_projfs(&config.mount_path);

    // Ensure the mount directory exists
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

    let handle = tokio::runtime::Handle::current();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<VfsStart, VfsError>>();

    let mount_path = config.mount_path.clone();
    std::thread::Builder::new()
        .name("projfs-runtime".into())
        .spawn(move || {
            let bridge = ProjFsBridge::new(vfs, handle);

            let _fs = match ProjectedFileSystem::new(mount_path.as_std_path(), bridge) {
                Ok(fs) => fs,
                Err(e) => {
                    let message = if matches!(e, windows_projfs::Error::WindowsFeatureNotEnabled) {
                        format!(
                            "ProjFS mount failed: {e}\n\n\
                            Enable it from an elevated PowerShell:\n\n  \
                            Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart\n"
                        )
                    } else {
                        format!("ProjFS mount failed: {e}")
                    };
                    let _ = result_tx.send(Err(VfsError::CommandFailed {
                        program: "projfs",
                        message,
                        _diagnostics: vec![],
                    }));
                    return;
                }
            };

            let (vfs_dead_tx, vfs_dead_rx) = tokio::sync::watch::channel(false);

            let _ = result_tx.send(Ok(VfsStart::Mounted {
                mount_path,
                vfs_dead: vfs_dead_rx,
            }));

            // Park this thread forever — the ProjFS instance stays alive
            // as long as `_fs` is not dropped. When the process exits,
            // Drop will stop the virtualization.
            std::thread::park();

            // If unparked, signal that VFS is dead
            let _ = vfs_dead_tx.send(true);
        })
        .expect("failed to spawn ProjFS runtime thread");

    result_rx.recv().unwrap_or_else(|_| {
        Err(VfsError::CommandFailed {
            program: "projfs",
            message: "ProjFS runtime thread died before sending result".to_string(),
            _diagnostics: vec![],
        })
    })
}

/// Remove a stale ProjFS virtualization root from a previous unclean shutdown.
///
/// ProjFS reparse points can't be detected via `GetFileAttributesW` and can't be
/// stripped with `fsutil reparsepoint delete` when the provider is dead (error 369).
/// Rust's `remove_dir_all` also fails because the ProjFS filter driver intercepts
/// filesystem operations. However, `cmd /c rmdir /s /q` works — it uses lower-level
/// removal that bypasses the filter. Same approach VFSForGit uses.
fn cleanup_stale_projfs(mount_path: &camino::Utf8Path) {
    if !mount_path.as_std_path().exists() {
        return;
    }

    tracing::info!(%mount_path, "removing previous ProjFS mount directory");
    let status = std::process::Command::new("cmd")
        .args(["/c", "rmdir", "/s", "/q", mount_path.as_str()])
        .status();
    match status {
        Ok(s) if s.success() => {
            tracing::info!(%mount_path, "removed stale ProjFS mount directory");
        }
        Ok(s) => {
            tracing::warn!(%mount_path, code = ?s.code(), "rmdir failed for ProjFS mount directory");
        }
        Err(e) => {
            tracing::warn!(%mount_path, %e, "failed to run rmdir");
        }
    }
}
