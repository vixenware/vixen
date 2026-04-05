#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ptr;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSData, NSError, NSObjectProtocol, NSString, NSUUID};
use objc2_fs_kit::{
    FSAccessMask, FSContainerStatus, FSDirectoryCookie, FSDirectoryCookieInitial,
    FSDirectoryEntryPacker, FSDirectoryVerifier, FSFileName, FSFileSystemBase, FSItem,
    FSItemAttribute, FSItemAttributes, FSItemGetAttributesRequest, FSItemID, FSItemType,
    FSMutableFileDataBuffer, FSSetXattrPolicy, FSStatFSResult, FSSyncFlags, FSTaskOptions,
    FSUnaryFileSystem, FSVolume, FSVolumeAccessCheckOperations, FSVolumeIdentifier,
    FSVolumeOpenCloseOperations, FSVolumeOpenModes, FSVolumeOperations, FSVolumePathConfOperations,
    FSVolumeReadWriteOperations, FSVolumeSupportedCapabilities, FSVolumeXattrOperations,
};
use vx_services::{
    CreateResult, GetAttributesResult, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, VfsResult, WriteResult,
};

use super::item::{RustItem, VirtualKind, as_rust_item};
use super::log_event;
use super::root_peer::root_peer_state;
use super::support::{
    ROOT_BACKEND_ID, ROOT_LOCAL_ID, ROUTED_ITEM_ID_START, STATUS_JSON_ID, STATUS_JSON_NAME,
    VIXENFS_DIR_ID, VIXENFS_DIR_NAME, VOLUME_NAME, attr_valid_mask, block_on_vfs, entry_attributes,
    fs_filename, fs_filename_string, posix_error, vox_rpc_errno,
};

mod access;
mod helpers;
mod open_close;
mod ops;
mod path_conf;
mod read_write;
mod xattrs;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RoutedItemKey {
    connection_generation: u64,
    backend_item_id: u64,
}

struct RoutedItems {
    by_key: HashMap<RoutedItemKey, u64>,
    next_local_id: u64,
}

pub(super) struct VolumeIvars {
    file_system: Retained<FSUnaryFileSystem>,
    items: Mutex<HashMap<u64, Retained<RustItem>>>,
    routed: Mutex<RoutedItems>,
}

define_class!(
    #[unsafe(super = FSVolume)]
    #[ivars = VolumeIvars]
    pub(super) struct RustVolume;

    unsafe impl NSObjectProtocol for RustVolume {}
);
