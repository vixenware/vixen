//! # vixenfs-proto
//!
//! VFS protocol for VixenFS filesystem implementations.
//!
//! ## For Backend Implementers
//!
//! To create your own VFS backend, simply implement the [`Vfs`] trait:
//!
//! ```rust,no_run
//! use vixenfs_proto::*;
//!
//! struct MyVfs;
//!
//! impl VfsServer for MyVfs {
//!     async fn lookup(&self, call: impl roam::Call<LookupResult, core::convert::Infallible>, parent_id: ItemId, name: String) {
//!         // Your implementation here
//!         call.ok(LookupResult {
//!             attrs: ItemAttributes {
//!                 item_id: parent_id,
//!                 item_type: ItemType::Directory,
//!                 size: 0,
//!                 modified_time: 0,
//!                 created_time: 0,
//!                 mode: mode::DIR_REGULAR,
//!             },
//!             error: errno::OK,
//!         }).await;
//!     }
//!     // ... implement other methods
//! #   async fn get_attributes(&self, call: impl roam::Call<GetAttributesResult, core::convert::Infallible>, _item_id: ItemId) { call.ok(GetAttributesResult { attrs: ItemAttributes { item_id: 1, item_type: ItemType::Directory, size: 0, modified_time: 0, created_time: 0, mode: mode::DIR_REGULAR }, error: errno::OK }).await; }
//! #   async fn read_dir(&self, call: impl roam::Call<ReadDirResult, core::convert::Infallible>, _item_id: ItemId, _cookie: u64, _verifier: u64) { call.ok(ReadDirResult { entries: vec![], next_cookie: 0, verifier: 0, error: errno::OK }).await; }
//! #   async fn read(&self, call: impl roam::Call<ReadResult, core::convert::Infallible>, _item_id: ItemId, _offset: u64, _len: u64) { call.ok(ReadResult { data: vec![], error: errno::OK }).await; }
//! #   async fn write(&self, call: impl roam::Call<WriteResult, core::convert::Infallible>, _item_id: ItemId, _offset: u64, _data: Vec<u8>) { call.ok(WriteResult { bytes_written: 0, error: errno::OK }).await; }
//! #   async fn create(&self, call: impl roam::Call<CreateResult, core::convert::Infallible>, _parent_id: ItemId, _name: String, _item_type: ItemType) { call.ok(CreateResult { item_id: 1, error: errno::OK }).await; }
//! #   async fn delete(&self, call: impl roam::Call<VfsResult, core::convert::Infallible>, _item_id: ItemId) { call.ok(VfsResult { error: errno::OK }).await; }
//! #   async fn rename(&self, call: impl roam::Call<VfsResult, core::convert::Infallible>, _item_id: ItemId, _new_parent_id: ItemId, _new_name: String) { call.ok(VfsResult { error: errno::OK }).await; }
//! #   async fn set_attributes(&self, call: impl roam::Call<VfsResult, core::convert::Infallible>, _item_id: ItemId, _params: SetAttributesParams) { call.ok(VfsResult { error: errno::OK }).await; }
//! #   async fn ping(&self, call: impl roam::Call<String, core::convert::Infallible>) { call.ok("pong".to_string()).await; }

//! }
//! ```
//!
//! Then serve it over TCP using `VfsDispatcher` with `roam_core::acceptor` and `roam_core::Driver`.
//!
//! ## Architecture
//!
//! This crate defines the roam service trait and types shared between:
//! - `Vixen.app` extension client code on macOS
//! - VFS backends - servers implementing the filesystem

/// Item ID type - unique identifier for files/directories.
/// ID 1 is reserved for the root directory.
pub type ItemId = u64;

/// File/directory attributes
#[derive(Debug, Clone, facet::Facet)]
pub struct ItemAttributes {
    pub item_id: ItemId,
    pub item_type: ItemType,
    pub size: u64,
    /// Unix timestamp (seconds since epoch)
    pub modified_time: u64,
    /// Unix timestamp (seconds since epoch)
    pub created_time: u64,
    /// Unix file permissions (e.g., 0o755 for executable, 0o644 for regular file)
    pub mode: u32,
}

/// Type of filesystem item
#[derive(Debug, Clone, Copy, PartialEq, Eq, facet::Facet)]
#[repr(u8)]
pub enum ItemType {
    File = 0,
    Directory = 1,
    Symlink = 2,
}

/// Result of a lookup operation
#[derive(Debug, Clone, facet::Facet)]
pub struct LookupResult {
    pub attrs: ItemAttributes,
    /// 0 = success, non-zero = errno-like error code
    pub error: i32,
}

/// Result of reading file contents
#[derive(Debug, Clone, facet::Facet)]
pub struct ReadResult {
    pub data: Vec<u8>,
    /// 0 = success, non-zero = errno-like error code
    pub error: i32,
}

/// Result of a write operation
#[derive(Debug, Clone, facet::Facet)]
pub struct WriteResult {
    pub bytes_written: u64,
    /// 0 = success, non-zero = errno-like error code
    pub error: i32,
}

