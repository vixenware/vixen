#![allow(non_snake_case)]

use std::ptr;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSData, NSError, NSObjectProtocol, NSPOSIXErrorDomain, NSString, NSUUID};
use objc2_fs_kit::{
    FSContainerStatus, FSDirectoryCookie, FSDirectoryCookieInitial, FSDirectoryEntryPacker,
    FSDirectoryVerifier, FSFileName, FSFileSystemBase, FSItem, FSItemAttributes,
    FSItemGetAttributesRequest, FSItemID, FSItemType, FSMutableFileDataBuffer, FSProbeResult,
    FSResource, FSStatFSResult, FSTaskOptions, FSUnaryFileSystem, FSUnaryFileSystemOperations,
    FSVolume, FSVolumeIdentifier, FSVolumeOperations, FSVolumePathConfOperations,
    FSVolumeReadWriteOperations, FSVolumeSupportedCapabilities,
};

use crate::log_event;

const ROOT_ITEM_ID: u64 = FSItemID::RootDirectory.0;
const HOUR_ITEM_ID: u64 = ROOT_ITEM_ID + 1;
const VOLUME_NAME: &str = "Rust FSKit PoC";
const HOUR_FILE_NAME: &str = "hour.txt";
const HOUR_FILE_BYTES: &[u8] = b"hour\n";
const DIRECTORY_VERIFIER: FSDirectoryVerifier = 1;

struct ItemIvars {
    item_id: u64,
    parent_id: u64,
    item_type: FSItemType,
    name: Retained<FSFileName>,
    contents: Vec<u8>,
}

define_class!(
    #[unsafe(super = FSItem)]
    #[ivars = ItemIvars]
    struct RustItem;

    unsafe impl NSObjectProtocol for RustItem {}
);

