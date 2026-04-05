#![allow(non_snake_case)]

use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use block2::RcBlock;
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSData, NSError, NSObjectProtocol, NSPOSIXErrorDomain, NSString, NSUUID,
};
use objc2_fs_kit::{
    FSAccessMask, FSContainerStatus, FSDeactivateOptions, FSDirectoryCookie,
    FSDirectoryCookieInitial, FSDirectoryEntryPacker, FSDirectoryVerifier, FSFileName,
    FSFileSystemBase, FSItem, FSItemAttribute, FSItemAttributes, FSItemGetAttributesRequest,
    FSItemID, FSItemType, FSMutableFileDataBuffer, FSProbeResult, FSResource, FSSetXattrPolicy,
    FSStatFSResult, FSTaskOptions, FSUnaryFileSystem, FSUnaryFileSystemOperations, FSVolume,
    FSVolumeAccessCheckOperations, FSVolumeIdentifier, FSVolumeOpenCloseOperations,
    FSVolumeOpenModes, FSVolumeOperations, FSVolumePathConfOperations, FSVolumeReadWriteOperations,
    FSVolumeSupportedCapabilities, FSVolumeXattrOperations,
};
use tokio::runtime::{Builder, Runtime};
use vox::{
    ClientLogging, ClientLoggingOptions, ConnectionAcceptor, ConnectionRequest, Metadata,
    PendingConnection, acceptor_on,
};
use vx_services::{
    CreateResult, DirEntry, GetAttributesResult, ItemAttributes as VfsItemAttributes,
    ItemType as VfsItemType, LookupResult, ReadDirResult, ReadResult, SetAttributesParams,
    VfsClient, VfsResult, WriteResult,
};

use crate::{log_event, real_home_dir};

const ROOT_LOCAL_ID: u64 = FSItemID::RootDirectory.0;
const ROOT_BACKEND_ID: u64 = 1;
const VIXENFS_DIR_ID: u64 = 0xFFFF_FFFF_FFFE_0001;
const STATUS_JSON_ID: u64 = 0xFFFF_FFFF_FFFE_0002;
const ROUTED_ITEM_ID_START: u64 = ROOT_LOCAL_ID + 1;
const DIRECTORY_VERIFIER: FSDirectoryVerifier = 1;
const VOLUME_NAME: &str = "Vixen";
const APP_GROUP_ID: &str = "B2N6FSRTPV.vixenfs";
const SOCKET_NAME: &str = "vixenfs.sock";
const VIXENFS_DIR_NAME: &str = ".vixenfs";
const STATUS_JSON_NAME: &str = "status.json";

