//! Linux VFS backend using FUSE (via fuse3).
//!
//! Maps the `Vfs` trait to the fuse3 `Filesystem` trait, providing a read-only
//! FUSE mount backed by any `Vfs` implementation.

use std::ffi::OsStr;
use std::num::NonZeroU32;
use std::time::Duration;

use bytes::Bytes;
use fuse3::raw::prelude::*;
use fuse3::{Errno, Inode, MountOptions, Timestamp};
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

    async fn lookup(&self, _req: Request, parent: Inode, name: &OsStr) -> fuse3::Result<ReplyEntry> {
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

    async fn opendir(
        &self,
        _req: Request,
        _inode: Inode,
        _flags: u32,
    ) -> fuse3::Result<ReplyOpen> {
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
        .read_only(true)
        .allow_other(false)
        .force_readdir_plus(true);

    let bridge = FuseBridge::new(vfs, uid, gid);
    let mount_path = config.mount_path.clone();

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
    });

    Ok(VfsStart::Mounted {
        mount_path: config.mount_path,
    })
}
