//! reply structures.
use std::ffi::OsString;
use std::num::NonZeroU32;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream::Stream;

use crate::helper::mode_from_kind_and_perm;
use crate::raw::abi::{
    fuse_attr, fuse_attr_out, fuse_bmap_out, fuse_entry_out, fuse_kstatfs, fuse_lseek_out,
    fuse_open_out, fuse_poll_out, fuse_statfs_out, fuse_write_out,
};
use crate::{FileType, Result, Timestamp};

/// file attributes
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct FileAttr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: Timestamp,
    pub mtime: Timestamp,
    pub ctime: Timestamp,
    pub kind: FileType,
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
}

impl From<FileAttr> for fuse_attr {
    fn from(attr: FileAttr) -> Self {
        fuse_attr {
            ino: attr.ino,
            size: attr.size,
            blocks: attr.blocks,
            atime: attr.atime.sec as u64,
            mtime: attr.mtime.sec as u64,
            ctime: attr.ctime.sec as u64,
            atimensec: attr.atime.nsec,
            mtimensec: attr.mtime.nsec,
            ctimensec: attr.ctime.nsec,
            mode: mode_from_kind_and_perm(attr.kind, attr.perm),
            nlink: attr.nlink,
            uid: attr.uid,
            gid: attr.gid,
            rdev: attr.rdev,
            blksize: attr.blksize,
            _padding: 0,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyInit {
    pub max_write: NonZeroU32,
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyEntry {
    pub ttl: Duration,
    pub attr: FileAttr,
    pub generation: u64,
}

impl From<ReplyEntry> for fuse_entry_out {
    fn from(entry: ReplyEntry) -> Self {
        let attr = entry.attr;
        fuse_entry_out {
            nodeid: attr.ino,
            generation: entry.generation,
            entry_valid: entry.ttl.as_secs(),
            attr_valid: entry.ttl.as_secs(),
            entry_valid_nsec: entry.ttl.subsec_nanos(),
            attr_valid_nsec: entry.ttl.subsec_nanos(),
            attr: attr.into(),
        }
    }
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyAttr {
    pub ttl: Duration,
    pub attr: FileAttr,
}

impl From<ReplyAttr> for fuse_attr_out {
    fn from(attr: ReplyAttr) -> Self {
        fuse_attr_out {
            attr_valid: attr.ttl.as_secs(),
            attr_valid_nsec: attr.ttl.subsec_nanos(),
            dummy: 0,
            attr: attr.attr.into(),
        }
    }
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyData {
    pub data: Bytes,
}

impl From<Bytes> for ReplyData {
    fn from(data: Bytes) -> Self {
        Self { data }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyOpen {
    pub fh: u64,
    pub flags: u32,
}

impl From<ReplyOpen> for fuse_open_out {
    fn from(opened: ReplyOpen) -> Self {
        fuse_open_out {
            fh: opened.fh,
            open_flags: opened.flags,
            _padding: 0,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyWrite {
    pub written: u32,
}

impl From<ReplyWrite> for fuse_write_out {
    fn from(written: ReplyWrite) -> Self {
        fuse_write_out {
            size: written.written,
            _padding: 0,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyStatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

impl From<ReplyStatFs> for fuse_statfs_out {
    fn from(stat_fs: ReplyStatFs) -> Self {
        fuse_statfs_out {
            st: fuse_kstatfs {
                blocks: stat_fs.blocks,
                bfree: stat_fs.bfree,
                bavail: stat_fs.bavail,
                files: stat_fs.files,
                ffree: stat_fs.ffree,
                bsize: stat_fs.bsize,
                namelen: stat_fs.namelen,
                frsize: stat_fs.frsize,
                _padding: 0,
                spare: [0; 6],
            },
        }
    }
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum ReplyXAttr {
    Size(u32),
    Data(Bytes),
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct DirectoryEntry {
    pub inode: u64,
    pub kind: FileType,
    pub name: OsString,
    pub offset: i64,
}

pub struct ReplyDirectory<S: Stream<Item = Result<DirectoryEntry>>> {
    pub entries: S,
}

impl<S: Stream<Item = Result<DirectoryEntry>> + std::fmt::Debug> std::fmt::Debug
    for ReplyDirectory<S>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyDirectory")
            .field("entries", &self.entries)
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyCreated {
    pub ttl: Duration,
    pub attr: FileAttr,
    pub generation: u64,
    pub fh: u64,
    pub flags: u32,
}

impl From<ReplyCreated> for (fuse_entry_out, fuse_open_out) {
    fn from(created: ReplyCreated) -> Self {
        let attr = created.attr;
        let entry_out = fuse_entry_out {
            nodeid: attr.ino,
            generation: created.generation,
            entry_valid: created.ttl.as_secs(),
            attr_valid: created.ttl.as_secs(),
            entry_valid_nsec: created.ttl.subsec_micros(),
            attr_valid_nsec: created.ttl.subsec_micros(),
            attr: attr.into(),
        };
        let open_out = fuse_open_out {
            fh: created.fh,
            open_flags: created.flags,
            _padding: 0,
        };
        (entry_out, open_out)
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyBmap {
    pub block: u64,
}

impl From<ReplyBmap> for fuse_bmap_out {
    fn from(bmap: ReplyBmap) -> Self {
        fuse_bmap_out { block: bmap.block }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyPoll {
    pub revents: u32,
}

impl From<ReplyPoll> for fuse_poll_out {
    fn from(poll: ReplyPoll) -> Self {
        fuse_poll_out {
            revents: poll.revents,
            _padding: 0,
        }
    }
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct DirectoryEntryPlus {
    pub inode: u64,
    pub generation: u64,
    pub kind: FileType,
    pub name: OsString,
    pub offset: i64,
    pub attr: FileAttr,
    pub entry_ttl: Duration,
    pub attr_ttl: Duration,
}

pub struct ReplyDirectoryPlus<S: Stream<Item = Result<DirectoryEntryPlus>>> {
    pub entries: S,
}

impl<S: Stream<Item = Result<DirectoryEntryPlus>> + std::fmt::Debug> std::fmt::Debug
    for ReplyDirectoryPlus<S>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyDirectoryPlus")
            .field("entries", &self.entries)
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyLSeek {
    pub offset: u64,
}

impl From<ReplyLSeek> for fuse_lseek_out {
    fn from(seek: ReplyLSeek) -> Self {
        fuse_lseek_out {
            offset: seek.offset,
        }
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ReplyCopyFileRange {
    pub copied: u64,
}

impl From<ReplyCopyFileRange> for fuse_write_out {
    fn from(copied: ReplyCopyFileRange) -> Self {
        fuse_write_out {
            size: copied.copied as u32,
            _padding: 0,
        }
    }
}
