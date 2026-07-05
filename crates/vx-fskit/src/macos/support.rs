use std::fmt::Display;
use std::future::Future;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use block2::{BlockFn, RcBlock};
use objc2::AnyThread;
use objc2::Message;
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSData, NSError, NSPOSIXErrorDomain, NSString};
use objc2_fs_kit::{
    FSDirectoryVerifier, FSFileName, FSItem, FSItemAttribute, FSItemAttributes,
    FSItemGetAttributesRequest, FSItemID, FSItemType, FSProbeResult, FSVolume,
};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::watch;
use vx_services::{DirEntry, ItemAttributes as VfsItemAttributes, ItemType as VfsItemType};

use super::log_event;
pub(super) const ROOT_LOCAL_ID: u64 = FSItemID::RootDirectory.0;
pub(super) const ROOT_BACKEND_ID: u64 = 1;
pub(super) const VIXENFS_DIR_ID: u64 = 0xFFFF_FFFF_FFFE_0001;
pub(super) const STATUS_JSON_ID: u64 = 0xFFFF_FFFF_FFFE_0002;
pub(super) const ROUTED_ITEM_ID_START: u64 = ROOT_LOCAL_ID + 1;
pub(super) const VOLUME_NAME: &str = "Vixen";
/// Stable container/volume identity for the block-backed volume. A usable FSKit
/// probe result must carry a non-nil `containerID` (`FSResource` probe docs); a
/// fixed UUID gives the volume a durable identity across remounts.
pub(super) const CONTAINER_UUID: &str = "F1A5B0C0-1234-4567-89AB-CDEF01234567";
pub(super) const VIXENFS_DIR_NAME: &str = ".vixenfs";
pub(super) const STATUS_JSON_NAME: &str = "status.json";

static TOKIO_RUNTIME: OnceLock<Runtime> = OnceLock::new();
static REPLY_GUARD_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub(super) struct VfsCancellation {
    pub(super) generation: u64,
    pub(super) errno: i32,
    pub(super) reason: String,
}

impl VfsCancellation {
    pub(super) fn initial() -> Self {
        Self {
            generation: 0,
            errno: libc::EIO,
            reason: "initial".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum VfsOp {
    Attributes,
    Lookup,
    Enumerate,
    Read,
    Write,
    Mutate,
}

impl VfsOp {
    pub(super) fn deadline(self) -> Duration {
        match self {
            Self::Read => Duration::from_secs(3),
            Self::Attributes | Self::Lookup | Self::Enumerate => Duration::from_secs(5),
            Self::Write | Self::Mutate => Duration::from_secs(10),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Attributes => "attributes",
            Self::Lookup => "lookup",
            Self::Enumerate => "enumerate",
            Self::Read => "read",
            Self::Write => "write",
            Self::Mutate => "mutate",
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ObjcTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

unsafe impl Encode for ObjcTimespec {
    const ENCODING: Encoding = Encoding::Struct("timespec", &[i64::ENCODING, i64::ENCODING]);
}

unsafe impl RefEncode for ObjcTimespec {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

pub(super) fn runtime() -> &'static Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .thread_name("vixen-fskit-vox")
            .build()
            .expect("failed to create Vixen FSKit tokio runtime")
    })
}

pub(super) async fn await_vfs<F, T>(
    op: VfsOp,
    mut cancel_rx: watch::Receiver<VfsCancellation>,
    future: F,
) -> Result<T, i32>
where
    F: Future<Output = Result<T, i32>>,
{
    await_vfs_with_deadline(op, op.deadline(), &mut cancel_rx, future).await
}

async fn await_vfs_with_deadline<F, T>(
    op: VfsOp,
    deadline: Duration,
    cancel_rx: &mut watch::Receiver<VfsCancellation>,
    future: F,
) -> Result<T, i32>
where
    F: Future<Output = Result<T, i32>>,
{
    let start_generation = cancel_rx.borrow().generation;
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => result,
        () = tokio::time::sleep(deadline) => {
            log_event(
                "error",
                format!(
                    "vixen-fskit vfs rpc timed out op={} deadline_ms={}",
                    op.label(),
                    deadline.as_millis()
                ),
            );
            Err(libc::ETIMEDOUT)
        }
        changed = cancel_rx.changed() => {
            match changed {
                Ok(()) => {
                    let cancellation = cancel_rx.borrow().clone();
                    if cancellation.generation == start_generation {
                        Err(libc::EIO)
                    } else {
                        log_event(
                            "warn",
                            format!(
                                "vixen-fskit vfs rpc cancelled op={} errno={} reason={}",
                                op.label(),
                                cancellation.errno,
                                cancellation.reason
                            ),
                        );
                        Err(cancellation.errno)
                    }
                }
                Err(_) => Err(libc::EIO),
            }
        }
    }
}

pub(super) fn run_vfs<F, T>(
    op: VfsOp,
    cancel_rx: watch::Receiver<VfsCancellation>,
    future: F,
) -> Result<T, i32>
where
    F: Future<Output = Result<T, i32>>,
{
    runtime()
        .handle()
        .block_on(await_vfs(op, cancel_rx, future))
}

pub(super) fn lock_unpoisoned<'a, T>(mutex: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        log_event(
            "error",
            format!("vixen-fskit recovered poisoned mutex name={name}"),
        );
        poisoned.into_inner()
    })
}

