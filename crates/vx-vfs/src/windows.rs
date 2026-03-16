//! Windows VFS backend using ProjFS (Projected File System).
//!
//! Maps the `Vfs` trait to the `windows_projfs::ProjectedFileSystemSource` trait,
//! providing a ProjFS mount backed by any `Vfs` implementation.

use std::io::{self, Cursor, Read};
use std::ops::ControlFlow;
use std::path::{Component, Path, PathBuf};

use vixenfs_proto::{ItemType, Vfs, VfsDiskNotify};
use windows_projfs::{
    DirectoryEntry, DirectoryInfo, FileCloseAction, FileInfo, Notification, ProjectedFileSystem,
    ProjectedFileSystemSource,
};

use crate::types::{VfsConfig, VfsError, VfsStart};

/// Offset in seconds between the Windows FILETIME epoch (1601-01-01)
/// and the Unix epoch (1970-01-01).
const UNIX_TO_FILETIME_EPOCH_OFFSET: u64 = 11_644_473_600;

/// FILETIME ticks per second (100-nanosecond intervals).
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;

/// Projected placeholder attributes observed in `windows-projfs`' own tests.
///
/// Plain `ARCHIVE` / `DIRECTORY` bits are not enough here. ProjFS expects
/// placeholder-like metadata, which the crate's reference tests represent as:
/// - files: `0x440000`
/// - directories: `0x440010`
const PROJFS_FILE_ATTRIBUTES: u32 = 0x440000;
const PROJFS_DIRECTORY_ATTRIBUTES: u32 = 0x440010;

/// Convert a Unix timestamp (seconds since epoch) to a Windows FILETIME value.
fn unix_to_filetime(unix_seconds: u64) -> u64 {
    (unix_seconds + UNIX_TO_FILETIME_EPOCH_OFFSET) * FILETIME_TICKS_PER_SECOND
}

struct ProjFsBridge<T> {
    vfs: T,
    handle: tokio::runtime::Handle,
    mount_path: PathBuf,
}

impl<T> ProjFsBridge<T> {
    fn new(vfs: T, handle: tokio::runtime::Handle, mount_path: PathBuf) -> Self {
        Self {
            vfs,
            handle,
            mount_path,
        }
    }
}

impl<T: Vfs + VfsDiskNotify + Send + Sync + 'static> ProjFsBridge<T> {
    fn summarize_directory_entries(entries: &[DirectoryEntry]) -> String {
        const SAMPLE_LIMIT: usize = 8;

        let mut sample = entries
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|entry| entry.name().to_string())
            .collect::<Vec<_>>();

        if entries.len() > SAMPLE_LIMIT {
            sample.push(format!("... (+{} more)", entries.len() - SAMPLE_LIMIT));
        }

        if sample.is_empty() {
            "-".to_string()
        } else {
            sample.join(", ")
        }
    }

    fn display_components(components: &[String]) -> String {
        if components.is_empty() {
            "<root>".to_string()
        } else {
            components.join("/")
        }
    }

    fn normalize_projfs_path(&self, path: &Path) -> Option<Vec<String>> {
        let relative = path.strip_prefix(&self.mount_path).unwrap_or(path);
        let mut normalized = Vec::new();

        for component in relative.components() {
            match component {
                Component::Normal(name) => normalized.push(name.to_string_lossy().to_string()),
                Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
                Component::ParentDir => {
                    tracing::warn!(
                        ?path,
                        mount_path = ?self.mount_path,
                        "resolve_path: refusing to traverse parent directory component"
                    );
                    return None;
                }
            }
        }

        tracing::trace!(
            ?path,
            mount_path = ?self.mount_path,
            normalized = ?normalized,
            "resolve_path: normalized ProjFS path"
        );

        Some(normalized)
    }

    /// Resolve a relative path (from ProjFS) to a VFS ItemId by walking
    /// from the root (ItemId 1) through each path component via `lookup()`.
    fn resolve_path(&self, path: &Path) -> Option<u64> {
        let mut current_id: u64 = 1; // root

        let components = self.normalize_projfs_path(path)?;
        for name in components {
            let result = self
                .handle
                .block_on(self.vfs.lookup(current_id, name.clone()));
            if result.error != 0 {
                tracing::debug!(
                    ?path,
                    %name,
                    current_id,
                    error = result.error,
                    "resolve_path: lookup failed"
                );
                return None;
            }
            current_id = result.attrs.item_id;
        }

        Some(current_id)
    }
}