static TOKIO_RUNTIME: OnceLock<Runtime> = OnceLock::new();
static ROOT_PEER_STATE: OnceLock<RootPeerState> = OnceLock::new();

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum VirtualKind {
    Root,
    MetaDir,
    StatusJson,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RoutedItemKey {
    connection_generation: u64,
    backend_item_id: u64,
}

struct CachedAttrs {
    parent_local_id: u64,
    item_type: FSItemType,
    size: u64,
    modified_time: u64,
    created_time: u64,
    mode: u32,
}

struct ItemIvars {
    local_item_id: u64,
    backend_item_id: u64,
    connection_generation: u64,
    name: Retained<FSFileName>,
    virtual_kind: VirtualKind,
    attrs: Mutex<CachedAttrs>,
}

define_class!(
    #[unsafe(super = FSItem)]
    #[ivars = ItemIvars]
    struct RustItem;

    unsafe impl NSObjectProtocol for RustItem {}
);

impl RustItem {
    fn new(
        local_item_id: u64,
        backend_item_id: u64,
        connection_generation: u64,
        parent_local_id: u64,
        name: &str,
        item_type: FSItemType,
        size: u64,
        modified_time: u64,
        created_time: u64,
        mode: u32,
        virtual_kind: VirtualKind,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ItemIvars {
            local_item_id,
            backend_item_id,
            connection_generation,
            name: fs_filename(name),
            virtual_kind,
            attrs: Mutex::new(CachedAttrs {
                parent_local_id,
                item_type,
                size,
                modified_time,
                created_time,
                mode,
            }),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn root(connection_generation: u64) -> Retained<Self> {
        Self::new(
            ROOT_LOCAL_ID,
            ROOT_BACKEND_ID,
            connection_generation,
            ROOT_LOCAL_ID,
            "/",
            FSItemType::Directory,
            0,
            0,
            0,
            0o755,
            VirtualKind::Root,
        )
    }

    fn meta_dir() -> Retained<Self> {
        Self::new(
            VIXENFS_DIR_ID,
            VIXENFS_DIR_ID,
            0,
            ROOT_LOCAL_ID,
            VIXENFS_DIR_NAME,
            FSItemType::Directory,
            0,
            0,
            0,
            0o755,
            VirtualKind::MetaDir,
        )
    }

    fn status_json(len: usize) -> Retained<Self> {
        let now = unix_time();
        Self::new(
            STATUS_JSON_ID,
            STATUS_JSON_ID,
            0,
            VIXENFS_DIR_ID,
            STATUS_JSON_NAME,
            FSItemType::File,
            len as u64,
            now,
            0,
            0o444,
            VirtualKind::StatusJson,
        )
    }

    fn from_backend(
        local_item_id: u64,
        parent_local_id: u64,
        connection_generation: u64,
        name: &str,
        attrs: &VfsItemAttributes,
    ) -> Retained<Self> {
        Self::new(
            local_item_id,
            attrs.item_id,
            connection_generation,
            parent_local_id,
            name,
            fs_item_type(attrs.item_type),
            attrs.size,
            attrs.modified_time,
            attrs.created_time,
            attrs.mode,
            VirtualKind::None,
        )
    }

    fn item_id(&self) -> u64 {
        self.ivars().local_item_id
    }

    fn backend_item_id(&self) -> u64 {
        self.ivars().backend_item_id
    }

    fn generation(&self) -> u64 {
        self.ivars().connection_generation
    }

    fn virtual_kind(&self) -> VirtualKind {
        self.ivars().virtual_kind
    }

    fn parent_local_id(&self) -> u64 {
        self.ivars()
            .attrs
            .lock()
            .expect("item attrs mutex poisoned")
            .parent_local_id
    }

    fn fs_name(&self) -> Retained<FSFileName> {
        self.ivars().name.clone()
    }

    fn attributes(&self) -> Retained<FSItemAttributes> {
        self.attributes_for_request(None)
    }

    fn attributes_for_request(
        &self,
        request: Option<&FSItemGetAttributesRequest>,
    ) -> Retained<FSItemAttributes> {
        let attrs = self
            .ivars()
            .attrs
            .lock()
            .expect("item attrs mutex poisoned");
        fs_item_attributes(
            request,
            self.item_id(),
            attrs.parent_local_id,
            attrs.item_type,
            attrs.size,
            attrs.modified_time,
            attrs.created_time,
            attrs.mode,
        )
    }

    fn update_from_backend(&self, parent_local_id: u64, attrs: &VfsItemAttributes) {
        let mut cached = self
            .ivars()
            .attrs
            .lock()
            .expect("item attrs mutex poisoned");
        cached.parent_local_id = parent_local_id;
        cached.item_type = fs_item_type(attrs.item_type);
        cached.size = attrs.size;
        cached.modified_time = attrs.modified_time;
        cached.created_time = attrs.created_time;
        cached.mode = attrs.mode;
    }

    fn update_status_len(&self, len: usize) {
        let mut cached = self
            .ivars()
            .attrs
            .lock()
            .expect("item attrs mutex poisoned");
        cached.size = len as u64;
        cached.modified_time = unix_time();
    }

    fn debug_summary(&self) -> String {
        let cached = self
            .ivars()
            .attrs
            .lock()
            .expect("item attrs mutex poisoned");
        format!(
            "local={} backend={} parent={} gen={} name={} kind={} type={:?} mode={:#o} size={}",
            self.ivars().local_item_id,
            self.ivars().backend_item_id,
            cached.parent_local_id,
            self.ivars().connection_generation,
            fs_filename_string(&self.ivars().name),
            self.virtual_kind().label(),
            cached.item_type,
            cached.mode,
            cached.size
        )
    }
}

impl VirtualKind {
    fn label(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::MetaDir => "meta_dir",
            Self::StatusJson => "status_json",
            Self::None => "backend",
        }
    }
}

struct RoutedItems {
    by_key: HashMap<RoutedItemKey, u64>,
    next_local_id: u64,
}

struct VolumeIvars {
    file_system: Retained<FSUnaryFileSystem>,
    items: Mutex<HashMap<u64, Retained<RustItem>>>,
    routed: Mutex<RoutedItems>,
}

define_class!(
    #[unsafe(super = FSVolume)]
    #[ivars = VolumeIvars]
    struct RustVolume;

    unsafe impl NSObjectProtocol for RustVolume {}

    unsafe impl FSVolumePathConfOperations for RustVolume {
        #[unsafe(method(maximumLinkCount))]
        fn maximumLinkCount(&self) -> isize {
            1
        }

        #[unsafe(method(maximumNameLength))]
        fn maximumNameLength(&self) -> isize {
            255
        }

        #[unsafe(method(restrictsOwnershipChanges))]
        fn restrictsOwnershipChanges(&self) -> bool {
            true
        }

        #[unsafe(method(truncatesLongNames))]
        fn truncatesLongNames(&self) -> bool {
            false
        }

        #[unsafe(method(maximumXattrSize))]
        fn maximumXattrSize(&self) -> isize {
            64 * 1024
        }

        #[unsafe(method(maximumXattrSizeInBits))]
        fn maximumXattrSizeInBits(&self) -> isize {
            17
        }

        #[unsafe(method(maximumFileSize))]
        fn maximumFileSize(&self) -> u64 {
            i64::MAX as u64
        }

        #[unsafe(method(maximumFileSizeInBits))]
        fn maximumFileSizeInBits(&self) -> isize {
            64
        }
    }

    unsafe impl FSVolumeOperations for RustVolume {
        #[unsafe(method(supportedVolumeCapabilities))]
        fn supportedVolumeCapabilities(&self) -> *mut FSVolumeSupportedCapabilities {
            Retained::into_raw(unsafe { FSVolumeSupportedCapabilities::new() })
        }

        #[unsafe(method(volumeStatistics))]
        fn volumeStatistics(&self) -> *mut FSStatFSResult {
            let stats = unsafe {
                FSStatFSResult::initWithFileSystemTypeName(
                    FSStatFSResult::alloc(),
                    &NSString::from_str("vixenfs"),
                )
            };
            unsafe {
                stats.setBlockSize(4096);
                stats.setIoSize(262_144);
                stats.setTotalBlocks(1_000_000);
                stats.setFreeBlocks(500_000);
                stats.setAvailableBlocks(500_000);
                stats.setTotalFiles(100_000);
                stats.setFreeFiles(50_000);
            }
            Retained::into_raw(stats)
        }

        #[unsafe(method(activateWithOptions:replyHandler:))]
        fn activateWithOptions_replyHandler(
            &self,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut NSError)>,
        ) {
            log_event("info", "fskit-poc activateWithOptions");
            let active = unsafe { FSContainerStatus::active() };
            unsafe { FSFileSystemBase::setContainerStatus(&*self.ivars().file_system, &active) };
            let root = self.root_item();
            if let Some(root_item) = as_rust_item(root.as_ref()) {
                log_event(
                    "info",
                    format!(
                        "fskit-poc activateWithOptions root {}",
                        root_item.debug_summary()
                    ),
                );
            }
            reply.call((Retained::into_raw(root), ptr::null_mut()));
        }

        #[unsafe(method(mountWithOptions:replyHandler:))]
        fn mountWithOptions_replyHandler(
            &self,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            log_event("info", "fskit-poc mountWithOptions");
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(deactivateWithOptions:replyHandler:))]
        fn deactivateWithOptions_replyHandler(
            &self,
            _options: FSDeactivateOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            log_event("info", "fskit-poc deactivateWithOptions");
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(unmountWithReplyHandler:))]
        fn unmountWithReplyHandler(&self, reply: &block2::DynBlock<dyn Fn()>) {
            log_event("info", "fskit-poc unmount");
            let ready = unsafe { FSContainerStatus::ready() };
            unsafe { FSFileSystemBase::setContainerStatus(&*self.ivars().file_system, &ready) };
            reply.call(());
        }

        #[unsafe(method(synchronizeWithFlags:replyHandler:))]
        fn synchronizeWithFlags_replyHandler(
            &self,
            _flags: objc2_fs_kit::FSSyncFlags,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            log_event("info", "fskit-poc synchronize");
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(getAttributes:ofItem:replyHandler:))]
        fn getAttributes_ofItem_replyHandler(
            &self,
            _desired_attributes: &FSItemGetAttributesRequest,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        ) {
            let Some(item) = as_rust_item(item) else {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
                return;
            };
            log_event(
                "debug",
                format!(
                    "fskit-poc getAttributes wanted_mask={:#x} {}",
                    unsafe { _desired_attributes.wantedAttributes().0 },
                    item.debug_summary()
                ),
            );

            match self.attributes_for_item(item, Some(_desired_attributes)) {
                Ok(attrs) => {
                    log_event(
                        "debug",
                        format!(
                            "fskit-poc getAttributes.reply valid_mask={:#x} file_id={} parent={} type={:?} mode={:#o} size={}",
                            attr_valid_mask(&attrs),
                            unsafe { attrs.fileID().0 },
                            unsafe { attrs.parentID().0 },
                            unsafe { attrs.r#type() },
                            unsafe { attrs.mode() },
                            unsafe { attrs.size() }
                        ),
                    );
                    reply.call((Retained::into_raw(attrs), ptr::null_mut()))
                }
                Err(error) => reply.call((ptr::null_mut(), Retained::into_raw(error))),
            }
        }

        #[unsafe(method(setAttributes:onItem:replyHandler:))]
        fn setAttributes_onItem_replyHandler(
            &self,
            new_attributes: &objc2_fs_kit::FSItemSetAttributesRequest,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        ) {
            let Some(item) = as_rust_item(item) else {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
                return;
            };

            if item.virtual_kind() != VirtualKind::None || item.item_id() == ROOT_LOCAL_ID {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::EACCES)),
                ));
                return;
            }

            let params = SetAttributesParams {
                mode: unsafe {
                    new_attributes
                        .isValid(objc2_fs_kit::FSItemAttribute::Mode)
                        .then(|| new_attributes.mode() as u32)
                },
                modified_time: None,
                size: unsafe {
                    new_attributes
                        .isValid(objc2_fs_kit::FSItemAttribute::Size)
                        .then(|| new_attributes.size() as u64)
                },
            };

            let result = block_on_vfs({
                let backend_item_id = item.backend_item_id();
                async move {
                    let (client, _) = root_peer_state().connected_client()?;
                    let result = client
                        .set_attributes(backend_item_id, params)
                        .await
                        .map_err(|error| vox_rpc_errno("set_attributes", error))?;
                    Ok::<_, i32>(result)
                }
            });

            match result {
                Ok(VfsResult { error: 0 }) => match self.refresh_backend_item(item, None) {
                    Ok(attrs) => reply.call((Retained::into_raw(attrs), ptr::null_mut())),
                    Err(error) => reply.call((ptr::null_mut(), Retained::into_raw(error))),
                },
                Ok(VfsResult { error }) => {
                    reply.call((ptr::null_mut(), Retained::into_raw(posix_error(error))))
                }
                Err(error) => reply.call((ptr::null_mut(), Retained::into_raw(error))),
            }
        }

        #[unsafe(method(lookupItemNamed:inDirectory:replyHandler:))]
        fn lookupItemNamed_inDirectory_replyHandler(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            let Some(directory) = as_rust_item(directory) else {
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
                return;
            };
            let requested_name = fs_filename_string(name);
            log_event(
                "info",
                format!(
                    "fskit-poc lookupItem name={} dir={}",
                    requested_name,
                    directory.debug_summary()
                ),
            );

            match self.lookup_item(directory, name) {
                Ok((item, item_name)) => {
                    if let Some(item) = as_rust_item(item.as_ref()) {
                        log_event(
                            "info",
                            format!("fskit-poc lookupItem.reply {}", item.debug_summary()),
                        );
                    }
                    reply.call((
                        Retained::into_raw(item),
                        Retained::into_raw(item_name),
                        ptr::null_mut(),
                    ))
                }
                Err(error) => {
                    reply.call((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error)))
                }
            }
        }

        #[unsafe(method(createItemNamed:type:inDirectory:attributes:replyHandler:))]
        fn createItemNamed_type_inDirectory_attributes_replyHandler(
            &self,
            name: &FSFileName,
            item_type: FSItemType,
            directory: &FSItem,
            new_attributes: &objc2_fs_kit::FSItemSetAttributesRequest,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            let Some(directory) = as_rust_item(directory) else {
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
                return;
            };

            if directory.virtual_kind() != VirtualKind::None || directory.item_id() == ROOT_LOCAL_ID
            {
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::EACCES)),
                ));
                return;
            }

            let requested_name = fs_filename_string(name);
            let mode = if unsafe { new_attributes.isValid(objc2_fs_kit::FSItemAttribute::Mode) } {
                unsafe { new_attributes.mode() as u32 }
            } else if item_type == FSItemType::Directory {
                0o755
            } else {
                0o644
            };

            let create_type = if item_type == FSItemType::Directory {
                VfsItemType::Directory
            } else {
                VfsItemType::File
            };

            let result = block_on_vfs({
                let parent_backend_id = directory.backend_item_id();
                let requested_name = requested_name.clone();
                async move {
                    let (client, _) = root_peer_state().connected_client()?;
                    let result = client
                        .create(parent_backend_id, requested_name, create_type, mode)
                        .await
                        .map_err(|error| vox_rpc_errno("create", error))?;
                    Ok::<_, i32>(result)
                }
            });

            match result {
                Ok(CreateResult { item_id, error: 0 }) => {
                    let attrs_result = block_on_vfs(async move {
                        let (client, generation) = root_peer_state().connected_client()?;
                        let attrs = client
                            .get_attributes(item_id)
                            .await
                            .map_err(|error| vox_rpc_errno("get_attributes", error))?;
                        Ok::<_, i32>((attrs, generation))
                    });
                    match attrs_result {
                        Ok((GetAttributesResult { attrs, error: 0 }, generation)) => {
                            let local_item_id = self.local_item_id(generation, attrs.item_id);
                            let item = self.cached_backend_item(
                                local_item_id,
                                directory.item_id(),
                                generation,
                                &requested_name,
                                &attrs,
                            );
                            reply.call((
                                Retained::into_raw(item.clone().into_super()),
                                Retained::into_raw(item.fs_name()),
                                ptr::null_mut(),
                            ));
                        }
                        Ok((GetAttributesResult { error, .. }, _)) => {
                            reply.call((
                                ptr::null_mut(),
                                ptr::null_mut(),
                                Retained::into_raw(posix_error(error)),
                            ));
                        }
                        Err(error) => {
                            reply.call((
                                ptr::null_mut(),
                                ptr::null_mut(),
                                Retained::into_raw(error),
                            ));
                        }
                    }
                }
                Ok(CreateResult { error, .. }) => reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(error)),
                )),
                Err(error) => {
                    reply.call((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error)))
                }
            }
        }

        #[unsafe(method(removeItem:named:fromDirectory:replyHandler:))]
        fn removeItem_named_fromDirectory_replyHandler(
            &self,
            item: &FSItem,
            _name: &FSFileName,
            _directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let Some(item) = as_rust_item(item) else {
                reply.call((Retained::into_raw(posix_error(libc::ENOENT)),));
                return;
            };

            if item.virtual_kind() != VirtualKind::None || item.item_id() == ROOT_LOCAL_ID {
                reply.call((Retained::into_raw(posix_error(libc::EACCES)),));
                return;
            }

            let result = block_on_vfs({
                let backend_item_id = item.backend_item_id();
                async move {
                    let (client, _) = root_peer_state().connected_client()?;
                    let result = client
                        .delete(backend_item_id)
                        .await
                        .map_err(|error| vox_rpc_errno("delete", error))?;
                    Ok::<_, i32>(result)
                }
            });

            match result {
                Ok(VfsResult { error: 0 }) => {
                    self.remove_cached_item(item.item_id());
                    reply.call((ptr::null_mut(),));
                }
                Ok(VfsResult { error }) => reply.call((Retained::into_raw(posix_error(error)),)),
                Err(error) => reply.call((Retained::into_raw(error),)),
            }
        }

        #[unsafe(method(renameItem:inDirectory:named:toNewName:inDirectory:overItem:replyHandler:))]
        fn renameItem_inDirectory_named_toNewName_inDirectory_overItem_replyHandler(
            &self,
            item: &FSItem,
            _source_directory: &FSItem,
            _source_name: &FSFileName,
            destination_name: &FSFileName,
            destination_directory: &FSItem,
            _over_item: *mut FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            let Some(item) = as_rust_item(item) else {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
                return;
            };
            let Some(destination_directory) = as_rust_item(destination_directory) else {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
                return;
            };

            if item.virtual_kind() != VirtualKind::None
                || destination_directory.virtual_kind() != VirtualKind::None
                || item.item_id() == ROOT_LOCAL_ID
                || destination_directory.item_id() == ROOT_LOCAL_ID
            {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::EACCES)),
                ));
                return;
            }

            let new_name = fs_filename_string(destination_name);
            let result = block_on_vfs({
                let backend_item_id = item.backend_item_id();
                let new_parent_id = destination_directory.backend_item_id();
                let new_name = new_name.clone();
                async move {
                    let (client, _) = root_peer_state().connected_client()?;
                    let result = client
                        .rename(backend_item_id, new_parent_id, new_name)
                        .await
                        .map_err(|error| vox_rpc_errno("rename", error))?;
                    Ok::<_, i32>(result)
                }
            });

            match result {
                Ok(VfsResult { error: 0 }) => {
                    reply.call((Retained::into_raw(fs_filename(&new_name)), ptr::null_mut()))
                }
                Ok(VfsResult { error }) => {
                    reply.call((ptr::null_mut(), Retained::into_raw(posix_error(error))))
                }
                Err(error) => reply.call((ptr::null_mut(), Retained::into_raw(error))),
            }
        }

        #[unsafe(method(enumerateDirectory:startingAtCookie:verifier:providingAttributes:usingPacker:replyHandler:))]
        fn enumerateDirectory_startingAtCookie_verifier_providingAttributes_usingPacker_replyHandler(
            &self,
            directory: &FSItem,
            cookie: FSDirectoryCookie,
            verifier: FSDirectoryVerifier,
            attributes: Option<&FSItemGetAttributesRequest>,
            packer: &FSDirectoryEntryPacker,
            reply: &block2::DynBlock<dyn Fn(FSDirectoryVerifier, *mut NSError)>,
        ) {
            let Some(directory) = as_rust_item(directory) else {
                reply.call((0, Retained::into_raw(posix_error(libc::ENOENT))));
                return;
            };
            log_event(
                "info",
                format!(
                    "fskit-poc enumerateDirectory cookie={} verifier={} {}",
                    cookie,
                    verifier,
                    directory.debug_summary()
                ),
            );

            match self.enumerate_directory(directory, cookie, verifier, attributes, packer) {
                Ok(next_verifier) => reply.call((next_verifier, ptr::null_mut())),
                Err(error) => reply.call((0, Retained::into_raw(error))),
            }
        }

        #[unsafe(method(reclaimItem:replyHandler:))]
        fn reclaimItem_replyHandler(
            &self,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            if let Some(item) = as_rust_item(item) {
                log_event(
                    "info",
                    format!("fskit-poc reclaimItem item={}", item.item_id()),
                );
                self.remove_cached_item(item.item_id());
            }
            reply.call((ptr::null_mut(),));
        }
    }

    unsafe impl FSVolumeReadWriteOperations for RustVolume {
        #[unsafe(method(readFromFile:offset:length:intoBuffer:replyHandler:))]
        fn readFromFile_offset_length_intoBuffer_replyHandler(
            &self,
            item: &FSItem,
            offset: libc::off_t,
            length: usize,
            buffer: &FSMutableFileDataBuffer,
            reply: &block2::DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            let Some(item) = as_rust_item(item) else {
                reply.call((0, Retained::into_raw(posix_error(libc::ENOENT))));
                return;
            };
            log_event(
                "info",
                format!(
                    "fskit-poc readFromFile offset={} length={} {}",
                    offset,
                    length,
                    item.debug_summary()
                ),
            );

            match self.read_item(item, offset, length, buffer) {
                Ok(actual) => reply.call((actual, ptr::null_mut())),
                Err(error) => reply.call((0, Retained::into_raw(error))),
            }
        }

        #[unsafe(method(writeContents:toFile:atOffset:replyHandler:))]
        fn writeContents_toFile_atOffset_replyHandler(
            &self,
            contents: &NSData,
            item: &FSItem,
            offset: libc::off_t,
            reply: &block2::DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            let Some(item) = as_rust_item(item) else {
                reply.call((0, Retained::into_raw(posix_error(libc::ENOENT))));
                return;
            };

            if item.virtual_kind() != VirtualKind::None {
                reply.call((0, Retained::into_raw(posix_error(libc::EACCES))));
                return;
            }

            let bytes = contents.to_vec();
            let result = block_on_vfs({
                let backend_item_id = item.backend_item_id();
                let offset = offset.max(0) as u64;
                async move {
                    let (client, _) = root_peer_state().connected_client()?;
                    let result = client
                        .write(backend_item_id, offset, bytes)
                        .await
                        .map_err(|error| vox_rpc_errno("write", error))?;
                    Ok::<_, i32>(result)
                }
            });

            match result {
                Ok(WriteResult {
                    bytes_written,
                    error: 0,
                }) => {
                    reply.call((bytes_written as usize, ptr::null_mut()));
                }
                Ok(WriteResult { error, .. }) => {
                    reply.call((0, Retained::into_raw(posix_error(error))));
                }
                Err(error) => reply.call((0, Retained::into_raw(error))),
            }
        }
    }

    unsafe impl FSVolumeOpenCloseOperations for RustVolume {
        #[unsafe(method(isOpenCloseInhibited))]
        fn isOpenCloseInhibited(&self) -> bool {
            true
        }

        #[unsafe(method(openItem:withModes:replyHandler:))]
        fn openItem_withModes_replyHandler(
            &self,
            item: &FSItem,
            modes: FSVolumeOpenModes,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!("fskit-poc openItem modes={:#x} {}", modes.0, details),
            );
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(closeItem:keepingModes:replyHandler:))]
        fn closeItem_keepingModes_replyHandler(
            &self,
            item: &FSItem,
            modes: FSVolumeOpenModes,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!("fskit-poc closeItem keep_modes={:#x} {}", modes.0, details),
            );
            reply.call((ptr::null_mut(),));
        }
    }

    unsafe impl FSVolumeAccessCheckOperations for RustVolume {
        #[unsafe(method(isAccessCheckInhibited))]
        fn isAccessCheckInhibited(&self) -> bool {
            true
        }

        #[unsafe(method(checkAccessToItem:requestedAccess:replyHandler:))]
        fn checkAccessToItem_requestedAccess_replyHandler(
            &self,
            item: &FSItem,
            access: FSAccessMask,
            reply: &block2::DynBlock<dyn Fn(Bool, *mut NSError)>,
        ) {
            let details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "debug",
                format!("fskit-poc checkAccess access={:#x} {}", access.0, details),
            );
            reply.call((Bool::YES, ptr::null_mut()));
        }
    }

    unsafe impl FSVolumeXattrOperations for RustVolume {
        #[unsafe(method(xattrOperationsInhibited))]
        fn xattrOperationsInhibited(&self) -> bool {
            false
        }

        #[unsafe(method(getXattrNamed:ofItem:replyHandler:))]
        fn getXattrNamed_ofItem_replyHandler(
            &self,
            _name: &FSFileName,
            _item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSData, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                Retained::into_raw(posix_error(libc::ENOATTR)),
            ));
        }

        #[unsafe(method(setXattrNamed:toData:onItem:policy:replyHandler:))]
        fn setXattrNamed_toData_onItem_policy_replyHandler(
            &self,
            _name: &FSFileName,
            _value: Option<&NSData>,
            _item: &FSItem,
            _policy: FSSetXattrPolicy,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            reply.call((Retained::into_raw(posix_error(libc::ENOTSUP)),));
        }

        #[unsafe(method(listXattrsOfItem:replyHandler:))]
        fn listXattrsOfItem_replyHandler(
            &self,
            _item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSArray<FSFileName>, *mut NSError)>,
        ) {
            reply.call((ptr::null_mut(), ptr::null_mut()));
        }
    }
);