pub(super) struct RetainedObjc<T: Message + ?Sized> {
    ptr: std::ptr::NonNull<T>,
}

unsafe impl<T: Message + ?Sized> Send for RetainedObjc<T> {}

impl<T: Message> RetainedObjc<T> {
    pub(super) fn retain(object: &T) -> Option<Self> {
        let ptr = object as *const T as *mut T;
        let retained = unsafe { Retained::retain(ptr)? };
        let raw = Retained::into_raw(retained);
        Some(Self {
            ptr: std::ptr::NonNull::new(raw)?,
        })
    }

    pub(super) unsafe fn get(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: Message + ?Sized> Drop for RetainedObjc<T> {
    fn drop(&mut self) {
        unsafe {
            let _ = Retained::from_raw(self.ptr.as_ptr());
        }
    }
}

struct SendBlock<F: ?Sized>(RcBlock<F>);

unsafe impl<F: ?Sized> Send for SendBlock<F> {}

impl<F: ?Sized> SendBlock<F> {
    fn copy(block: &block2::DynBlock<F>) -> Self {
        Self(block.copy())
    }

    fn call(&self, args: F::Args) -> F::Output
    where
        F: BlockFn,
    {
        self.0.call(args)
    }
}

pub(super) trait ReplyErrorArgs {
    fn error(errno: i32) -> Self;
}

impl ReplyErrorArgs for (*mut NSError,) {
    fn error(errno: i32) -> Self {
        (Retained::into_raw(posix_error(errno)),)
    }
}

impl ReplyErrorArgs for (*mut FSItemAttributes, *mut NSError) {
    fn error(errno: i32) -> Self {
        (ptr::null_mut(), Retained::into_raw(posix_error(errno)))
    }
}

impl ReplyErrorArgs for (*mut FSItem, *mut FSFileName, *mut NSError) {
    fn error(errno: i32) -> Self {
        (
            ptr::null_mut(),
            ptr::null_mut(),
            Retained::into_raw(posix_error(errno)),
        )
    }
}

impl ReplyErrorArgs for (*mut FSFileName, *mut NSError) {
    fn error(errno: i32) -> Self {
        (ptr::null_mut(), Retained::into_raw(posix_error(errno)))
    }
}

impl ReplyErrorArgs for (FSDirectoryVerifier, *mut NSError) {
    fn error(errno: i32) -> Self {
        (0, Retained::into_raw(posix_error(errno)))
    }
}

impl ReplyErrorArgs for (usize, *mut NSError) {
    fn error(errno: i32) -> Self {
        (0, Retained::into_raw(posix_error(errno)))
    }
}

impl ReplyErrorArgs for (*mut FSVolume, *mut NSError) {
    fn error(errno: i32) -> Self {
        (ptr::null_mut(), Retained::into_raw(posix_error(errno)))
    }
}

impl ReplyErrorArgs for (*mut FSProbeResult, *mut NSError) {
    fn error(errno: i32) -> Self {
        (ptr::null_mut(), Retained::into_raw(posix_error(errno)))
    }
}

impl ReplyErrorArgs for (*mut NSData, *mut NSError) {
    fn error(errno: i32) -> Self {
        (ptr::null_mut(), Retained::into_raw(posix_error(errno)))
    }
}

impl ReplyErrorArgs for (*mut NSArray<FSFileName>, *mut NSError) {
    fn error(errno: i32) -> Self {
        (ptr::null_mut(), Retained::into_raw(posix_error(errno)))
    }
}

pub(super) struct ReplyGuard<F: ?Sized>
where
    F: BlockFn,
    F::Args: ReplyErrorArgs,
{
    id: u64,
    label: &'static str,
    block: Option<SendBlock<F>>,
}

impl<F: ?Sized> ReplyGuard<F>
where
    F: BlockFn,
    F::Args: ReplyErrorArgs,
{
    pub(super) fn copy(label: &'static str, block: &block2::DynBlock<F>) -> Self {
        Self {
            id: REPLY_GUARD_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            label,
            block: Some(SendBlock::copy(block)),
        }
    }

    pub(super) fn reply(mut self, args: F::Args) {
        if let Some(block) = self.block.take() {
            block.call(args);
        }
    }

    pub(super) fn error(self, errno: i32) {
        self.reply(F::Args::error(errno));
    }
}

impl<F: ?Sized> Drop for ReplyGuard<F>
where
    F: BlockFn,
    F::Args: ReplyErrorArgs,
{
    fn drop(&mut self) {
        if let Some(block) = self.block.take() {
            log_event(
                "error",
                format!(
                    "vixen-fskit reply guard dropped without reply id={} label={}",
                    self.id, self.label
                ),
            );
            block.call(F::Args::error(libc::EIO));
        }
    }
}

pub(super) fn fs_filename(name: &str) -> Retained<FSFileName> {
    unsafe { FSFileName::initWithString(FSFileName::alloc(), &NSString::from_str(name)) }
}

pub(super) fn fs_filename_string(name: &FSFileName) -> String {
    if let Some(string) = unsafe { name.string() } {
        return string.to_string();
    }
    let data = unsafe { name.data() };
    String::from_utf8_lossy(&data.to_vec()).into_owned()
}

pub(super) fn posix_error(code: i32) -> Retained<NSError> {
    NSError::new(code as isize, unsafe { NSPOSIXErrorDomain })
}

pub(super) fn fs_item_type(item_type: VfsItemType) -> FSItemType {
    match item_type {
        VfsItemType::File => FSItemType::File,
        VfsItemType::Directory => FSItemType::Directory,
        VfsItemType::Symlink => FSItemType::Symlink,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn fs_item_attributes(
    _request: Option<&FSItemGetAttributesRequest>,
    item_id: u64,
    parent_id: u64,
    item_type: FSItemType,
    size: u64,
    modified_time: u64,
    created_time: u64,
    mode: u32,
) -> Retained<FSItemAttributes> {
    let attrs = unsafe { FSItemAttributes::new() };
    unsafe {
        attrs.setType(item_type);
        attrs.setMode(mode);
        attrs.setFileID(FSItemID(item_id));
        attrs.setParentID(FSItemID(parent_id));
        attrs.setLinkCount(1);
        attrs.setFlags(0);
        attrs.setSize(size);
        attrs.setAllocSize(size);
        attrs.setUid(libc::getuid());
        attrs.setGid(libc::getgid());
        // TODO: is that _truly_ necessary?
        let _: () = msg_send![&*attrs, setModifyTime: objc_timespec(modified_time)];
        let _: () = msg_send![&*attrs, setChangeTime: objc_timespec(modified_time)];
        let _: () = msg_send![&*attrs, setAccessTime: objc_timespec(modified_time)];
        let _: () = msg_send![&*attrs, setBirthTime: objc_timespec(created_time)];
    }
    attrs
}

pub(super) fn attr_valid_mask(attrs: &FSItemAttributes) -> isize {
    let tracked = [
        FSItemAttribute::Type,
        FSItemAttribute::Mode,
        FSItemAttribute::LinkCount,
        FSItemAttribute::UID,
        FSItemAttribute::GID,
        FSItemAttribute::Flags,
        FSItemAttribute::Size,
        FSItemAttribute::AllocSize,
        FSItemAttribute::FileID,
        FSItemAttribute::ParentID,
        FSItemAttribute::AccessTime,
        FSItemAttribute::ModifyTime,
        FSItemAttribute::ChangeTime,
        FSItemAttribute::BirthTime,
    ];
    tracked.into_iter().fold(0, |mask, attribute| {
        if unsafe { attrs.isValid(attribute) } {
            mask | attribute.0
        } else {
            mask
        }
    })
}

pub(super) fn vox_rpc_errno(context: &str, error: impl Display) -> i32 {
    log_event("error", format!("vox rpc {context} failed: {error}"));
    libc::EIO
}

pub(super) fn entry_attributes(entry: &DirEntry) -> VfsItemAttributes {
    VfsItemAttributes {
        item_id: entry.item_id,
        item_type: entry.item_type,
        size: entry.size,
        modified_time: entry.modified_time,
        created_time: entry.created_time,
        mode: entry.mode,
    }
}

pub(super) fn unix_time() -> u64 {
    let mut now = 0;
    unsafe {
        libc::time(&mut now);
    }
    now as u64
}

fn objc_timespec(seconds: u64) -> ObjcTimespec {
    ObjcTimespec {
        tv_sec: seconds.min(i64::MAX as u64) as i64,
        tv_nsec: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[tokio::test]
    async fn hanging_vfs_future_returns_deadline_error() {
        let (_tx, mut rx) = watch::channel(VfsCancellation::initial());
        let result = await_vfs_with_deadline(
            VfsOp::Read,
            Duration::from_millis(10),
            &mut rx,
            std::future::pending::<Result<(), i32>>(),
        )
        .await;

        assert_eq!(result, Err(libc::ETIMEDOUT));
    }

    #[tokio::test]
    async fn cancellation_wakes_hanging_vfs_future() {
        let (tx, mut rx) = watch::channel(VfsCancellation::initial());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            tx.send_replace(VfsCancellation {
                generation: 1,
                errno: libc::ESTALE,
                reason: "test cancellation".to_owned(),
            });
        });

        let result = await_vfs_with_deadline(
            VfsOp::Lookup,
            Duration::from_secs(30),
            &mut rx,
            std::future::pending::<Result<(), i32>>(),
        )
        .await;

        assert_eq!(result, Err(libc::ESTALE));
    }

    #[test]
    fn reply_guard_drop_replies_with_error() {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |error: *mut NSError| {
            assert!(tx.send(error.is_null()).is_ok());
        });
        let guard = ReplyGuard::<dyn Fn(*mut NSError)>::copy("drop-test", &block);

        drop(guard);

        match rx.recv() {
            Ok(replied_with_null) => assert!(!replied_with_null),
            Err(error) => panic!("reply guard did not call test block: {error}"),
        }
    }
}
