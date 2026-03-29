//! Linux VFS backend using FUSE (via fuse3).
//!
//! Maps the `Vfs` trait to the fuse3 `Filesystem` trait, providing a
//! FUSE mount backed by any `Vfs` implementation.

use std::ffi::OsStr;
use std::num::NonZeroU32;
use std::time::Duration;

use async_stream::try_stream;
use bytes::Bytes;
use fuse3::raw::prelude::*;
use fuse3::telemetry::TelemetryState;
use fuse3::{Errno, Inode, MountOptions, SetAttr, Timestamp};
use futures_util::Stream;
use vx_services::{ItemAttributes, ItemType, Vfs, VfsDiskNotify};

use crate::types::{VfsConfig, VfsError, VfsStart};

const TTL: Duration = Duration::from_secs(1);
const MAX_WRITE: u32 = 1024 * 1024;

struct FuseBridge<T> {
    vfs: T,
    uid: u32,
    gid: u32,
}

impl<T> FuseBridge<T> {
    fn new(vfs: T, uid: u32, gid: u32) -> Self {
        Self { vfs, uid, gid }
    }
}

fn item_type_to_file_type(t: ItemType) -> FileType {
    match t {
        ItemType::File => FileType::RegularFile,
        ItemType::Directory => FileType::Directory,
        ItemType::Symlink => FileType::Symlink,
    }
}

fn attrs_to_file_attr(attrs: &ItemAttributes, uid: u32, gid: u32) -> FileAttr {
    let kind = item_type_to_file_type(attrs.item_type);
    let nlink = match attrs.item_type {
        ItemType::Directory => 2,
        _ => 1,
    };
    FileAttr {
        ino: attrs.item_id,
        size: attrs.size,
        blocks: attrs.size.div_ceil(512),
        atime: Timestamp::new(attrs.modified_time as i64, 0),
        mtime: Timestamp::new(attrs.modified_time as i64, 0),
        ctime: Timestamp::new(attrs.created_time as i64, 0),
        kind,
        perm: attrs.mode as u16,
        nlink,
        uid,
        gid,
        rdev: 0,
        blksize: 4096,
    }
}

fn errno_from_vfs(code: i32) -> Errno {
    Errno::from(code)
}