impl RustVolume {
    fn new(file_system: &FSUnaryFileSystem) -> Retained<Self> {
        let uuid = NSUUID::new();
        let volume_id =
            unsafe { FSVolumeIdentifier::initWithUUID(FSVolumeIdentifier::alloc(), &uuid) };
        let volume_name = fs_filename(VOLUME_NAME);
        let generation = root_peer_state().generation();
        let root = RustItem::root(generation);
        let meta_dir = RustItem::meta_dir();
        let status = RustItem::status_json(root_peer_state().status_json_bytes().len());
        let mut items = HashMap::new();
        items.insert(root.item_id(), root);
        items.insert(meta_dir.item_id(), meta_dir);
        items.insert(status.item_id(), status);

        let this = Self::alloc().set_ivars(VolumeIvars {
            file_system: unsafe {
                Retained::retain(file_system as *const FSUnaryFileSystem as *mut FSUnaryFileSystem)
                    .unwrap()
            },
            items: Mutex::new(items),
            routed: Mutex::new(RoutedItems {
                by_key: HashMap::new(),
                next_local_id: ROUTED_ITEM_ID_START,
            }),
        });
        unsafe { msg_send![super(this), initWithVolumeID: &*volume_id, volumeName: &*volume_name] }
    }

    fn root_item(&self) -> Retained<FSItem> {
        self.get_cached_item(ROOT_LOCAL_ID)
            .expect("root item should exist")
            .into_super()
    }