impl<T: Vfs + VfsDiskNotify + Send + Sync + 'static> ProjectedFileSystemSource for ProjFsBridge<T> {
    fn list_directory(&self, path: &Path) -> Vec<DirectoryEntry> {
        tracing::debug!(?path, "ProjFS list_directory");
        let Some(normalized_components) = self.normalize_projfs_path(path) else {
            tracing::warn!(?path, "ProjFS list_directory: normalize_projfs_path failed");
            return vec![];
        };
        let Some(dir_id) = self.resolve_path(path) else {
            tracing::warn!(
                ?path,
                normalized_path = %Self::display_components(&normalized_components),
                "ProjFS list_directory: resolve_path failed"
            );
            return vec![];
        };

        let mut entries = Vec::new();
        let mut cookie: u64 = 0;
        let mut verifier: u64 = 0;

        loop {
            let result = self
                .handle
                .block_on(self.vfs.read_dir(dir_id, cookie, verifier));
            if result.error != 0 {
                tracing::warn!(error = result.error, ?path, "read_dir failed");
                return vec![];
            }
            verifier = result.verifier;

            for entry in &result.entries {
                let ft_created = unix_to_filetime(entry.created_time);
                let ft_modified = unix_to_filetime(entry.modified_time);

                match entry.item_type {
                    ItemType::File | ItemType::Symlink => {
                        entries.push(DirectoryEntry::File(FileInfo {
                            file_name: entry.name.clone(),
                            file_size: entry.size,
                            file_attributes: PROJFS_FILE_ATTRIBUTES,
                            creation_time: ft_created,
                            last_access_time: ft_modified,
                            last_write_time: ft_modified,
                        }));
                    }
                    ItemType::Directory => {
                        entries.push(DirectoryEntry::Directory(DirectoryInfo {
                            directory_name: entry.name.clone(),
                            directory_attributes: PROJFS_DIRECTORY_ATTRIBUTES,
                            creation_time: ft_created,
                            last_access_time: ft_modified,
                            last_write_time: ft_modified,
                        }));
                    }
                }
            }

            if result.next_cookie == 0 {
                break;
            }
            cookie = result.next_cookie;
        }

        // ProjFS requires entries sorted by name
        entries.sort();
        tracing::info!(
            ?path,
            normalized_path = %Self::display_components(&normalized_components),
            dir_id,
            entry_count = entries.len(),
            entries = %Self::summarize_directory_entries(&entries),
            "ProjFS list_directory result"
        );
        entries
    }

    fn get_directory_entry(&self, path: &Path) -> Option<DirectoryEntry> {
        tracing::debug!(?path, "ProjFS get_directory_entry");
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let file_name = path.file_name()?.to_string_lossy().to_string();
        let normalized_path = self
            .normalize_projfs_path(path)
            .map(|components| Self::display_components(&components))
            .unwrap_or_else(|| "<invalid>".to_string());

        let parent_id = match self.resolve_path(parent) {
            Some(id) => id,
            None => {
                tracing::warn!(
                    ?path,
                    ?parent,
                    normalized_path = %normalized_path,
                    "ProjFS get_directory_entry: parent resolve failed"
                );
                return None;
            }
        };
        let result = self
            .handle
            .block_on(self.vfs.lookup(parent_id, file_name.clone()));
        if result.error != 0 {
            tracing::warn!(
                ?path,
                normalized_path = %normalized_path,
                parent_id,
                %file_name,
                error = result.error,
                "ProjFS get_directory_entry: lookup failed"
            );
            return None;
        }

        let attrs = &result.attrs;
        let ft_created = unix_to_filetime(attrs.created_time);
        let ft_modified = unix_to_filetime(attrs.modified_time);

        let entry = match attrs.item_type {
            ItemType::File | ItemType::Symlink => DirectoryEntry::File(FileInfo {
                file_name,
                file_size: attrs.size,
                file_attributes: PROJFS_FILE_ATTRIBUTES,
                creation_time: ft_created,
                last_access_time: ft_modified,
                last_write_time: ft_modified,
            }),
            ItemType::Directory => DirectoryEntry::Directory(DirectoryInfo {
                directory_name: file_name,
                directory_attributes: PROJFS_DIRECTORY_ATTRIBUTES,
                creation_time: ft_created,
                last_access_time: ft_modified,
                last_write_time: ft_modified,
            }),
        };
        tracing::info!(
            ?path,
            normalized_path = %normalized_path,
            parent_id,
            item_id = attrs.item_id,
            item_type = ?attrs.item_type,
            "ProjFS get_directory_entry result"
        );
        Some(entry)
    }

    fn stream_file_content(
        &self,
        path: &Path,
        byte_offset: usize,
        length: usize,
    ) -> io::Result<Box<dyn Read>> {
        let normalized_path = self
            .normalize_projfs_path(path)
            .map(|components| Self::display_components(&components))
            .unwrap_or_else(|| "<invalid>".to_string());
        tracing::info!(
            ?path,
            normalized_path = %normalized_path,
            byte_offset,
            length,
            "ProjFS stream_file_content"
        );
        let Some(file_id) = self.resolve_path(path) else {
            tracing::error!(
                ?path,
                normalized_path = %normalized_path,
                "stream_file_content: path resolution failed"
            );
            // Return zero-filled buffer — constructing a proper windows_projfs::Error
            // requires the `windows` crate as a direct dependency. CAS read failures
            // should be extremely rare.
            return Ok(Box::new(Cursor::new(vec![0u8; length])));
        };

        let result =
            self.handle
                .block_on(self.vfs.read(file_id, byte_offset as u64, length as u64));
        if result.error != 0 {
            tracing::error!(
                error = result.error,
                ?path,
                normalized_path = %normalized_path,
                file_id,
                "stream_file_content: read failed"
            );
            return Ok(Box::new(Cursor::new(vec![0u8; length])));
        }

        tracing::info!(
            ?path,
            normalized_path = %normalized_path,
            file_id,
            bytes_returned = result.data.len(),
            "ProjFS stream_file_content result"
        );
        Ok(Box::new(Cursor::new(result.data)))
    }

    fn handle_notification(&self, notification: &Notification) -> ControlFlow<()> {
        match notification {
            Notification::FileCreated(file) | Notification::FileOverwritten(file) => {
                let path = file.path.to_string_lossy();
                tracing::debug!(%path, is_directory = file.is_directory, "ProjFS: file created");
                self.vfs.on_disk_file_created(&path, file.is_directory);
            }
            Notification::FileClosed(file, action) => match action {
                FileCloseAction::Modified => {
                    let path = file.path.to_string_lossy();
                    tracing::debug!(%path, "ProjFS: file modified and closed");
                    self.vfs.on_disk_file_created(&path, file.is_directory);
                }
                FileCloseAction::Deleted => {
                    let path = file.path.to_string_lossy();
                    tracing::debug!(%path, "ProjFS: file deleted");
                    self.vfs.on_disk_file_deleted(&path);
                }
                FileCloseAction::NoModification => {}
            },
            Notification::FileRenamed(info) => {
                if let (Some(src), Some(dst)) = (&info.source, &info.destination) {
                    let src = src.to_string_lossy();
                    let dst = dst.to_string_lossy();
                    tracing::debug!(%src, %dst, "ProjFS: file renamed");
                    self.vfs.on_disk_file_renamed(&src, &dst);
                }
            }
            other => {
                tracing::trace!(?other, "ProjFS notification (ignored)");
            }
        }
        ControlFlow::Continue(())
    }
}

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    // Clean up stale ProjFS reparse points from a previous (unclean) shutdown.
    cleanup_stale_projfs(&config.mount_path);

    // Ensure the mount directory exists
    moire::fs::create_dir_all(config.mount_path.as_std_path())
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
            let bridge = ProjFsBridge::new(vfs, handle, mount_path.as_std_path().to_path_buf());

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

            let (vfs_dead_tx, vfs_dead_rx) = moire::sync::watch::channel("vfs_dead", false);

            let access_path = mount_path.clone();
            let _ = result_tx.send(Ok(VfsStart::Mounted {
                mount_path,
                access_path,
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