impl<T: Vfs + Send + Sync + 'static> Filesystem for FuseBridge<T> {
    async fn init(&self, _req: Request) -> fuse3::Result<ReplyInit> {
        Ok(ReplyInit {
            max_write: NonZeroU32::new(MAX_WRITE).unwrap(),
        })
    }

    async fn destroy(&self, _req: Request) {}

    async fn lookup(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
    ) -> fuse3::Result<ReplyEntry> {
        let name_str = name.to_str().ok_or_else(Errno::new_not_exist)?;
        let result = self.vfs.lookup(parent, name_str.to_owned()).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        Ok(ReplyEntry {
            ttl: TTL,
            attr: attrs_to_file_attr(&result.attrs, self.uid, self.gid),
            generation: 0,
        })
    }

    async fn getattr(
        &self,
        _req: Request,
        inode: Inode,
        _fh: Option<u64>,
        _flags: u32,
    ) -> fuse3::Result<ReplyAttr> {
        let result = self.vfs.get_attributes(inode).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        Ok(ReplyAttr {
            ttl: TTL,
            attr: attrs_to_file_attr(&result.attrs, self.uid, self.gid),
        })
    }

    async fn open(&self, _req: Request, _inode: Inode, _flags: u32) -> fuse3::Result<ReplyOpen> {
        Ok(ReplyOpen { fh: 0, flags: 0 })
    }

    async fn read(
        &self,
        _req: Request,
        inode: Inode,
        _fh: u64,
        offset: u64,
        size: u32,
    ) -> fuse3::Result<ReplyData> {
        let result = self.vfs.read(inode, offset, size as u64).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        Ok(ReplyData {
            data: Bytes::from(result.data),
        })
    }

    async fn write(
        &self,
        _req: Request,
        inode: Inode,
        _fh: u64,
        offset: u64,
        data: &[u8],
        _write_flags: u32,
        _flags: u32,
    ) -> fuse3::Result<ReplyWrite> {
        let len = data.len();
        let result = self.vfs.write(inode, offset, data.to_vec()).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }
        Ok(ReplyWrite {
            written: len as u32,
        })
    }

    async fn setattr(
        &self,
        _req: Request,
        inode: Inode,
        _fh: Option<u64>,
        set_attr: SetAttr,
    ) -> fuse3::Result<ReplyAttr> {
        let params = vx_services::SetAttributesParams {
            mode: set_attr.mode,
            modified_time: None,
            size: set_attr.size,
        };
        let result = self.vfs.set_attributes(inode, params).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        // Return updated attributes
        let attr_result = self.vfs.get_attributes(inode).await;
        if attr_result.error != 0 {
            return Err(errno_from_vfs(attr_result.error));
        }
        Ok(ReplyAttr {
            ttl: TTL,
            attr: attrs_to_file_attr(&attr_result.attrs, self.uid, self.gid),
        })
    }

    async fn mkdir(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
    ) -> fuse3::Result<ReplyEntry> {
        let name_str = name.to_str().ok_or_else(Errno::new_not_exist)?;
        let result = self
            .vfs
            .create(parent, name_str.to_owned(), ItemType::Directory)
            .await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        let attr_result = self.vfs.get_attributes(result.item_id).await;
        if attr_result.error != 0 {
            return Err(errno_from_vfs(attr_result.error));
        }
        Ok(ReplyEntry {
            ttl: TTL,
            attr: attrs_to_file_attr(&attr_result.attrs, self.uid, self.gid),
            generation: 0,
        })
    }

    async fn create(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        _mode: u32,
        _flags: u32,
    ) -> fuse3::Result<ReplyCreated> {
        let name_str = name.to_str().ok_or_else(Errno::new_not_exist)?;
        let result = self
            .vfs
            .create(parent, name_str.to_owned(), ItemType::File)
            .await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        let attr_result = self.vfs.get_attributes(result.item_id).await;
        if attr_result.error != 0 {
            return Err(errno_from_vfs(attr_result.error));
        }
        let attr = attrs_to_file_attr(&attr_result.attrs, self.uid, self.gid);
        Ok(ReplyCreated {
            ttl: TTL,
            attr,
            generation: 0,
            fh: 0,
            flags: 0,
        })
    }

    async fn unlink(&self, _req: Request, parent: Inode, name: &OsStr) -> fuse3::Result<()> {
        let name_str = name.to_str().ok_or_else(Errno::new_not_exist)?;
        let lookup = self.vfs.lookup(parent, name_str.to_owned()).await;
        if lookup.error != 0 {
            return Err(errno_from_vfs(lookup.error));
        }
        let result = self.vfs.delete(lookup.attrs.item_id).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }
        Ok(())
    }

    async fn rmdir(&self, _req: Request, parent: Inode, name: &OsStr) -> fuse3::Result<()> {
        let name_str = name.to_str().ok_or_else(Errno::new_not_exist)?;
        let lookup = self.vfs.lookup(parent, name_str.to_owned()).await;
        if lookup.error != 0 {
            return Err(errno_from_vfs(lookup.error));
        }
        let result = self.vfs.delete(lookup.attrs.item_id).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }
        Ok(())
    }

    async fn rename(
        &self,
        _req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> fuse3::Result<()> {
        let name_str = name.to_str().ok_or_else(Errno::new_not_exist)?;
        let new_name_str = new_name.to_str().ok_or_else(Errno::new_not_exist)?;
        let lookup = self.vfs.lookup(parent, name_str.to_owned()).await;
        if lookup.error != 0 {
            return Err(errno_from_vfs(lookup.error));
        }
        let result = self
            .vfs
            .rename(lookup.attrs.item_id, new_parent, new_name_str.to_owned())
            .await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }
        Ok(())
    }

    async fn opendir(&self, _req: Request, _inode: Inode, _flags: u32) -> fuse3::Result<ReplyOpen> {
        Ok(ReplyOpen { fh: 0, flags: 0 })
    }

    async fn readdir(
        &self,
        _req: Request,
        parent: Inode,
        _fh: u64,
        offset: i64,
    ) -> fuse3::Result<ReplyDirectory<impl Stream<Item = fuse3::Result<DirectoryEntry>> + Send + '_>>
    {
        Ok(ReplyDirectory {
            entries: try_stream! {
                let mut cookie: u64 = 0;
                let mut verifier: u64 = 0;
                let mut entry_offset: i64 = 1;
                loop {
                    let result = self.vfs.read_dir(parent, cookie, verifier).await;
                    if result.error != 0 {
                        Err(errno_from_vfs(result.error))?;
                    }
                    verifier = result.verifier;
                    for entry in result.entries {
                        if entry_offset > offset {
                            yield DirectoryEntry {
                                inode: entry.item_id,
                                kind: item_type_to_file_type(entry.item_type),
                                name: entry.name.into(),
                                offset: entry_offset,
                            };
                        }
                        entry_offset += 1;
                    }
                    if result.next_cookie == 0 {
                        break;
                    }
                    cookie = result.next_cookie;
                }
            },
        })
    }

    async fn readdirplus(
        &self,
        _req: Request,
        parent: Inode,
        _fh: u64,
        offset: u64,
        _lock_owner: u64,
    ) -> fuse3::Result<
        ReplyDirectoryPlus<impl Stream<Item = fuse3::Result<DirectoryEntryPlus>> + Send + '_>,
    > {
        Ok(ReplyDirectoryPlus {
            entries: try_stream! {
                let mut cookie: u64 = 0;
                let mut verifier: u64 = 0;
                let mut entry_offset: i64 = 1;
                loop {
                    let result = self.vfs.read_dir(parent, cookie, verifier).await;
                    if result.error != 0 {
                        Err(errno_from_vfs(result.error))?;
                    }
                    verifier = result.verifier;
                    for entry in result.entries {
                        if entry_offset > offset as i64 {
                            let attr_result = self.vfs.get_attributes(entry.item_id).await;
                            let attr = if attr_result.error == 0 {
                                attrs_to_file_attr(&attr_result.attrs, self.uid, self.gid)
                            } else {
                                let kind = item_type_to_file_type(entry.item_type);
                                FileAttr {
                                    ino: entry.item_id,
                                    size: 0,
                                    blocks: 0,
                                    atime: Timestamp::new(0, 0),
                                    mtime: Timestamp::new(0, 0),
                                    ctime: Timestamp::new(0, 0),
                                    kind,
                                    perm: 0o444,
                                    nlink: 1,
                                    uid: self.uid,
                                    gid: self.gid,
                                    rdev: 0,
                                    blksize: 4096,
                                }
                            };
                            yield DirectoryEntryPlus {
                                inode: entry.item_id,
                                generation: 0,
                                kind: item_type_to_file_type(entry.item_type),
                                name: entry.name.into(),
                                offset: entry_offset,
                                attr,
                                entry_ttl: TTL,
                                attr_ttl: TTL,
                            };
                        }
                        entry_offset += 1;
                    }
                    if result.next_cookie == 0 {
                        break;
                    }
                    cookie = result.next_cookie;
                }
            },
        })
    }

    async fn access(&self, _req: Request, _inode: Inode, _mask: u32) -> fuse3::Result<()> {
        Ok(())
    }

    async fn statfs(&self, _req: Request, _inode: Inode) -> fuse3::Result<ReplyStatFs> {
        Ok(ReplyStatFs {
            blocks: 1024 * 1024,
            bfree: 0,
            bavail: 0,
            files: 1024 * 1024,
            ffree: 0,
            bsize: 4096,
            namelen: 255,
            frsize: 4096,
        })
    }
}