    fn get_cached_item(&self, item_id: u64) -> Option<Retained<RustItem>> {
        self.ivars()
            .items
            .lock()
            .expect("items mutex poisoned")
            .get(&item_id)
            .cloned()
    }

    fn cache_item(&self, item: Retained<RustItem>) -> Retained<RustItem> {
        self.ivars()
            .items
            .lock()
            .expect("items mutex poisoned")
            .insert(item.item_id(), item.clone());
        item
    }

    fn remove_cached_item(&self, item_id: u64) {
        if item_id == ROOT_LOCAL_ID || item_id == VIXENFS_DIR_ID || item_id == STATUS_JSON_ID {
            return;
        }
        self.ivars()
            .items
            .lock()
            .expect("items mutex poisoned")
            .remove(&item_id);
    }

    fn local_item_id(&self, generation: u64, backend_item_id: u64) -> u64 {
        if backend_item_id == ROOT_BACKEND_ID {
            log_event(
                "debug",
                format!(
                    "fskit-poc local_item_id root backend={} generation={} -> local={}",
                    backend_item_id, generation, ROOT_LOCAL_ID
                ),
            );
            return ROOT_LOCAL_ID;
        }

        let mut routed = self
            .ivars()
            .routed
            .lock()
            .expect("routed items mutex poisoned");
        let key = RoutedItemKey {
            connection_generation: generation,
            backend_item_id,
        };
        if let Some(existing) = routed.by_key.get(&key) {
            log_event(
                "debug",
                format!(
                    "fskit-poc local_item_id reuse backend={} generation={} -> local={}",
                    backend_item_id, generation, existing
                ),
            );
            return *existing;
        }
        let local_item_id = loop {
            let candidate = routed.next_local_id;
            routed.next_local_id += 1;
            if self.get_cached_item(candidate).is_none() {
                break candidate;
            }
            log_event(
                "debug",
                format!("fskit-poc local_item_id skip collision local={candidate}"),
            );
        };
        routed.by_key.insert(key, local_item_id);
        log_event(
            "debug",
            format!(
                "fskit-poc local_item_id insert backend={} generation={} -> local={}",
                backend_item_id, generation, local_item_id
            ),
        );
        local_item_id
    }

