use std::fmt::Display;
use std::sync::OnceLock;

use objc2::AnyThread;
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_foundation::{NSError, NSPOSIXErrorDomain, NSString};
use objc2_fs_kit::{
    FSFileName, FSItemAttribute, FSItemAttributes, FSItemGetAttributesRequest, FSItemID, FSItemType,
};
use tokio::runtime::{Builder, Runtime};
use vx_services::{DirEntry, ItemAttributes as VfsItemAttributes, ItemType as VfsItemType};

use super::log_event;
pub(super) const ROOT_LOCAL_ID: u64 = FSItemID::RootDirectory.0;
pub(super) const ROOT_BACKEND_ID: u64 = 1;
pub(super) const VIXENFS_DIR_ID: u64 = 0xFFFF_FFFF_FFFE_0001;
pub(super) const STATUS_JSON_ID: u64 = 0xFFFF_FFFF_FFFE_0002;
pub(super) const ROUTED_ITEM_ID_START: u64 = ROOT_LOCAL_ID + 1;
pub(super) const VOLUME_NAME: &str = "Vixen";
pub(super) const APP_GROUP_ID: &str = "B2N6FSRTPV.vixenfs";
pub(super) const SOCKET_NAME: &str = "vixenfs.sock";
pub(super) const VIXENFS_DIR_NAME: &str = ".vixenfs";
pub(super) const STATUS_JSON_NAME: &str = "status.json";

static TOKIO_RUNTIME: OnceLock<Runtime> = OnceLock::new();

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

pub(super) fn block_on_vfs<F, T>(future: F) -> Result<T, Retained<NSError>>
where
    F: std::future::Future<Output = Result<T, i32>>,
{
    runtime()
        .handle()
        .block_on(async move { future.await.map_err(posix_error) })
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

pub(super) fn json_string(value: impl AsRef<str>) -> String {
    let mut out = String::from("\"");
    for ch in value.as_ref().chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
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
