//! inode based FUSE filesystem interface.

use bytes::Bytes;
pub use filesystem::Filesystem;
use futures_util::future::Either;
pub use request::Request;
pub use session::{MountHandle, Session};

pub(crate) type FuseData = Either<Vec<u8>, (Vec<u8>, Bytes)>;

pub(crate) mod abi;
pub(crate) mod connection;
mod filesystem;
pub mod flags;
pub mod reply;
mod request;
pub(crate) mod session;

pub mod prelude {
    pub use super::Filesystem;
    pub use super::Request;
    pub use super::Session;
    pub use super::reply::FileAttr;
    pub use super::reply::*;
    pub use crate::FileType;
    pub use crate::SetAttr;
    pub use crate::notify::Notify;
}