    fn cached_backend_item(
        &self,
        local_item_id: u64,
        parent_local_id: u64,
        generation: u64,
        name: &str,
        attrs: &VfsItemAttributes,
    ) -> Retained<RustItem> {
        if let Some(item) = self.get_cached_item(local_item_id) {
            log_event(
                "debug",
                format!(
                    "fskit-poc cached_backend_item hit local={} backend={} parent={} generation={} requested_name={} existing={}",
                    local_item_id,
                    attrs.item_id,
                    parent_local_id,
                    generation,
                    name,
                    item.debug_summary()
                ),
            );
            item.update_from_backend(parent_local_id, attrs);
            log_event(
                "debug",
                format!(
                    "fskit-poc cached_backend_item updated local={} backend={} now={}",
                    local_item_id,
                    attrs.item_id,
                    item.debug_summary()
                ),
            );
            return item;
        }
        let item = RustItem::from_backend(local_item_id, parent_local_id, generation, name, attrs);
        log_event(
            "debug",
            format!(
                "fskit-poc cached_backend_item insert requested_name={} {}",
                name,
                item.debug_summary()
            ),
        );
        self.cache_item(item)
    }

    fn attributes_for_item(
        &self,
        item: &RustItem,
        request: Option<&FSItemGetAttributesRequest>,
    ) -> Result<Retained<FSItemAttributes>, Retained<NSError>> {
        match item.virtual_kind() {
            VirtualKind::Root => Ok(item.attributes_for_request(request)),
            VirtualKind::MetaDir => Ok(item.attributes_for_request(request)),
            VirtualKind::StatusJson => {
                let bytes = root_peer_state().status_json_bytes();
                if let Some(status_item) = self.get_cached_item(STATUS_JSON_ID) {
                    status_item.update_status_len(bytes.len());
                    Ok(status_item.attributes_for_request(request))
                } else {
                    let status_item = self.cache_item(RustItem::status_json(bytes.len()));
                    Ok(status_item.attributes_for_request(request))
                }
            }
            VirtualKind::None => self.refresh_backend_item(item, request),
        }
    }

    fn refresh_backend_item(
        &self,
        item: &RustItem,
        request: Option<&FSItemGetAttributesRequest>,
    ) -> Result<Retained<FSItemAttributes>, Retained<NSError>> {
        let backend_item_id = item.backend_item_id();
        let parent_local_id = item.parent_local_id();
        let generation = item.generation();
        let result = block_on_vfs(async move {
            let (client, current_generation) = root_peer_state().connected_client()?;
            if generation != 0 && generation != current_generation {
                return Err(libc::ESTALE);
            }
            let result = client
                .get_attributes(backend_item_id)
                .await
                .map_err(|error| vox_rpc_errno("get_attributes", error))?;
            Ok::<_, i32>((result, current_generation))
        })?;

        let (result, current_generation) = result;
        if result.error != 0 {
            return Err(posix_error(result.error));
        }

        log_event(
            "debug",
            format!(
                "fskit-poc refresh_backend_item local={} backend={} generation={} current_generation={} result_backend={} type={:?} mode={:#o} size={}",
                item.item_id(),
                backend_item_id,
                generation,
                current_generation,
                result.attrs.item_id,
                result.attrs.item_type,
                result.attrs.mode,
                result.attrs.size
            ),
        );

        item.update_from_backend(parent_local_id, &result.attrs);
        if generation != 0 && current_generation != generation {
            return Err(posix_error(libc::ESTALE));
        }
        Ok(item.attributes_for_request(request))
    }