impl RustItem {
    fn new(
        item_id: u64,
        parent_id: u64,
        item_type: FSItemType,
        name: &str,
        contents: &[u8],
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ItemIvars {
            item_id,
            parent_id,
            item_type,
            name: fs_filename(name),
            contents: contents.to_vec(),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn fs_name(&self) -> Retained<FSFileName> {
        self.ivars().name.clone()
    }

    fn item_id(&self) -> u64 {
        self.ivars().item_id
    }

    fn contents(&self) -> &[u8] {
        &self.ivars().contents
    }

    fn attributes(&self) -> Retained<FSItemAttributes> {
        let attrs = unsafe { FSItemAttributes::new() };
        unsafe {
            attrs.setType(self.ivars().item_type);
            attrs.setMode(match self.ivars().item_type {
                FSItemType::Directory => 0o755,
                _ => 0o644,
            });
            attrs.setFileID(FSItemID(self.ivars().item_id));
            attrs.setParentID(FSItemID(self.ivars().parent_id));
            attrs.setLinkCount(1);
            attrs.setSize(self.ivars().contents.len() as u64);
            attrs.setAllocSize(self.ivars().contents.len() as u64);
            attrs.setUid(0);
            attrs.setGid(0);
        }
        attrs
    }
}

struct VolumeIvars {
    file_system: Retained<FSUnaryFileSystem>,
    root: Retained<RustItem>,
    hour_file: Retained<RustItem>,
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
            false
        }

        #[unsafe(method(truncatesLongNames))]
        fn truncatesLongNames(&self) -> bool {
            false
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
                    &NSString::from_str("vixen-poc"),
                )
            };
            unsafe {
                stats.setBlockSize(4096);
                stats.setIoSize(4096);
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
            let root: Retained<FSItem> = self.ivars().root.clone().into_super();
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
            if let Some(item) = as_rust_item(item) {
                let attrs = item.attributes();
                log_event(
                    "info",
                    format!("fskit-poc getAttributes item={}", item.item_id()),
                );
                reply.call((Retained::into_raw(attrs), ptr::null_mut()));
            } else {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
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

            let requested = fs_filename_bytes(name);
            let root = self.ivars().root.item_id();
            if directory.item_id() == root && requested == b"." {
                let item: Retained<FSItem> = self.ivars().root.clone().into_super();
                let fs_name = fs_filename(".");
                log_event("info", "fskit-poc lookupItemNamed .");
                reply.call((
                    Retained::into_raw(item),
                    Retained::into_raw(fs_name),
                    ptr::null_mut(),
                ));
            } else if directory.item_id() == root && requested == b".." {
                let item: Retained<FSItem> = self.ivars().root.clone().into_super();
                let fs_name = fs_filename("..");
                log_event("info", "fskit-poc lookupItemNamed ..");
                reply.call((
                    Retained::into_raw(item),
                    Retained::into_raw(fs_name),
                    ptr::null_mut(),
                ));
            } else if directory.item_id() == root && requested == HOUR_FILE_NAME.as_bytes() {
                log_event("info", "fskit-poc lookupItemNamed hour.txt");
                let item: Retained<FSItem> = self.ivars().hour_file.clone().into_super();
                let fs_name = self.ivars().hour_file.fs_name();
                reply.call((
                    Retained::into_raw(item),
                    Retained::into_raw(fs_name),
                    ptr::null_mut(),
                ));
            } else {
                let requested_name = String::from_utf8_lossy(&requested).into_owned();
                log_event(
                    "info",
                    format!(
                        "fskit-poc lookupItemNamed miss dir={} name={requested_name:?}",
                        directory.item_id()
                    ),
                );
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
            }
        }

        #[unsafe(method(enumerateDirectory:startingAtCookie:verifier:providingAttributes:usingPacker:replyHandler:))]
        fn enumerateDirectory_startingAtCookie_verifier_providingAttributes_usingPacker_replyHandler(
            &self,
            directory: &FSItem,
            cookie: FSDirectoryCookie,
            _verifier: FSDirectoryVerifier,
            attributes: Option<&FSItemGetAttributesRequest>,
            packer: &FSDirectoryEntryPacker,
            reply: &block2::DynBlock<dyn Fn(FSDirectoryVerifier, *mut NSError)>,
        ) {
            let Some(directory) = as_rust_item(directory) else {
                reply.call((0, Retained::into_raw(posix_error(libc::ENOENT))));
                return;
            };
            if directory.item_id() != self.ivars().root.item_id() {
                reply.call((0, Retained::into_raw(posix_error(libc::ENOTDIR))));
                return;
            }

            let include_dots = attributes.is_none();
            let mut entries = Vec::with_capacity(if include_dots { 3 } else { 1 });
            if include_dots {
                entries.push((
                    fs_filename("."),
                    FSItemType::Directory,
                    FSItemID(ROOT_ITEM_ID),
                    Some(self.ivars().root.attributes()),
                ));
                entries.push((
                    fs_filename(".."),
                    FSItemType::Directory,
                    FSItemID(ROOT_ITEM_ID),
                    Some(self.ivars().root.attributes()),
                ));
            }
            entries.push((
                self.ivars().hour_file.fs_name(),
                FSItemType::File,
                FSItemID(HOUR_ITEM_ID),
                Some(self.ivars().hour_file.attributes()),
            ));

            let start = if cookie == unsafe { FSDirectoryCookieInitial } {
                0
            } else {
                cookie.saturating_sub(1) as usize
            };
            if start > entries.len() {
                reply.call((0, Retained::into_raw(posix_error(libc::EINVAL))));
                return;
            }

            log_event(
                "info",
                format!(
                    "fskit-poc enumerateDirectory cookie={cookie} include_dots={include_dots} start={start}"
                ),
            );

            for (index, (name, item_type, item_id, entry_attributes)) in
                entries.iter().enumerate().skip(start)
            {
                let next_cookie = if index + 1 < entries.len() {
                    (index + 2) as FSDirectoryCookie
                } else {
                    0
                };
                let packed = unsafe {
                    packer.packEntryWithName_itemType_itemID_nextCookie_attributes(
                        name,
                        *item_type,
                        *item_id,
                        next_cookie,
                        entry_attributes.as_deref(),
                    )
                };
                if !packed {
                    break;
                }
            }

            reply.call((DIRECTORY_VERIFIER, ptr::null_mut()));
        }

        #[unsafe(method(reclaimItem:replyHandler:))]
        fn reclaimItem_replyHandler(
            &self,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let item_id = as_rust_item(item)
                .map(RustItem::item_id)
                .unwrap_or_default();
            log_event("info", format!("fskit-poc reclaimItem item={item_id}"));
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

            let offset = offset.max(0) as usize;
            let available = item.contents();
            if offset >= available.len() {
                reply.call((0, ptr::null_mut()));
                return;
            }

            let readable = available.len() - offset;
            let buffer_len = unsafe { buffer.length() };
            let actual = readable.min(length).min(buffer_len);
            let src = &available[offset..offset + actual];
            unsafe {
                ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    buffer.mutableBytes().as_ptr().cast::<u8>(),
                    actual,
                );
            }
            log_event(
                "info",
                format!(
                    "fskit-poc read item={} offset={offset} actual={actual}",
                    item.item_id()
                ),
            );
            reply.call((actual, ptr::null_mut()));
        }

