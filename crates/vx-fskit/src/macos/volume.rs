#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ptr;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSData, NSError, NSObjectProtocol, NSString, NSUUID};
use objc2_fs_kit::{
    FSAccessMask, FSContainerStatus, FSDeactivateOptions, FSDirectoryCookie,
    FSDirectoryCookieInitial, FSDirectoryEntryPacker, FSDirectoryVerifier, FSFileName,
    FSFileSystemBase, FSItem, FSItemAttribute, FSItemAttributes, FSItemGetAttributesRequest,
    FSItemID, FSItemType, FSMutableFileDataBuffer, FSSetXattrPolicy, FSStatFSResult, FSSyncFlags,
    FSTaskOptions, FSUnaryFileSystem, FSVolume, FSVolumeAccessCheckOperations, FSVolumeIdentifier,
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

mod helpers;

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

    unsafe impl FSVolumeOperations for RustVolume {
        #[unsafe(method_id(supportedVolumeCapabilities))]
        unsafe fn supportedVolumeCapabilities(&self) -> Retained<FSVolumeSupportedCapabilities> {
            unsafe { FSVolumeSupportedCapabilities::new() }
        }

        #[unsafe(method_id(volumeStatistics))]
        unsafe fn volumeStatistics(&self) -> Retained<FSStatFSResult> {
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
            stats
        }

        #[unsafe(method(mountWithOptions:replyHandler:))]
        unsafe fn mountWithOptions_replyHandler(
            &self,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            log_event("info", "vixen-fskit mountWithOptions");
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(unmountWithReplyHandler:))]
        unsafe fn unmountWithReplyHandler(&self, reply: &block2::DynBlock<dyn Fn()>) {
            log_event("info", "vixen-fskit unmount");
            let ready = unsafe { FSContainerStatus::ready() };
            unsafe { FSFileSystemBase::setContainerStatus(&*self.ivars().file_system, &ready) };
            reply.call(());
        }

        #[unsafe(method(synchronizeWithFlags:replyHandler:))]
        unsafe fn synchronizeWithFlags_replyHandler(
            &self,
            _flags: FSSyncFlags,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            log_event("info", "vixen-fskit synchronize");
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(getAttributes:ofItem:replyHandler:))]
        unsafe fn getAttributes_ofItem_replyHandler(
            &self,
            desired_attributes: &FSItemGetAttributesRequest,
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
                    "vixen-fskit getAttributes wanted_mask={:#x} {}",
                    unsafe { desired_attributes.wantedAttributes().0 },
                    item.debug_summary()
                ),
            );

            match self.attributes_for_item(item, Some(desired_attributes)) {
                Ok(attrs) => {
                    log_event(
                        "debug",
                        format!(
                            "vixen-fskit getAttributes.reply valid_mask={:#x} file_id={} parent={} type={:?} mode={:#o} size={}",
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
        unsafe fn setAttributes_onItem_replyHandler(
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

            if item.virtual_kind() != VirtualKind::None {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::EACCES)),
                ));
                return;
            }

            let params = SetAttributesParams {
                mode: unsafe {
                    new_attributes
                        .isValid(FSItemAttribute::Mode)
                        .then(|| new_attributes.mode())
                },
                modified_time: None,
                size: unsafe {
                    new_attributes
                        .isValid(FSItemAttribute::Size)
                        .then(|| new_attributes.size())
                },
            };

            let backend_item_id = item.backend_item_id();
            let (client, _) = match self.connected_client_for_item(item) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.call((ptr::null_mut(), Retained::into_raw(error)));
                    return;
                }
            };
            let result = block_on_vfs(async move {
                let result = client
                    .set_attributes(backend_item_id, params)
                    .await
                    .map_err(|error| vox_rpc_errno("set_attributes", error))?;
                Ok::<_, i32>(result)
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
        unsafe fn lookupItemNamed_inDirectory_replyHandler(
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
                    "vixen-fskit lookupItem name={} dir={}",
                    requested_name,
                    directory.debug_summary()
                ),
            );

            match self.lookup_item(directory, name) {
                Ok((item, item_name)) => {
                    if let Some(item) = as_rust_item(item.as_ref()) {
                        log_event(
                            "info",
                            format!("vixen-fskit lookupItem.reply {}", item.debug_summary()),
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

        #[unsafe(method(reclaimItem:replyHandler:))]
        unsafe fn reclaimItem_replyHandler(
            &self,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            if let Some(item) = as_rust_item(item) {
                log_event(
                    "info",
                    format!("vixen-fskit reclaimItem item={}", item.item_id()),
                );
                self.remove_cached_item(item.item_id());
            }
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(readSymbolicLink:replyHandler:))]
        unsafe fn readSymbolicLink_replyHandler(
            &self,
            _item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                Retained::into_raw(posix_error(libc::ENOTSUP)),
            ));
        }

        #[unsafe(method(createItemNamed:type:inDirectory:attributes:replyHandler:))]
        unsafe fn createItemNamed_type_inDirectory_attributes_replyHandler(
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

            if !self.directory_allows_mutation(directory) {
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::EACCES)),
                ));
                return;
            }

            let requested_name = fs_filename_string(name);
            let mode = if unsafe { new_attributes.isValid(FSItemAttribute::Mode) } {
                unsafe { new_attributes.mode() }
            } else if item_type == FSItemType::Directory {
                0o755
            } else {
                0o644
            };

            let create_type = match item_type {
                FSItemType::Directory => vx_services::ItemType::Directory,
                FSItemType::File => vx_services::ItemType::File,
                FSItemType::Symlink => {
                    reply.call((
                        ptr::null_mut(),
                        ptr::null_mut(),
                        Retained::into_raw(posix_error(libc::ENOTSUP)),
                    ));
                    return;
                }
                _ => {
                    reply.call((
                        ptr::null_mut(),
                        ptr::null_mut(),
                        Retained::into_raw(posix_error(libc::ENOTSUP)),
                    ));
                    return;
                }
            };

            let parent_backend_id = directory.backend_item_id();
            let (client, generation) = match self.connected_client_for_item(directory) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.call((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error)));
                    return;
                }
            };
            let result = block_on_vfs({
                let requested_name = requested_name.clone();
                let client = client.clone();
                async move {
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
                        let attrs = client
                            .get_attributes(item_id)
                            .await
                            .map_err(|error| vox_rpc_errno("get_attributes", error))?;
                        Ok::<_, i32>(attrs)
                    });
                    match attrs_result {
                        Ok(GetAttributesResult { attrs, error: 0 }) => {
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
                        Ok(GetAttributesResult { error, .. }) => {
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

        #[unsafe(method(createSymbolicLinkNamed:inDirectory:attributes:linkContents:replyHandler:))]
        unsafe fn createSymbolicLinkNamed_inDirectory_attributes_linkContents_replyHandler(
            &self,
            _name: &FSFileName,
            _directory: &FSItem,
            _new_attributes: &objc2_fs_kit::FSItemSetAttributesRequest,
            _contents: &FSFileName,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                ptr::null_mut(),
                Retained::into_raw(posix_error(libc::ENOTSUP)),
            ));
        }

        #[unsafe(method(createLinkToItem:named:inDirectory:replyHandler:))]
        unsafe fn createLinkToItem_named_inDirectory_replyHandler(
            &self,
            _item: &FSItem,
            _name: &FSFileName,
            _directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                Retained::into_raw(posix_error(libc::ENOTSUP)),
            ));
        }

        #[unsafe(method(removeItem:named:fromDirectory:replyHandler:))]
        unsafe fn removeItem_named_fromDirectory_replyHandler(
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

            if item.virtual_kind() != VirtualKind::None {
                reply.call((Retained::into_raw(posix_error(libc::EACCES)),));
                return;
            }

            let backend_item_id = item.backend_item_id();
            let (client, _) = match self.connected_client_for_item(item) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.call((Retained::into_raw(error),));
                    return;
                }
            };
            let result = block_on_vfs(async move {
                let result = client
                    .delete(backend_item_id)
                    .await
                    .map_err(|error| vox_rpc_errno("delete", error))?;
                Ok::<_, i32>(result)
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
        unsafe fn renameItem_inDirectory_named_toNewName_inDirectory_overItem_replyHandler(
            &self,
            item: &FSItem,
            source_directory: &FSItem,
            _source_name: &FSFileName,
            destination_name: &FSFileName,
            destination_directory: &FSItem,
            over_item: Option<&FSItem>,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            let Some(item) = as_rust_item(item) else {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::ENOENT)),
                ));
                return;
            };
            let Some(source_directory) = as_rust_item(source_directory) else {
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
                || !self.directory_allows_mutation(source_directory)
                || !self.directory_allows_mutation(destination_directory)
            {
                reply.call((
                    ptr::null_mut(),
                    Retained::into_raw(posix_error(libc::EACCES)),
                ));
                return;
            }

            let new_name = fs_filename_string(destination_name);
            let backend_item_id = item.backend_item_id();
            let new_parent_id = destination_directory.backend_item_id();
            let (client, generation) = match self.connected_client_for_item(item) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.call((ptr::null_mut(), Retained::into_raw(error)));
                    return;
                }
            };
            for directory in [source_directory, destination_directory] {
                if matches!(
                    directory.virtual_kind(),
                    VirtualKind::Root | VirtualKind::None
                ) && directory.generation() != 0
                    && directory.generation() != generation
                {
                    reply.call((
                        ptr::null_mut(),
                        Retained::into_raw(posix_error(libc::ESTALE)),
                    ));
                    return;
                }
            }

            let result = block_on_vfs({
                let new_name = new_name.clone();
                async move {
                    let result = client
                        .rename(backend_item_id, new_parent_id, new_name)
                        .await
                        .map_err(|error| vox_rpc_errno("rename", error))?;
                    Ok::<_, i32>(result)
                }
            });

            match result {
                Ok(VfsResult { error: 0 }) => {
                    item.update_name_and_parent(destination_directory.item_id(), &new_name);
                    if let Some(over_item) = over_item.and_then(as_rust_item) {
                        self.remove_cached_item(over_item.item_id());
                    }
                    reply.call((Retained::into_raw(fs_filename(&new_name)), ptr::null_mut()))
                }
                Ok(VfsResult { error }) => {
                    reply.call((ptr::null_mut(), Retained::into_raw(posix_error(error))))
                }
                Err(error) => reply.call((ptr::null_mut(), Retained::into_raw(error))),
            }
        }

        #[unsafe(method(enumerateDirectory:startingAtCookie:verifier:providingAttributes:usingPacker:replyHandler:))]
        unsafe fn enumerateDirectory_startingAtCookie_verifier_providingAttributes_usingPacker_replyHandler(
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
                    "vixen-fskit enumerateDirectory cookie={} verifier={} {}",
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

        #[unsafe(method(activateWithOptions:replyHandler:))]
        unsafe fn activateWithOptions_replyHandler(
            &self,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut NSError)>,
        ) {
            log_event("info", "vixen-fskit activateWithOptions");
            match self.get_cached_item(ROOT_LOCAL_ID) {
                Some(root) => {
                    let root_fs_item: Retained<FSItem> = unsafe { Retained::cast_unchecked(root) };
                    reply.call((Retained::into_raw(root_fs_item), ptr::null_mut()));
                }
                None => {
                    log_event(
                        "error",
                        "vixen-fskit activateWithOptions: root item not found",
                    );
                    reply.call((
                        ptr::null_mut(),
                        Retained::into_raw(posix_error(libc::ENOENT)),
                    ));
                }
            }
        }

        #[unsafe(method(deactivateWithOptions:replyHandler:))]
        unsafe fn deactivateWithOptions_replyHandler(
            &self,
            _options: FSDeactivateOptions,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            log_event("info", "vixen-fskit deactivateWithOptions");
            reply.call((ptr::null_mut(),));
        }
    }

    unsafe impl FSVolumePathConfOperations for RustVolume {
        #[unsafe(method(maximumLinkCount))]
        unsafe fn maximumLinkCount(&self) -> isize {
            1
        }

        #[unsafe(method(maximumNameLength))]
        unsafe fn maximumNameLength(&self) -> isize {
            255
        }

        #[unsafe(method(restrictsOwnershipChanges))]
        unsafe fn restrictsOwnershipChanges(&self) -> bool {
            true
        }

        #[unsafe(method(truncatesLongNames))]
        unsafe fn truncatesLongNames(&self) -> bool {
            false
        }

        #[unsafe(method(maximumXattrSize))]
        unsafe fn maximumXattrSize(&self) -> isize {
            64 * 1024
        }

        #[unsafe(method(maximumXattrSizeInBits))]
        unsafe fn maximumXattrSizeInBits(&self) -> isize {
            17
        }

        #[unsafe(method(maximumFileSize))]
        unsafe fn maximumFileSize(&self) -> u64 {
            i64::MAX as u64
        }

        #[unsafe(method(maximumFileSizeInBits))]
        unsafe fn maximumFileSizeInBits(&self) -> isize {
            64
        }
    }

    unsafe impl FSVolumeReadWriteOperations for RustVolume {
        #[unsafe(method(readFromFile:offset:length:intoBuffer:replyHandler:))]
        unsafe fn readFromFile_offset_length_intoBuffer_replyHandler(
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
                    "vixen-fskit readFromFile offset={} length={} {}",
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
        unsafe fn writeContents_toFile_atOffset_replyHandler(
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
            let backend_item_id = item.backend_item_id();
            let offset = offset.max(0) as u64;
            let (client, _) = match self.connected_client_for_item(item) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.call((0, Retained::into_raw(error)));
                    return;
                }
            };
            let result = block_on_vfs(async move {
                let result = client
                    .write(backend_item_id, offset, bytes)
                    .await
                    .map_err(|error| vox_rpc_errno("write", error))?;
                Ok::<_, i32>(result)
            });

            match result {
                Ok(WriteResult {
                    bytes_written,
                    error: 0,
                }) => reply.call((bytes_written as usize, ptr::null_mut())),
                Ok(WriteResult { error, .. }) => {
                    reply.call((0, Retained::into_raw(posix_error(error))));
                }
                Err(error) => reply.call((0, Retained::into_raw(error))),
            }
        }
    }

    unsafe impl FSVolumeXattrOperations for RustVolume {
        #[unsafe(method(xattrOperationsInhibited))]
        unsafe fn xattrOperationsInhibited(&self) -> bool {
            false
        }

        #[unsafe(method(getXattrNamed:ofItem:replyHandler:))]
        unsafe fn getXattrNamed_ofItem_replyHandler(
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
        unsafe fn setXattrNamed_toData_onItem_policy_replyHandler(
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
        unsafe fn listXattrsOfItem_replyHandler(
            &self,
            _item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSArray<FSFileName>, *mut NSError)>,
        ) {
            let empty = NSArray::<FSFileName>::from_slice(&[]);
            reply.call((Retained::into_raw(empty), ptr::null_mut()));
        }
    }

    unsafe impl FSVolumeOpenCloseOperations for RustVolume {
        #[unsafe(method(isOpenCloseInhibited))]
        unsafe fn isOpenCloseInhibited(&self) -> bool {
            true
        }

        #[unsafe(method(openItem:withModes:replyHandler:))]
        unsafe fn openItem_withModes_replyHandler(
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
                format!("vixen-fskit openItem modes={:#x} {}", modes.0, details),
            );
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(closeItem:keepingModes:replyHandler:))]
        unsafe fn closeItem_keepingModes_replyHandler(
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
                format!(
                    "vixen-fskit closeItem keep_modes={:#x} {}",
                    modes.0, details
                ),
            );
            reply.call((ptr::null_mut(),));
        }
    }

    unsafe impl FSVolumeAccessCheckOperations for RustVolume {
        #[unsafe(method(isAccessCheckInhibited))]
        unsafe fn isAccessCheckInhibited(&self) -> bool {
            true
        }

        #[unsafe(method(checkAccessToItem:requestedAccess:replyHandler:))]
        unsafe fn checkAccessToItem_requestedAccess_replyHandler(
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
                format!("vixen-fskit checkAccess access={:#x} {}", access.0, details),
            );
            reply.call((Bool::YES, ptr::null_mut()));
        }
    }
);