    fn lookup_item(
        &self,
        directory: &RustItem,
        name: &FSFileName,
    ) -> Result<(Retained<FSItem>, Retained<FSFileName>), Retained<NSError>> {
        let requested_name = fs_filename_string(name);

        if requested_name == "." {
            let item: Retained<FSItem> = self
                .get_cached_item(directory.item_id())
                .expect("directory should be cached")
                .into_super();
            return Ok((item, fs_filename(".")));
        }

        if requested_name == ".." {
            let parent_id = directory.parent_local_id();
            if let Some(parent) = self.get_cached_item(parent_id) {
                return Ok((parent.clone().into_super(), parent.fs_name()));
            }
        }

        if directory.item_id() == ROOT_LOCAL_ID && requested_name == VIXENFS_DIR_NAME {
            let item = self
                .get_cached_item(VIXENFS_DIR_ID)
                .expect(".vixenfs dir should be cached");
            return Ok((item.clone().into_super(), item.fs_name()));
        }

        if directory.item_id() == VIXENFS_DIR_ID && requested_name == STATUS_JSON_NAME {
            let bytes = root_peer_state().status_json_bytes();
            let item = if let Some(item) = self.get_cached_item(STATUS_JSON_ID) {
                item.update_status_len(bytes.len());
                item
            } else {
                self.cache_item(RustItem::status_json(bytes.len()))
            };
            return Ok((item.clone().into_super(), item.fs_name()));
        }

        if directory.item_id() == VIXENFS_DIR_ID {
            return Err(posix_error(libc::ENOENT));
        }

        let parent_backend_id = directory.backend_item_id();
        let parent_local_id = directory.item_id();
        let requested_name_for_rpc = requested_name.clone();
        let result = block_on_vfs(async move {
            let (client, generation) = root_peer_state().connected_client()?;
            let lookup = client
                .lookup(parent_backend_id, requested_name_for_rpc)
                .await
                .map_err(|error| vox_rpc_errno("lookup", error))?;
            Ok::<_, i32>((lookup, generation))
        })?;

        let (LookupResult { attrs, error }, generation) = result;
        if error != 0 {
            return Err(posix_error(error));
        }

        log_event(
            "debug",
            format!(
                "fskit-poc lookup_item.rpc name={} parent_backend={} generation={} result_backend={} type={:?} mode={:#o} size={}",
                requested_name,
                parent_backend_id,
                generation,
                attrs.item_id,
                attrs.item_type,
                attrs.mode,
                attrs.size
            ),
        );
        if requested_name != "." && requested_name != ".." && attrs.item_id == ROOT_BACKEND_ID {
            log_event(
                "error",
                format!(
                    "fskit-poc lookup_item.rpc returned root for name={} parent_backend={}",
                    requested_name, parent_backend_id
                ),
            );
        }

        let local_item_id = self.local_item_id(generation, attrs.item_id);
        log_event(
            "debug",
            format!(
                "fskit-poc lookup_item.map name={} backend={} generation={} -> local={}",
                requested_name, attrs.item_id, generation, local_item_id
            ),
        );
        let item = self.cached_backend_item(
            local_item_id,
            parent_local_id,
            generation,
            &requested_name,
            &attrs,
        );
        Ok((item.clone().into_super(), item.fs_name()))
    }

    fn enumerate_directory(
        &self,
        directory: &RustItem,
        cookie: FSDirectoryCookie,
        verifier: FSDirectoryVerifier,
        attributes: Option<&FSItemGetAttributesRequest>,
        packer: &FSDirectoryEntryPacker,
    ) -> Result<FSDirectoryVerifier, Retained<NSError>> {
        if directory.item_id() == VIXENFS_DIR_ID {
            let bytes = root_peer_state().status_json_bytes();
            let item = if let Some(item) = self.get_cached_item(STATUS_JSON_ID) {
                item.update_status_len(bytes.len());
                item
            } else {
                self.cache_item(RustItem::status_json(bytes.len()))
            };
            if cookie == unsafe { FSDirectoryCookieInitial } {
                let attrs = item.attributes_for_request(attributes);
                log_event(
                    "debug",
                    format!("fskit-poc enumerate virtual entry {}", item.debug_summary()),
                );
                let _ = unsafe {
                    packer.packEntryWithName_itemType_itemID_nextCookie_attributes(
                        &item.fs_name(),
                        FSItemType::File,
                        FSItemID(STATUS_JSON_ID),
                        0,
                        if attributes.is_some() {
                            Some(attrs.as_ref())
                        } else {
                            None
                        },
                    )
                };
            }
            return Ok(0);
        }

        if directory.item_id() == ROOT_LOCAL_ID {
            let start = if cookie == unsafe { FSDirectoryCookieInitial } {
                0
            } else {
                cookie.saturating_sub(1) as usize
            };

            let mut root_entries = Vec::<(Retained<RustItem>, u64)>::new();
            let meta_dir = self
                .get_cached_item(VIXENFS_DIR_ID)
                .expect(".vixenfs dir should be cached");
            root_entries.push((meta_dir, 1));

            let result = block_on_vfs(async move {
                let (client, generation) = root_peer_state().connected_client()?;
                let listing = client
                    .read_dir(ROOT_BACKEND_ID, 0, 0)
                    .await
                    .map_err(|error| vox_rpc_errno("read_dir", error))?;
                Ok::<_, i32>((listing, generation))
            });

            if let Ok((
                ReadDirResult {
                    entries, error: 0, ..
                },
                generation,
            )) = result
            {
                for entry in entries {
                    let local_item_id = self.local_item_id(generation, entry.item_id);
                    let item = self.cached_backend_item(
                        local_item_id,
                        ROOT_LOCAL_ID,
                        generation,
                        &entry.name,
                        &entry_attributes(&entry),
                    );
                    log_event(
                        "debug",
                        format!(
                            "fskit-poc enumerate root entry local={} backend={} name={} type={:?} size={}",
                            local_item_id, entry.item_id, entry.name, entry.item_type, entry.size
                        ),
                    );
                    let next_cookie = root_entries.len() as u64 + 1;
                    root_entries.push((item, next_cookie));
                }
            } else if let Ok((ReadDirResult { error, .. }, _)) = result {
                return Err(posix_error(error));
            } else if let Err(error) = result {
                return Err(error);
            }

            for (index, (item, next_cookie)) in root_entries.iter().enumerate().skip(start) {
                let attrs = item.attributes_for_request(attributes);
                let next_cookie = if index + 1 < root_entries.len() {
                    *next_cookie
                } else {
                    0
                };
                log_event(
                    "debug",
                    format!(
                        "fskit-poc pack root entry next_cookie={} {}",
                        next_cookie,
                        item.debug_summary()
                    ),
                );
                let packed = unsafe {
                    packer.packEntryWithName_itemType_itemID_nextCookie_attributes(
                        &item.fs_name(),
                        attrs.r#type(),
                        FSItemID(item.item_id()),
                        next_cookie,
                        if attributes.is_some() {
                            Some(attrs.as_ref())
                        } else {
                            None
                        },
                    )
                };
                if !packed {
                    break;
                }
            }

            return Ok(0);
        }

        let directory_backend_id = directory.backend_item_id();
        let directory_generation = directory.generation();
        let parent_local_id = directory.item_id();
        let result = block_on_vfs(async move {
            let (client, current_generation) = root_peer_state().connected_client()?;
            if directory_generation != 0 && directory_generation != current_generation {
                return Err(libc::ESTALE);
            }
            let listing = client
                .read_dir(directory_backend_id, cookie, verifier)
                .await
                .map_err(|error| vox_rpc_errno("read_dir", error))?;
            Ok::<_, i32>((listing, current_generation))
        })?;

        let (
            ReadDirResult {
                entries,
                next_cookie,
                verifier,
                error,
            },
            generation,
        ) = result;
        if error != 0 {
            return Err(posix_error(error));
        }

        for entry in entries {
            let local_item_id = self.local_item_id(generation, entry.item_id);
            let item = self.cached_backend_item(
                local_item_id,
                parent_local_id,
                generation,
                &entry.name,
                &entry_attributes(&entry),
            );
            let attrs = item.attributes_for_request(attributes);
            log_event(
                "debug",
                format!(
                    "fskit-poc enumerate entry local={} backend={} name={} type={:?} size={}",
                    local_item_id, entry.item_id, entry.name, entry.item_type, entry.size
                ),
            );
            let packed = unsafe {
                packer.packEntryWithName_itemType_itemID_nextCookie_attributes(
                    &item.fs_name(),
                    attrs.r#type(),
                    FSItemID(item.item_id()),
                    next_cookie,
                    if attributes.is_some() {
                        Some(attrs.as_ref())
                    } else {
                        None
                    },
                )
            };
            if !packed {
                break;
            }
        }

        Ok(verifier)
    }