        #[unsafe(method(writeContents:toFile:atOffset:replyHandler:))]
        fn writeContents_toFile_atOffset_replyHandler(
            &self,
            _contents: &NSData,
            _item: &FSItem,
            _offset: libc::off_t,
            reply: &block2::DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            log_event("info", "fskit-poc writeContents unsupported");
            reply.call((0, Retained::into_raw(posix_error(libc::EROFS))));
        }
    }
);

impl RustVolume {
    fn new(file_system: &FSUnaryFileSystem) -> Retained<Self> {
        let root = RustItem::new(
            ROOT_ITEM_ID,
            FSItemID::ParentOfRoot.0,
            FSItemType::Directory,
            "/",
            &[],
        );
        let hour_file = RustItem::new(
            HOUR_ITEM_ID,
            ROOT_ITEM_ID,
            FSItemType::File,
            HOUR_FILE_NAME,
            HOUR_FILE_BYTES,
        );
        let uuid = NSUUID::new();
        let volume_id =
            unsafe { FSVolumeIdentifier::initWithUUID(FSVolumeIdentifier::alloc(), &uuid) };
        let volume_name = fs_filename(VOLUME_NAME);
        let this = Self::alloc().set_ivars(VolumeIvars {
            file_system: unsafe {
                Retained::retain(file_system as *const FSUnaryFileSystem as *mut FSUnaryFileSystem)
                    .unwrap()
            },
            root,
            hour_file,
        });
        unsafe { msg_send![super(this), initWithVolumeID: &*volume_id, volumeName: &*volume_name] }
    }

    pub fn root_item(&self) -> Retained<FSItem> {
        self.ivars().root.clone().into_super()
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
            log_event("info", "fskit-poc didFinishLoading");
        }
    }
);

impl RustUnaryFileSystem {
    pub fn new() -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc().set_ivars(())), init] }
    }
}

pub fn create_file_system() -> Retained<FSUnaryFileSystem> {
    RustUnaryFileSystem::new().into_super()
}

fn fs_filename(name: &str) -> Retained<FSFileName> {
    unsafe { FSFileName::initWithString(FSFileName::alloc(), &NSString::from_str(name)) }
}

fn fs_filename_bytes(name: &FSFileName) -> Vec<u8> {
    unsafe { name.data() }.to_vec()
}

fn posix_error(code: i32) -> Retained<NSError> {
    NSError::new(code as isize, unsafe { NSPOSIXErrorDomain })
}

fn as_rust_item(item: &FSItem) -> Option<&RustItem> {
    let object: &AnyObject = item.as_ref();
    object.downcast_ref::<RustItem>()
}

pub fn run_fskit_smoke() {
    log_event("info", "fskit-poc smoke begin");

    let fs = RustUnaryFileSystem::new();
    unsafe { FSUnaryFileSystemOperations::didFinishLoading(&*fs) };

    let volume = RustVolume::new(fs.as_super());
    let root = volume.root_item();
    let hour = volume.ivars().hour_file.clone();
    let attrs = hour.attributes();

    log_event(
        "info",
        format!(
            "fskit-poc smoke volume={} root={} hour={} size={}",
            VOLUME_NAME,
            as_rust_item(&root)
                .map(RustItem::item_id)
                .unwrap_or_default(),
            hour.item_id(),
            unsafe { attrs.size() }
        ),
    );

    let lookup_name = fs_filename(HOUR_FILE_NAME);
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

    let looked_up_item_ref = looked_up_item.borrow();
    let found = looked_up_item_ref
        .as_ref()
        .and_then(|item| as_rust_item(item))
        .map(RustItem::item_id)
        .unwrap_or_default();
    let found_name = looked_up_name
        .borrow()
        .as_ref()
        .map(|name| String::from_utf8_lossy(&fs_filename_bytes(name)).into_owned())
        .unwrap_or_default();
    let failed = lookup_error.borrow().is_some();

    log_event(
        "info",
        format!(
            "fskit-poc smoke lookup failed={failed} found_item={} found_name={found_name}",
            found
        ),
    );
}