pub(super) async fn mount<T>(config: VfsConfig, vfs: T) -> Result<VfsStart, VfsError>
where
    T: Vfs + VfsDiskNotify + Clone + Send + Sync + 'static,
{
    // Skip creating mount directory for system-managed mount points
    if !config.mount_path.starts_with("/media/") {
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
    }

    // Clean up ghost mounts from previous runs that didn't unmount cleanly.
    lazy_unmount_if_mounted(&config.mount_path).await;

    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<VfsStart, VfsError>>();

    let mount_path = config.mount_path.clone();
    let telemetry = TelemetryState::new();
    let telemetry_for_fuse = telemetry.clone();
    std::thread::Builder::new()
        .name("fuse-runtime".into())
        .spawn(move || {
            // Fork the sentinel BEFORE creating the tokio runtime, so the child
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create FUSE tokio runtime");

            rt.block_on(async move {
                let uid = unsafe { libc::getuid() };
                let gid = unsafe { libc::getgid() };

                let mut mount_options = MountOptions::default();
                mount_options
                    .fs_name("vixenfs")
                    .allow_other(false)
                    .force_readdir_plus(true);

                let bridge = FuseBridge::new(vfs, uid, gid);

                let mut session = Session::new(mount_options);
                session.telemetry(telemetry_for_fuse);

                let mount_handle = match session
                    .mount_with_unprivileged(bridge, mount_path.as_std_path())
                    .await
                {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = result_tx.send(Err(VfsError::CommandFailed {
                            program: "fuse3",
                            message: format!("FUSE mount failed: {e}"),
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
                    telemetry,
                }));

                // Run the FUSE session until it ends.
                if let Err(e) = mount_handle.await {
                    tracing::error!(%e, "FUSE mount ended with error");
                }
                let _ = vfs_dead_tx.send(true);
            });
        })
        .expect("failed to spawn FUSE runtime thread");

    result_rx.recv().unwrap_or_else(|_| {
        Err(VfsError::CommandFailed {
            program: "fuse3",
            message: "FUSE runtime thread died before sending result".to_string(),
            _diagnostics: vec![],
        })
    })
}

/// If `mount_path` is already a mountpoint (ghost mount from a previous run),
/// perform a lazy unmount (`fusermount3 -uz`) so we can re-mount cleanly.
async fn lazy_unmount_if_mounted(mount_path: &camino::Utf8Path) {
    let Ok(mounts) = moire::fs::read_to_string("/proc/mounts").await else {
        return;
    };
    let is_mounted = mounts.lines().any(|line| {
        // /proc/mounts format: device mountpoint fstype options ...
        line.split_whitespace()
            .nth(1)
            .is_some_and(|mp| mp == mount_path.as_str())
    });
    if is_mounted {
        tracing::warn!(%mount_path, "found ghost mount, performing lazy unmount");
        let _ = std::process::Command::new("fusermount3")
            .args(["-uz", mount_path.as_str()])
            .status();
    }
}