/// A directory entry
#[derive(Debug, Clone, facet::Facet)]
pub struct DirEntry {
    pub name: String,
    pub item_id: ItemId,
    pub item_type: ItemType,
    pub size: u64,
    /// Unix timestamp (seconds since epoch)
    pub modified_time: u64,
    /// Unix timestamp (seconds since epoch)
    pub created_time: u64,
    /// Unix file permissions (e.g., 0o755 for executable, 0o644 for regular file)
    pub mode: u32,
}

/// Result of reading a directory
#[derive(Debug, Clone, facet::Facet)]
pub struct ReadDirResult {
    pub entries: Vec<DirEntry>,
    /// Cookie for pagination (0 = no more entries)
    pub next_cookie: u64,
    /// Directory enumeration verifier.
    /// Must be supplied unchanged with subsequent non-zero cookies.
    pub verifier: u64,
    /// 0 = success, non-zero = errno-like error code
    pub error: i32,
}

/// Result of getting attributes
#[derive(Debug, Clone, facet::Facet)]
pub struct GetAttributesResult {
    pub attrs: ItemAttributes,
    /// 0 = success, non-zero = errno-like error code
    pub error: i32,
}

/// Result of create operation
#[derive(Debug, Clone, facet::Facet)]
pub struct CreateResult {
    pub item_id: ItemId,
    /// 0 = success, non-zero = errno-like error code
    pub error: i32,
}

/// Simple success/error result
#[derive(Debug, Clone, facet::Facet)]
pub struct VfsResult {
    /// 0 = success, non-zero = errno-like error code
    pub error: i32,
}

/// Parameters for setting attributes
#[derive(Debug, Clone, facet::Facet)]
pub struct SetAttributesParams {
    /// New file mode/permissions (None = don't change)
    pub mode: Option<u32>,
    /// New modified time (None = don't change)
    pub modified_time: Option<u64>,
    /// New size in bytes (None = don't change). Used by ftruncate.
    pub size: Option<u64>,
}

/// The VFS service trait - defines the RPC interface between client and server.
#[allow(async_fn_in_trait)]
#[roam::service]
pub trait Vfs {
    /// Look up an item by name in a parent directory.
    async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult;

    /// Get attributes for an item.
    async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult;

    /// Read directory contents.
    /// Use cookie=0/verifier=0 for first page, then pass returned
    /// next_cookie/verifier for subsequent pages.
    async fn read_dir(&self, item_id: ItemId, cookie: u64, verifier: u64) -> ReadDirResult;

    /// Read file contents.
    async fn read(&self, item_id: ItemId, offset: u64, len: u64) -> ReadResult;

    /// Write file contents.
    async fn write(&self, item_id: ItemId, offset: u64, data: Vec<u8>) -> WriteResult;

    /// Create a new file or directory.
    async fn create(&self, parent_id: ItemId, name: String, item_type: ItemType) -> CreateResult;

    /// Delete an item.
    async fn delete(&self, item_id: ItemId) -> VfsResult;

    /// Rename/move an item.
    async fn rename(&self, item_id: ItemId, new_parent_id: ItemId, new_name: String) -> VfsResult;

    /// Set attributes (permissions, times, etc.) on an item.
    async fn set_attributes(&self, item_id: ItemId, params: SetAttributesParams) -> VfsResult;

    /// Ping for connectivity check.
    async fn ping(&self) -> String;
}

/// Common errno-like error codes
pub mod errno {
    /// No error
    pub const OK: i32 = 0;
    /// No such file or directory
    pub const ENOENT: i32 = 2;
    /// I/O error
    pub const EIO: i32 = 5;
    /// Permission denied
    pub const EACCES: i32 = 13;
    /// File exists
    pub const EEXIST: i32 = 17;
    /// Not a directory
    pub const ENOTDIR: i32 = 20;
    /// Is a directory
    pub const EISDIR: i32 = 21;
    /// Invalid argument
    pub const EINVAL: i32 = 22;
    /// Stale file handle / stale directory cookie
    pub const ESTALE: i32 = 116;
    /// No space left on device
    pub const ENOSPC: i32 = 28;
    /// Directory not empty
    pub const ENOTEMPTY: i32 = 66;
}

/// Common Unix file mode constants
pub mod mode {
    /// Regular file, read-write for owner, read-only for others (rw-r--r--)
    pub const FILE_REGULAR: u32 = 0o644;
    /// Executable file (rwxr-xr-x)
    pub const FILE_EXECUTABLE: u32 = 0o755;
    /// Directory (rwxr-xr-x) - directories need 'x' bit to be traversable
    pub const DIRECTORY: u32 = 0o755;
    /// Private file, owner only (rw-------)
    pub const FILE_PRIVATE: u32 = 0o600;
    /// Private executable, owner only (rwx------)
    pub const FILE_PRIVATE_EXECUTABLE: u32 = 0o700;
}
