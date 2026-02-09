use std::ffi::OsStr;

use bytes::Bytes;
use futures_util::stream::{Empty, Stream};

use crate::notify::Notify;
use crate::raw::reply::*;
use crate::raw::request::Request;
use crate::{Inode, Result, SetAttr};

#[allow(unused_variables)]
#[trait_make::make(Send)]
/// Inode based filesystem trait.
pub trait Filesystem {
    async fn init(&self, req: Request) -> Result<ReplyInit>;
    async fn destroy(&self, req: Request);

    async fn lookup(&self, req: Request, parent: Inode, name: &OsStr) -> Result<ReplyEntry> {
        Err(libc::ENOSYS.into())
    }

    async fn forget(&self, req: Request, inode: Inode, nlookup: u64) {}

    async fn getattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        flags: u32,
    ) -> Result<ReplyAttr> {
        Err(libc::ENOSYS.into())
    }

    async fn setattr(
        &self,
        req: Request,
        inode: Inode,
        fh: Option<u64>,
        set_attr: SetAttr,
    ) -> Result<ReplyAttr> {
        Err(libc::ENOSYS.into())
    }

    async fn readlink(&self, req: Request, inode: Inode) -> Result<ReplyData> {
        Err(libc::ENOSYS.into())
    }

    async fn symlink(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        link: &OsStr,
    ) -> Result<ReplyEntry> {
        Err(libc::ENOSYS.into())
    }

    async fn mknod(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        rdev: u32,
    ) -> Result<ReplyEntry> {
        Err(libc::ENOSYS.into())
    }

    async fn mkdir(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        umask: u32,
    ) -> Result<ReplyEntry> {
        Err(libc::ENOSYS.into())
    }

    async fn unlink(&self, req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn rmdir(&self, req: Request, parent: Inode, name: &OsStr) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn rename(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn link(
        &self,
        req: Request,
        inode: Inode,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<ReplyEntry> {
        Err(libc::ENOSYS.into())
    }

    async fn open(&self, req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        Err(libc::ENOSYS.into())
    }

    async fn read(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        size: u32,
    ) -> Result<ReplyData> {
        Err(libc::ENOSYS.into())
    }

    #[allow(clippy::too_many_arguments)]
    async fn write(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        data: &[u8],
        write_flags: u32,
        flags: u32,
    ) -> Result<ReplyWrite> {
        Err(libc::ENOSYS.into())
    }

    async fn statfs(&self, req: Request, inode: Inode) -> Result<ReplyStatFs> {
        Err(libc::ENOSYS.into())
    }

    async fn release(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        flags: u32,
        lock_owner: u64,
        flush: bool,
    ) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn fsync(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        Ok(())
    }

    async fn setxattr(
        &self,
        req: Request,
        inode: Inode,
        name: &OsStr,
        value: &[u8],
        flags: u32,
        position: u32,
    ) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn getxattr(
        &self,
        req: Request,
        inode: Inode,
        name: &OsStr,
        size: u32,
    ) -> Result<ReplyXAttr> {
        Err(libc::ENOSYS.into())
    }

    async fn listxattr(&self, req: Request, inode: Inode, size: u32) -> Result<ReplyXAttr> {
        Err(libc::ENOSYS.into())
    }

    async fn removexattr(&self, req: Request, inode: Inode, name: &OsStr) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn flush(&self, req: Request, inode: Inode, fh: u64, lock_owner: u64) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn opendir(&self, req: Request, inode: Inode, flags: u32) -> Result<ReplyOpen> {
        Err(libc::ENOSYS.into())
    }

    async fn readdir<'a>(
        &'a self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: i64,
    ) -> Result<ReplyDirectory<impl Stream<Item = Result<DirectoryEntry>> + Send + 'a>> {
        Err::<ReplyDirectory<Empty<_>>, _>(libc::ENOSYS.into())
    }

    async fn releasedir(&self, req: Request, inode: Inode, fh: u64, flags: u32) -> Result<()> {
        Ok(())
    }

    async fn fsyncdir(&self, req: Request, inode: Inode, fh: u64, datasync: bool) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn access(&self, req: Request, inode: Inode, mask: u32) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn create(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        mode: u32,
        flags: u32,
    ) -> Result<ReplyCreated> {
        Err(libc::ENOSYS.into())
    }

    async fn interrupt(&self, req: Request, unique: u64) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn bmap(
        &self,
        req: Request,
        inode: Inode,
        blocksize: u32,
        idx: u64,
    ) -> Result<ReplyBmap> {
        Err(libc::ENOSYS.into())
    }

    #[allow(clippy::too_many_arguments)]
    async fn poll(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        kh: Option<u64>,
        flags: u32,
        events: u32,
        notify: &Notify,
    ) -> Result<ReplyPoll> {
        Err(libc::ENOSYS.into())
    }

    async fn notify_reply(
        &self,
        req: Request,
        inode: Inode,
        offset: u64,
        data: Bytes,
    ) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn batch_forget(&self, req: Request, inodes: &[Inode]) {}

    async fn fallocate(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        length: u64,
        mode: u32,
    ) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn readdirplus<'a>(
        &'a self,
        req: Request,
        parent: Inode,
        fh: u64,
        offset: u64,
        lock_owner: u64,
    ) -> Result<ReplyDirectoryPlus<impl Stream<Item = Result<DirectoryEntryPlus>> + Send + 'a>>
    {
        Err::<ReplyDirectoryPlus<Empty<_>>, _>(libc::ENOSYS.into())
    }

    async fn rename2(
        &self,
        req: Request,
        parent: Inode,
        name: &OsStr,
        new_parent: Inode,
        new_name: &OsStr,
        flags: u32,
    ) -> Result<()> {
        Err(libc::ENOSYS.into())
    }

    async fn lseek(
        &self,
        req: Request,
        inode: Inode,
        fh: u64,
        offset: u64,
        whence: u32,
    ) -> Result<ReplyLSeek> {
        Err(libc::ENOSYS.into())
    }

    #[allow(clippy::too_many_arguments)]
    async fn copy_file_range(
        &self,
        req: Request,
        inode: Inode,
        fh_in: u64,
        off_in: u64,
        inode_out: Inode,
        fh_out: u64,
        off_out: u64,
        length: u64,
        flags: u64,
    ) -> Result<ReplyCopyFileRange> {
        Err(libc::ENOSYS.into())
    }
}
