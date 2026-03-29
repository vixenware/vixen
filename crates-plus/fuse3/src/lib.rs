#![cfg(target_os = "linux")]

//! Simplified FUSE library for Linux/tokio.
//!
//! Vendored and stripped-down from github.com/Sherlock-Holo/fuse3.
//! Only supports Linux + tokio runtime. No async-io, FreeBSD, or macOS.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use errno::Errno;
pub use helper::{mode_from_kind_and_perm, perm_from_mode_and_kind};
pub use mount_options::MountOptions;
use nix::sys::stat::mode_t;
use raw::abi::{
    FATTR_ATIME, FATTR_ATIME_NOW, FATTR_CTIME, FATTR_GID, FATTR_LOCKOWNER, FATTR_MODE, FATTR_MTIME,
    FATTR_MTIME_NOW, FATTR_SIZE, FATTR_UID, fuse_setattr_in,
};

mod errno;
mod helper;
mod mount_options;
pub mod notify;
pub mod raw;
#[cfg(feature = "telemetry")]
pub mod telemetry;

/// Filesystem Inode.
pub type Inode = u64;

/// pre-defined Result, the Err type is [`Errno`].
pub type Result<T> = std::result::Result<T, Errno>;

/// File types
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum FileType {
    NamedPipe,
    CharDevice,
    BlockDevice,
    Directory,
    RegularFile,
    Symlink,
    Socket,
}

impl FileType {
    pub const fn const_into_mode_t(self) -> mode_t {
        match self {
            FileType::NamedPipe => libc::S_IFIFO,
            FileType::CharDevice => libc::S_IFCHR,
            FileType::BlockDevice => libc::S_IFBLK,
            FileType::Directory => libc::S_IFDIR,
            FileType::RegularFile => libc::S_IFREG,
            FileType::Symlink => libc::S_IFLNK,
            FileType::Socket => libc::S_IFSOCK,
        }
    }
}

impl From<FileType> for mode_t {
    fn from(kind: FileType) -> Self {
        kind.const_into_mode_t()
    }
}

/// the setattr argument.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct SetAttr {
    pub mode: Option<mode_t>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub lock_owner: Option<u64>,
    pub atime: Option<Timestamp>,
    pub mtime: Option<Timestamp>,
    pub ctime: Option<Timestamp>,
}

macro_rules! fsai2ts {
    ( $secs: expr, $nsecs: expr) => {
        Some(Timestamp::new($secs as i64, $nsecs))
    };
}

impl From<&fuse_setattr_in> for SetAttr {
    fn from(setattr_in: &fuse_setattr_in) -> Self {
        let mut set_attr = Self::default();

        if setattr_in.valid & FATTR_MODE > 0 {
            set_attr.mode = Some(setattr_in.mode as mode_t);
        }
        if setattr_in.valid & FATTR_UID > 0 {
            set_attr.uid = Some(setattr_in.uid);
        }
        if setattr_in.valid & FATTR_GID > 0 {
            set_attr.gid = Some(setattr_in.gid);
        }
        if setattr_in.valid & FATTR_SIZE > 0 {
            set_attr.size = Some(setattr_in.size);
        }
        if setattr_in.valid & FATTR_ATIME > 0 {
            set_attr.atime = fsai2ts!(setattr_in.atime, setattr_in.atimensec);
        }
        if setattr_in.valid & FATTR_ATIME_NOW > 0 {
            set_attr.atime = Some(SystemTime::now().into());
        }
        if setattr_in.valid & FATTR_MTIME > 0 {
            set_attr.mtime = fsai2ts!(setattr_in.mtime, setattr_in.mtimensec);
        }
        if setattr_in.valid & FATTR_MTIME_NOW > 0 {
            set_attr.mtime = Some(SystemTime::now().into());
        }
        if setattr_in.valid & FATTR_LOCKOWNER > 0 {
            set_attr.lock_owner = Some(setattr_in.lock_owner);
        }
        if setattr_in.valid & FATTR_CTIME > 0 {
            set_attr.ctime = fsai2ts!(setattr_in.ctime, setattr_in.ctimensec);
        }

        set_attr
    }
}

/// A file's timestamp, according to FUSE.
#[derive(Debug, Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct Timestamp {
    pub sec: i64,
    pub nsec: u32,
}

impl Timestamp {
    pub fn new(sec: i64, nsec: u32) -> Self {
        Timestamp { sec, nsec }
    }
}

impl From<SystemTime> for Timestamp {
    fn from(t: SystemTime) -> Self {
        let d = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0));
        Timestamp {
            sec: d.as_secs().try_into().unwrap_or(i64::MAX),
            nsec: d.subsec_nanos(),
        }
    }
}

pub(crate) fn find_fusermount3() -> io::Result<PathBuf> {
    which::which("fusermount3")
        .map_err(|err| io::Error::other(format!("find fusermount3 binary failed {err:?}")))
}