    fn read_item(
        &self,
        item: &RustItem,
        offset: libc::off_t,
        length: usize,
        buffer: &FSMutableFileDataBuffer,
    ) -> Result<usize, Retained<NSError>> {
        log_event(
            "debug",
            format!(
                "fskit-poc read_item.begin offset={} length={} {}",
                offset,
                length,
                item.debug_summary()
            ),
        );
        if item.virtual_kind() == VirtualKind::StatusJson {
            let data = root_peer_state().status_json_bytes();
            let start = offset.max(0) as usize;
            let start = start.min(data.len());
            let end = (start + length).min(data.len());
            let slice = &data[start..end];
            let actual = slice.len().min(unsafe { buffer.length() });
            if actual > 0 {
                unsafe {
                    ptr::copy_nonoverlapping(
                        slice.as_ptr(),
                        buffer.mutableBytes().as_ptr().cast::<u8>(),
                        actual,
                    );
                }
            }
            log_event(
                "debug",
                format!(
                    "fskit-poc read_item.virtual actual={actual} {}",
                    item.debug_summary()
                ),
            );
            return Ok(actual);
        }

        let backend_item_id = item.backend_item_id();
        let offset = offset.max(0) as u64;
        let result = block_on_vfs(async move {
            let (client, _) = root_peer_state().connected_client()?;
            let result = client
                .read(backend_item_id, offset, length as u64)
                .await
                .map_err(|error| vox_rpc_errno("read", error))?;
            Ok::<_, i32>(result)
        })?;

        match result {
            ReadResult { data, error: 0 } => {
                let actual = data.len().min(unsafe { buffer.length() });
                if actual > 0 {
                    unsafe {
                        ptr::copy_nonoverlapping(
                            data.as_ptr(),
                            buffer.mutableBytes().as_ptr().cast::<u8>(),
                            actual,
                        );
                    }
                }
                log_event(
                    "info",
                    format!(
                        "fskit-poc read item={} backend={} offset={offset} actual={actual}",
                        item.item_id(),
                        item.backend_item_id()
                    ),
                );
                Ok(actual)
            }
            ReadResult { error, .. } => {
                log_event(
                    "error",
                    format!(
                        "fskit-poc read_item.error errno={} {}",
                        error,
                        item.debug_summary()
                    ),
                );
                Err(posix_error(error))
            }
        }
    }
}

define_class!(
    #[unsafe(super = FSUnaryFileSystem)]
    #[ivars = ()]
    struct RustUnaryFileSystem;

    unsafe impl NSObjectProtocol for RustUnaryFileSystem {}

    unsafe impl FSUnaryFileSystemOperations for RustUnaryFileSystem {
        #[unsafe(method(probeResource:replyHandler:))]
        fn probeResource_replyHandler(
            &self,
            _resource: &FSResource,
            reply: &block2::DynBlock<dyn Fn(*mut FSProbeResult, *mut NSError)>,
        ) {
            root_peer_state();
            log_event("info", "fskit-poc probeResource");
            let result = unsafe { FSProbeResult::usableButLimitedProbeResult() };
            reply.call((Retained::into_raw(result), ptr::null_mut()));
        }

        #[unsafe(method(loadResource:options:replyHandler:))]
        fn loadResource_options_replyHandler(
            &self,
            _resource: &FSResource,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut FSVolume, *mut NSError)>,
        ) {
            root_peer_state();
            log_event("info", "fskit-poc loadResource");
            let file_system = self.as_super();
            let ready = unsafe { FSContainerStatus::ready() };
            unsafe { FSFileSystemBase::setContainerStatus(file_system, &ready) };
            let volume: Retained<FSVolume> = RustVolume::new(file_system).into_super();
            reply.call((Retained::into_raw(volume), ptr::null_mut()));
        }

        #[unsafe(method(unloadResource:options:replyHandler:))]
        fn unloadResource_options_replyHandler(
            &self,
            _resource: &FSResource,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            log_event("info", "fskit-poc unloadResource");
            let file_system = self.as_super();
            let status =
                unsafe { FSContainerStatus::notReadyWithStatus(&posix_error(libc::ENOTCONN)) };
            unsafe { FSFileSystemBase::setContainerStatus(file_system, &status) };
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(didFinishLoading))]
        fn didFinishLoading(&self) {
            root_peer_state();
            log_event("info", "fskit-poc didFinishLoading");
        }
    }
);

impl RustUnaryFileSystem {
    pub fn new() -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc().set_ivars(())), init] }
    }
}

#[derive(Clone)]
struct RootPeerState {
    app_group_root: PathBuf,
    socket_path: PathBuf,
    inner: Arc<Mutex<RootPeerSnapshot>>,
}

struct RootPeerSnapshot {
    client: Option<VfsClient>,
    connected: bool,
    ever_connected: bool,
    generation: u64,
    disconnect_reason: Option<String>,
}

impl RootPeerState {
    fn new() -> Self {
        let app_group_root = real_home_dir()
            .join("Library")
            .join("Group Containers")
            .join(APP_GROUP_ID);
        let socket_path = app_group_root.join(SOCKET_NAME);
        let _ = std::fs::create_dir_all(&app_group_root);
        let state = Self {
            app_group_root,
            socket_path,
            inner: Arc::new(Mutex::new(RootPeerSnapshot {
                client: None,
                connected: false,
                ever_connected: false,
                generation: 0,
                disconnect_reason: Some("not connected".to_owned()),
            })),
        };
        state.start_listener();
        state
    }

    fn start_listener(&self) {
        let state = self.clone();
        runtime().spawn(async move {
            state.run_listener().await;
        });
    }

    async fn run_listener(self) {
        let socket_path = self.socket_path_string();
        let _ = std::fs::create_dir_all(&self.app_group_root);
        let _ = vox::transport::local::remove_endpoint(&socket_path);
        log_event(
            "info",
            format!("starting VFS root listener addr=local://{socket_path}"),
        );
        let listener = match vox::transport::local::LocalLinkAcceptor::bind(&socket_path) {
            Ok(listener) => listener,
            Err(error) => {
                self.record_disconnect(format!("listener_bind_error:{error}"));
                log_event(
                    "error",
                    format!("failed to bind VFS root listener: {error}"),
                );
                return;
            }
        };

        loop {
            log_event("debug", "waiting for VFS root peer");
            match listener.accept().await {
                Ok(link) => {
                    let state = self.clone();
                    tokio::spawn(async move {
                        state.accept_link(link).await;
                    });
                }
                Err(error) => {
                    self.record_disconnect(format!("listener_accept_error:{error}"));
                    log_event("error", format!("VFS root listener accept failed: {error}"));
                    return;
                }
            }
        }
    }

    async fn accept_link(self, link: vox::transport::local::LocalLink) {
        match acceptor_on(link)
            .on_connection(RootPeerAcceptor {
                state: self.clone(),
            })
            .establish::<vox::NoopClient>()
            .await
        {
            Ok(client) => {
                log_event("info", "VFS root peer session established");
                client.caller.closed().await;
            }
            Err(error) => {
                self.record_disconnect(format!("accept_error:{error}"));
                log_event("error", format!("failed to accept VFS root peer: {error}"));
            }
        }
    }

