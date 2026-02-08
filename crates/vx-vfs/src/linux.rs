//! Linux VFS backend using FUSE (via fuse3).
//!
//! Maps the `Vfs` trait to the fuse3 `Filesystem` trait, providing a
//! FUSE mount backed by any `Vfs` implementation.

use std::ffi::OsStr;
use std::num::NonZeroU32;
use std::time::Duration;

use bytes::Bytes;
use fuse3::raw::prelude::*;
use fuse3::{Errno, Inode, MountOptions, SetAttr, Timestamp};
use futures_util::stream;
use vixenfs_proto::{ItemAttributes, ItemType, Vfs};

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

fn dummy_context() -> roam::Context {
    roam::Context::new(
        roam::wire::ConnectionId(0),
        roam::wire::RequestId(0),
        roam::wire::MethodId(0),
        Default::default(),
        vec![],
    )
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
    type DirEntryStream<'a> = stream::Iter<std::vec::IntoIter<fuse3::Result<DirectoryEntry>>>;
    type DirEntryPlusStream<'a> =
        stream::Iter<std::vec::IntoIter<fuse3::Result<DirectoryEntryPlus>>>;

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
        let cx = dummy_context();
        let result = self.vfs.lookup(&cx, parent, name_str.to_owned()).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        let attr_result = self.vfs.get_attributes(&cx, result.item_id).await;
        if attr_result.error != 0 {
            return Err(errno_from_vfs(attr_result.error));
        }

        Ok(ReplyEntry {
            ttl: TTL,
            attr: attrs_to_file_attr(&attr_result.attrs, self.uid, self.gid),
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
        let cx = dummy_context();
        let result = self.vfs.get_attributes(&cx, inode).await;
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
        let cx = dummy_context();
        let result = self.vfs.read(&cx, inode, offset, size as u64).await;
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
        let cx = dummy_context();
        let len = data.len();
        let result = self.vfs.write(&cx, inode, offset, data.to_vec()).await;
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
        let cx = dummy_context();
        let params = vixenfs_proto::SetAttributesParams {
            mode: set_attr.mode,
            modified_time: None,
            size: set_attr.size,
        };
        let result = self.vfs.set_attributes(&cx, inode, params).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        // Return updated attributes
        let attr_result = self.vfs.get_attributes(&cx, inode).await;
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
        let cx = dummy_context();
        let result = self
            .vfs
            .create(&cx, parent, name_str.to_owned(), ItemType::Directory)
            .await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        let attr_result = self.vfs.get_attributes(&cx, result.item_id).await;
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
        let cx = dummy_context();
        let result = self
            .vfs
            .create(&cx, parent, name_str.to_owned(), ItemType::File)
            .await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }

        let attr_result = self.vfs.get_attributes(&cx, result.item_id).await;
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
        let cx = dummy_context();
        let lookup = self.vfs.lookup(&cx, parent, name_str.to_owned()).await;
        if lookup.error != 0 {
            return Err(errno_from_vfs(lookup.error));
        }
        let result = self.vfs.delete(&cx, lookup.item_id).await;
        if result.error != 0 {
            return Err(errno_from_vfs(result.error));
        }
        Ok(())
    }

    async fn rmdir(&self, _req: Request, parent: Inode, name: &OsStr) -> fuse3::Result<()> {
        let name_str = name.to_str().ok_or_else(Errno::new_not_exist)?;
        let cx = dummy_context();
        let lookup = self.vfs.lookup(&cx, parent, name_str.to_owned()).await;
        if lookup.error != 0 {
            return Err(errno_from_vfs(lookup.error));
        }
        let result = self.vfs.delete(&cx, lookup.item_id).await;
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
        let cx = dummy_context();
        let lookup = self.vfs.lookup(&cx, parent, name_str.to_owned()).await;
        if lookup.error != 0 {
            return Err(errno_from_vfs(lookup.error));
        }
        let result = self
            .vfs
            .rename(&cx, lookup.item_id, new_parent, new_name_str.to_owned())
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
    ) -> fuse3::Result<ReplyDirectory<Self::DirEntryStream<'_>>> {
        let cx = dummy_context();
        let mut entries: Vec<fuse3::Result<DirectoryEntry>> = Vec::new();
        let mut cursor: u64 = 0;
        let mut entry_offset: i64 = 1;

        loop {
            let result = self.vfs.read_dir(&cx, parent, cursor).await;
            if result.error != 0 {
                return Err(errno_from_vfs(result.error));
            }
            for entry in result.entries {
                entries.push(Ok(DirectoryEntry {
                    inode: entry.item_id,
                    kind: item_type_to_file_type(entry.item_type),
                    name: entry.name.into(),
                    offset: entry_offset,
                }));
                entry_offset += 1;
            }
            if result.next_cursor == 0 {
                break;
            }
            cursor = result.next_cursor;
        }

        // Skip entries before offset (FUSE provides the offset of the last entry it received)
        let entries: Vec<_> = entries.into_iter().skip(offset as usize).collect();
        Ok(ReplyDirectory {
            entries: stream::iter(entries),
        })
    }

    async fn readdirplus(
        &self,
        _req: Request,
        parent: Inode,
        _fh: u64,
        offset: u64,
        _lock_owner: u64,
    ) -> fuse3::Result<ReplyDirectoryPlus<Self::DirEntryPlusStream<'_>>> {
        let cx = dummy_context();
        let mut entries: Vec<fuse3::Result<DirectoryEntryPlus>> = Vec::new();
        let mut cursor: u64 = 0;
        let mut entry_offset: i64 = 1;

        loop {
            let result = self.vfs.read_dir(&cx, parent, cursor).await;
            if result.error != 0 {
                return Err(errno_from_vfs(result.error));
            }
            for entry in result.entries {
                let attr_result = self.vfs.get_attributes(&cx, entry.item_id).await;
                let attr = if attr_result.error == 0 {
                    attrs_to_file_attr(&attr_result.attrs, self.uid, self.gid)
                } else {
                    // Fallback: synthesize minimal attributes from the dir entry
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

                entries.push(Ok(DirectoryEntryPlus {
                    inode: entry.item_id,
                    generation: 0,
                    kind: item_type_to_file_type(entry.item_type),
                    name: entry.name.into(),
                    offset: entry_offset,
                    attr,
                    entry_ttl: TTL,
                    attr_ttl: TTL,
                }));
                entry_offset += 1;
            }
            if result.next_cursor == 0 {
                break;
            }
            cursor = result.next_cursor;
        }

        let entries: Vec<_> = entries.into_iter().skip(offset as usize).collect();
        Ok(ReplyDirectoryPlus {
            entries: stream::iter(entries),
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
    T: Vfs + Clone + Send + Sync + 'static,
{
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

    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let mut mount_options = MountOptions::default();
    mount_options
        .fs_name("vixenfs")
        .allow_other(false)
        .force_readdir_plus(true);

    let bridge = FuseBridge::new(vfs, uid, gid);
    let mount_path = config.mount_path.clone();

    let (vfs_dead_tx, vfs_dead_rx) = tokio::sync::watch::channel(false);

    let mount_handle = Session::new(mount_options)
        .mount_with_unprivileged(bridge, mount_path.as_std_path())
        .await
        .map_err(|e| VfsError::CommandFailed {
            program: "fuse3",
            message: format!("FUSE mount failed: {e}"),
            _diagnostics: vec![],
        })?;

    tokio::spawn(async move {
        if let Err(e) = mount_handle.await {
            tracing::error!(%e, "FUSE mount ended with error");
        }
        let _ = vfs_dead_tx.send(true);
    });

    // Fork a sentinel child process that will unmount the FUSE filesystem
    // when the parent process dies (including SIGKILL). The child uses
    // PR_SET_PDEATHSIG to receive SIGTERM when the parent exits, then calls
    // fusermount3 -u to clean up the mount.
    {
        let mount_path_cstr = std::ffi::CString::new(config.mount_path.as_str()).unwrap();
        // SAFETY: fork() is safe here because we're calling it before spawning
        // many threads (the tokio runtime runs on the current thread until we
        // return). The child process immediately sets up a signal handler and
        // pauses — it does not use any async runtime or locks.
        unsafe {
            let pid = libc::fork();
            match pid {
                -1 => {
                    tracing::warn!("failed to fork FUSE unmount sentinel");
                }
                0 => {
                    // Child: set up death signal, then wait for it
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);

                    // Check if parent already died between fork and prctl
                    if libc::getppid() == 1 {
                        let _ = libc::execlp(
                            c"fusermount3".as_ptr(),
                            c"fusermount3".as_ptr(),
                            c"-u".as_ptr(),
                            mount_path_cstr.as_ptr(),
                            std::ptr::null::<libc::c_char>(),
                        );
                        libc::_exit(1);
                    }

                    // Wait for SIGTERM (from parent death)
                    let mut sigset: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut sigset);
                    libc::sigaddset(&mut sigset, libc::SIGTERM);
                    libc::sigprocmask(libc::SIG_BLOCK, &sigset, std::ptr::null_mut());
                    let mut sig: libc::c_int = 0;
                    libc::sigwait(&sigset, &mut sig);

                    // Parent died — unmount via fusermount3
                    let _ = libc::execlp(
                        c"fusermount3".as_ptr(),
                        c"fusermount3".as_ptr(),
                        c"-u".as_ptr(),
                        mount_path_cstr.as_ptr(),
                        std::ptr::null::<libc::c_char>(),
                    );
                    libc::_exit(1);
                }
                _ => {
                    tracing::debug!(mount_path = %config.mount_path, sentinel_pid = pid, "forked FUSE unmount sentinel");
                }
            }
        }
    }

    Ok(VfsStart::Mounted {
        mount_path: config.mount_path,
        vfs_dead: vfs_dead_rx,
    })
}