    fn record_connected(&self, client: VfsClient) -> u64 {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot.generation += 1;
        snapshot.client = Some(client);
        snapshot.connected = true;
        snapshot.ever_connected = true;
        snapshot.disconnect_reason = None;
        snapshot.generation
    }

    fn record_closed(&self, generation: u64, reason: &str) {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        if snapshot.generation != generation {
            return;
        }
        snapshot.client = None;
        snapshot.connected = false;
        snapshot.disconnect_reason = Some(reason.to_owned());
    }

    fn record_disconnect(&self, reason: String) {
        let mut snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        snapshot.client = None;
        snapshot.connected = false;
        snapshot.disconnect_reason = Some(reason);
    }

    fn connected_client(&self) -> Result<(VfsClient, u64), i32> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        match snapshot.client.clone() {
            Some(client) => Ok((client, snapshot.generation)),
            None => Err(libc::ENOTCONN),
        }
    }

    fn generation(&self) -> u64 {
        self.inner
            .lock()
            .expect("root peer snapshot mutex poisoned")
            .generation
    }

    fn socket_path_string(&self) -> String {
        self.socket_path.display().to_string()
    }

    fn status_json_bytes(&self) -> Vec<u8> {
        let snapshot = self
            .inner
            .lock()
            .expect("root peer snapshot mutex poisoned");
        let app_group_path = json_string(self.app_group_root.display().to_string());
        let disconnect_reason = snapshot
            .disconnect_reason
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned());
        format!(
            concat!(
                "{{\n",
                "  \"connected\": {},\n",
                "  \"ever_connected\": {},\n",
                "  \"app_group_path\": {},\n",
                "  \"connected_at\": null,\n",
                "  \"disconnected_at\": null,\n",
                "  \"uptime_seconds\": null,\n",
                "  \"disconnect_reason\": {}\n",
                "}}\n"
            ),
            snapshot.connected, snapshot.ever_connected, app_group_path, disconnect_reason
        )
        .into_bytes()
    }
}

#[derive(Clone)]
struct RootPeerAcceptor {
    state: RootPeerState,
}

impl ConnectionAcceptor for RootPeerAcceptor {
    fn accept(
        &self,
        request: &ConnectionRequest,
        connection: PendingConnection,
    ) -> Result<(), Metadata<'static>> {
        log_event(
            "info",
            format!("root listener request.service={}", request.service()),
        );
        let client = connection.handle_with_client::<VfsClient>(());
        let client: VfsClient = client.with_middleware(ClientLogging::new(ClientLoggingOptions {
            log_metadata: true,
        }));
        let generation = self.state.record_connected(client.clone());
        log_event(
            "info",
            format!("accepted VFS root peer generation={generation}"),
        );
        let state = self.state.clone();
        tokio::spawn(async move {
            client.caller.closed().await;
            state.record_closed(generation, "caller closed");
        });
        Ok(())
    }
}

pub fn create_file_system() -> Retained<FSUnaryFileSystem> {
    root_peer_state();
    RustUnaryFileSystem::new().into_super()
}

pub fn run_fskit_smoke() {
    root_peer_state();
    log_event("info", "fskit-poc smoke begin");

    let fs = RustUnaryFileSystem::new();
    unsafe { FSUnaryFileSystemOperations::didFinishLoading(&*fs) };

    let volume = RustVolume::new(fs.as_super());
    let root = volume.root_item();
    let lookup_name = fs_filename(VIXENFS_DIR_NAME);
    let looked_up_item = std::rc::Rc::new(std::cell::RefCell::new(None::<Retained<FSItem>>));
    let looked_up_name = std::rc::Rc::new(std::cell::RefCell::new(None::<Retained<FSFileName>>));
    let lookup_error = std::rc::Rc::new(std::cell::RefCell::new(None::<Retained<NSError>>));

    let item_slot = looked_up_item.clone();
    let name_slot = looked_up_name.clone();
    let error_slot = lookup_error.clone();
    let reply = RcBlock::new(
        move |item: *mut FSItem, name: *mut FSFileName, error: *mut NSError| {
            if let Some(item) = unsafe { Retained::retain(item) } {
                *item_slot.borrow_mut() = Some(item);
            }
            if let Some(name) = unsafe { Retained::retain(name) } {
                *name_slot.borrow_mut() = Some(name);
            }
            if let Some(error) = unsafe { Retained::retain(error) } {
                *error_slot.borrow_mut() = Some(error);
            }
        },
    );

    unsafe {
        FSVolumeOperations::lookupItemNamed_inDirectory_replyHandler(
            &*volume,
            &lookup_name,
            &root,
            &reply,
        );
    }

    let found_name = looked_up_name
        .borrow()
        .as_ref()
        .map(|name| fs_filename_string(name))
        .unwrap_or_default();
    let failed = lookup_error.borrow().is_some();
    log_event(
        "info",
        format!(
            "fskit-poc smoke lookup failed={failed} found_name={found_name} status_len={}",
            root_peer_state().status_json_bytes().len()
        ),
    );
}

fn root_peer_state() -> &'static RootPeerState {
    ROOT_PEER_STATE.get_or_init(RootPeerState::new)
}

fn runtime() -> &'static Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .thread_name("xpc-poc-vox")
            .build()
            .expect("failed to create xpc-poc tokio runtime")
    })
}

fn block_on_vfs<F, T>(future: F) -> Result<T, Retained<NSError>>
where
    F: std::future::Future<Output = Result<T, i32>>,
{
    runtime()
        .handle()
        .block_on(async move { future.await.map_err(posix_error) })
}

fn fs_filename(name: &str) -> Retained<FSFileName> {
    unsafe { FSFileName::initWithString(FSFileName::alloc(), &NSString::from_str(name)) }
}

fn fs_filename_string(name: &FSFileName) -> String {
    if let Some(string) = unsafe { name.string() } {
        return string.to_string();
    }
    let data = unsafe { name.data() };
    String::from_utf8_lossy(&data.to_vec()).into_owned()
}

fn posix_error(code: i32) -> Retained<NSError> {
    NSError::new(code as isize, unsafe { NSPOSIXErrorDomain })
}

fn as_rust_item(item: &FSItem) -> Option<&RustItem> {
    let object: &AnyObject = item.as_ref();
    object.downcast_ref::<RustItem>()
}

fn fs_item_type(item_type: VfsItemType) -> FSItemType {
    match item_type {
        VfsItemType::File => FSItemType::File,
        VfsItemType::Directory => FSItemType::Directory,
        VfsItemType::Symlink => FSItemType::Symlink,
    }
}

fn fs_item_attributes(
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

fn attr_valid_mask(attrs: &FSItemAttributes) -> isize {
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

fn objc_timespec(seconds: u64) -> ObjcTimespec {
    ObjcTimespec {
        tv_sec: seconds.min(i64::MAX as u64) as i64,
        tv_nsec: 0,
    }
}

fn vox_rpc_errno(context: &str, error: impl Display) -> i32 {
    log_event("error", format!("vox rpc {context} failed: {error}"));
    libc::EIO
}

fn entry_attributes(entry: &DirEntry) -> VfsItemAttributes {
    VfsItemAttributes {
        item_id: entry.item_id,
        item_type: entry.item_type,
        size: entry.size,
        modified_time: entry.modified_time,
        created_time: entry.created_time,
        mode: entry.mode,
    }
}

fn json_string(value: impl AsRef<str>) -> String {
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

fn unix_time() -> u64 {
    let mut now = 0;
    unsafe {
        libc::time(&mut now);
    }
    now as u64
}
