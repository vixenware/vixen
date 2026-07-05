#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ptr;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSData, NSError, NSObjectProtocol, NSString, NSUUID};
use objc2_fs_kit::{
    FSContainerStatus, FSDeactivateOptions, FSDirectoryCookie, FSDirectoryCookieInitial,
    FSDirectoryEntryPacker, FSDirectoryVerifier, FSFileName, FSFileSystemBase, FSItem,
    FSItemAttribute, FSItemAttributes, FSItemGetAttributesRequest, FSItemID, FSItemType,
    FSMutableFileDataBuffer, FSSetXattrPolicy, FSStatFSResult, FSSyncFlags, FSTaskOptions,
    FSUnaryFileSystem, FSVolume, FSVolumeIdentifier, FSVolumeOpenCloseOperations,
    FSVolumeOpenModes, FSVolumeOperations, FSVolumePathConfOperations, FSVolumeReadWriteOperations,
    FSVolumeSupportedCapabilities, FSVolumeXattrOperations,
};
use vx_services::{
    CreateResult, GetAttributesResult, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, VfsResult, WriteResult,
};

use super::item::{RustItem, VirtualKind, as_rust_item};
use super::log_event;
use super::root_peer::root_peer_state;
use super::support::{
    ROOT_BACKEND_ID, ROOT_LOCAL_ID, ROUTED_ITEM_ID_START, ReplyGuard, RetainedObjc, STATUS_JSON_ID,
    STATUS_JSON_NAME, VIXENFS_DIR_ID, VIXENFS_DIR_NAME, VOLUME_NAME, VfsOp, attr_valid_mask,
    entry_attributes, fs_filename, fs_filename_string, lock_unpoisoned, posix_error, run_vfs,
    runtime, vox_rpc_errno,
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
            log_event("trace", "vixen-fskit supportedVolumeCapabilities");
            unsafe { FSVolumeSupportedCapabilities::new() }
        }

        #[unsafe(method_id(volumeStatistics))]
        unsafe fn volumeStatistics(&self) -> Retained<FSStatFSResult> {
            log_event("trace", "vixen-fskit volumeStatistics");
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
            root_peer_state().cancel_inflight(libc::ESTALE, "volume unmount");
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
            let reply = ReplyGuard::<dyn Fn(*mut FSItemAttributes, *mut NSError)>::copy(
                "getAttributes",
                reply,
            );
            let Some(item) = as_rust_item(item) else {
                reply.error(libc::ENOENT);
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

            if item.virtual_kind() == VirtualKind::None {
                let Some(volume) = RetainedObjc::retain(self) else {
                    reply.error(libc::EIO);
                    return;
                };
                let Some(item) = RetainedObjc::retain(item) else {
                    reply.error(libc::EIO);
                    return;
                };
                runtime().spawn_blocking(move || {
                    let volume = unsafe { volume.get() };
                    let item = unsafe { item.get() };
                    match volume.refresh_backend_item(item, None) {
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
                            reply.reply((Retained::into_raw(attrs), ptr::null_mut()));
                        }
                        Err(error) => reply.reply((ptr::null_mut(), Retained::into_raw(error))),
                    }
                });
                return;
            }

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
                    reply.reply((Retained::into_raw(attrs), ptr::null_mut()))
                }
                Err(error) => reply.reply((ptr::null_mut(), Retained::into_raw(error))),
            }
        }

        #[unsafe(method(setAttributes:onItem:replyHandler:))]
        unsafe fn setAttributes_onItem_replyHandler(
            &self,
            new_attributes: &objc2_fs_kit::FSItemSetAttributesRequest,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        ) {
            let reply = ReplyGuard::<dyn Fn(*mut FSItemAttributes, *mut NSError)>::copy(
                "setAttributes",
                reply,
            );
            let Some(item) = as_rust_item(item) else {
                reply.error(libc::ENOENT);
                return;
            };
            log_event(
                "info",
                format!(
                    "vixen-fskit setAttributes mode_valid={} size_valid={} {}",
                    unsafe { new_attributes.isValid(FSItemAttribute::Mode) },
                    unsafe { new_attributes.isValid(FSItemAttribute::Size) },
                    item.debug_summary()
                ),
            );

            if item.virtual_kind() != VirtualKind::None {
                reply.error(libc::EACCES);
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
                    reply.reply((ptr::null_mut(), Retained::into_raw(error)));
                    return;
                }
            };
            let Some(volume) = RetainedObjc::retain(self) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(item) = RetainedObjc::retain(item) else {
                reply.error(libc::EIO);
                return;
            };
            runtime().spawn_blocking(move || {
                let result = run_vfs(
                    VfsOp::Mutate,
                    root_peer_state().subscribe_cancellation(),
                    async move {
                        let result = client
                            .set_attributes(backend_item_id, params)
                            .await
                            .map_err(|error| vox_rpc_errno("set_attributes", error))?;
                        Ok::<_, i32>(result)
                    },
                );

                match result {
                    Ok(VfsResult { error: 0 }) => {
                        let volume = unsafe { volume.get() };
                        let item = unsafe { item.get() };
                        match volume.refresh_backend_item(item, None) {
                            Ok(attrs) => reply.reply((Retained::into_raw(attrs), ptr::null_mut())),
                            Err(error) => reply.reply((ptr::null_mut(), Retained::into_raw(error))),
                        }
                    }
                    Ok(VfsResult { error }) => {
                        reply.reply((ptr::null_mut(), Retained::into_raw(posix_error(error))))
                    }
                    Err(error) => {
                        reply.reply((ptr::null_mut(), Retained::into_raw(posix_error(error))))
                    }
                }
            });
        }

        #[unsafe(method(lookupItemNamed:inDirectory:replyHandler:))]
        unsafe fn lookupItemNamed_inDirectory_replyHandler(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            let reply = ReplyGuard::<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>::copy(
                "lookup", reply,
            );
            let Some(directory) = as_rust_item(directory) else {
                reply.error(libc::ENOENT);
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

            let Some(volume) = RetainedObjc::retain(self) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(directory) = RetainedObjc::retain(directory) else {
                reply.error(libc::EIO);
                return;
            };
            runtime().spawn_blocking(move || {
                let volume = unsafe { volume.get() };
                let directory = unsafe { directory.get() };
                match volume.lookup_item(directory, requested_name) {
                    Ok((item, item_name)) => {
                        if let Some(item) = as_rust_item(item.as_ref()) {
                            log_event(
                                "info",
                                format!("vixen-fskit lookupItem.reply {}", item.debug_summary()),
                            );
                        }
                        reply.reply((
                            Retained::into_raw(item),
                            Retained::into_raw(item_name),
                            ptr::null_mut(),
                        ))
                    }
                    Err(error) => {
                        reply.reply((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error)))
                    }
                }
            });
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
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            let details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!("vixen-fskit readSymbolicLink unsupported {details}"),
            );
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
            let reply = ReplyGuard::<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>::copy(
                "create", reply,
            );
            let Some(directory) = as_rust_item(directory) else {
                reply.error(libc::ENOENT);
                return;
            };
            let requested_name = fs_filename_string(name);
            log_event(
                "info",
                format!(
                    "vixen-fskit createItem name={} type={:?} dir={}",
                    requested_name,
                    item_type,
                    directory.debug_summary()
                ),
            );

            if !self.directory_allows_mutation(directory) {
                reply.error(libc::EACCES);
                return;
            }

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
                    reply.error(libc::ENOTSUP);
                    return;
                }
                _ => {
                    reply.error(libc::ENOTSUP);
                    return;
                }
            };

            let parent_backend_id = directory.backend_item_id();
            let (client, generation) = match self.connected_client_for_item(directory) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.reply((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error)));
                    return;
                }
            };
            let Some(volume) = RetainedObjc::retain(self) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(directory) = RetainedObjc::retain(directory) else {
                reply.error(libc::EIO);
                return;
            };
            runtime().spawn_blocking(move || {
                let create_result =
                    run_vfs(VfsOp::Mutate, root_peer_state().subscribe_cancellation(), {
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

                match create_result {
                    Ok(CreateResult { item_id, error: 0 }) => {
                        let attrs_result = run_vfs(
                            VfsOp::Attributes,
                            root_peer_state().subscribe_cancellation(),
                            async move {
                                let attrs = client
                                    .get_attributes(item_id)
                                    .await
                                    .map_err(|error| vox_rpc_errno("get_attributes", error))?;
                                Ok::<_, i32>(attrs)
                            },
                        );
                        match attrs_result {
                            Ok(GetAttributesResult { attrs, error: 0 }) => {
                                let volume = unsafe { volume.get() };
                                let directory = unsafe { directory.get() };
                                let local_item_id = volume.local_item_id(generation, attrs.item_id);
                                let item = volume.cached_backend_item(
                                    local_item_id,
                                    directory.item_id(),
                                    generation,
                                    &requested_name,
                                    &attrs,
                                );
                                reply.reply((
                                    Retained::into_raw(item.clone().into_super()),
                                    Retained::into_raw(item.fs_name()),
                                    ptr::null_mut(),
                                ));
                            }
                            Ok(GetAttributesResult { error, .. }) => {
                                reply.reply((
                                    ptr::null_mut(),
                                    ptr::null_mut(),
                                    Retained::into_raw(posix_error(error)),
                                ));
                            }
                            Err(error) => {
                                reply.reply((
                                    ptr::null_mut(),
                                    ptr::null_mut(),
                                    Retained::into_raw(posix_error(error)),
                                ));
                            }
                        }
                    }
                    Ok(CreateResult { error, .. }) => reply.reply((
                        ptr::null_mut(),
                        ptr::null_mut(),
                        Retained::into_raw(posix_error(error)),
                    )),
                    Err(error) => reply.reply((
                        ptr::null_mut(),
                        ptr::null_mut(),
                        Retained::into_raw(posix_error(error)),
                    )),
                }
            });
        }

        #[unsafe(method(createSymbolicLinkNamed:inDirectory:attributes:linkContents:replyHandler:))]
        unsafe fn createSymbolicLinkNamed_inDirectory_attributes_linkContents_replyHandler(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            _new_attributes: &objc2_fs_kit::FSItemSetAttributesRequest,
            contents: &FSFileName,
            reply: &block2::DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            let details = as_rust_item(directory)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!(
                    "vixen-fskit createSymbolicLink unsupported name={} contents={} dir={details}",
                    fs_filename_string(name),
                    fs_filename_string(contents)
                ),
            );
            reply.call((
                ptr::null_mut(),
                ptr::null_mut(),
                Retained::into_raw(posix_error(libc::ENOTSUP)),
            ));
        }

        #[unsafe(method(createLinkToItem:named:inDirectory:replyHandler:))]
        unsafe fn createLinkToItem_named_inDirectory_replyHandler(
            &self,
            item: &FSItem,
            name: &FSFileName,
            directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            let item_details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            let directory_details = as_rust_item(directory)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!(
                    "vixen-fskit createLink unsupported name={} item={} dir={}",
                    fs_filename_string(name),
                    item_details,
                    directory_details
                ),
            );
            reply.call((
                ptr::null_mut(),
                Retained::into_raw(posix_error(libc::ENOTSUP)),
            ));
        }

        #[unsafe(method(removeItem:named:fromDirectory:replyHandler:))]
        unsafe fn removeItem_named_fromDirectory_replyHandler(
            &self,
            item: &FSItem,
            name: &FSFileName,
            directory: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let reply = ReplyGuard::<dyn Fn(*mut NSError)>::copy("remove", reply);
            let Some(item) = as_rust_item(item) else {
                reply.error(libc::ENOENT);
                return;
            };
            let directory_details = as_rust_item(directory)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!(
                    "vixen-fskit removeItem name={} item={} dir={}",
                    fs_filename_string(name),
                    item.debug_summary(),
                    directory_details
                ),
            );

            if item.virtual_kind() != VirtualKind::None {
                reply.error(libc::EACCES);
                return;
            }

            let backend_item_id = item.backend_item_id();
            let (client, _) = match self.connected_client_for_item(item) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.reply((Retained::into_raw(error),));
                    return;
                }
            };
            let item_id = item.item_id();
            let Some(volume) = RetainedObjc::retain(self) else {
                reply.error(libc::EIO);
                return;
            };
            runtime().spawn_blocking(move || {
                let result = run_vfs(
                    VfsOp::Mutate,
                    root_peer_state().subscribe_cancellation(),
                    async move {
                        let result = client
                            .delete(backend_item_id)
                            .await
                            .map_err(|error| vox_rpc_errno("delete", error))?;
                        Ok::<_, i32>(result)
                    },
                );

                match result {
                    Ok(VfsResult { error: 0 }) => {
                        let volume = unsafe { volume.get() };
                        volume.remove_cached_item(item_id);
                        reply.reply((ptr::null_mut(),));
                    }
                    Ok(VfsResult { error }) => {
                        reply.reply((Retained::into_raw(posix_error(error)),))
                    }
                    Err(error) => reply.reply((Retained::into_raw(posix_error(error)),)),
                }
            });
        }

        #[unsafe(method(renameItem:inDirectory:named:toNewName:inDirectory:overItem:replyHandler:))]
        unsafe fn renameItem_inDirectory_named_toNewName_inDirectory_overItem_replyHandler(
            &self,
            item: &FSItem,
            source_directory: &FSItem,
            source_name: &FSFileName,
            destination_name: &FSFileName,
            destination_directory: &FSItem,
            over_item: Option<&FSItem>,
            reply: &block2::DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            let reply = ReplyGuard::<dyn Fn(*mut FSFileName, *mut NSError)>::copy("rename", reply);
            let Some(item) = as_rust_item(item) else {
                reply.error(libc::ENOENT);
                return;
            };
            let Some(source_directory) = as_rust_item(source_directory) else {
                reply.error(libc::ENOENT);
                return;
            };
            let Some(destination_directory) = as_rust_item(destination_directory) else {
                reply.error(libc::ENOENT);
                return;
            };
            log_event(
                "info",
                format!(
                    "vixen-fskit renameItem source_name={} destination_name={} item={} source_dir={} destination_dir={} over={}",
                    fs_filename_string(source_name),
                    fs_filename_string(destination_name),
                    item.debug_summary(),
                    source_directory.debug_summary(),
                    destination_directory.debug_summary(),
                    over_item
                        .and_then(as_rust_item)
                        .map(|item| item.debug_summary())
                        .unwrap_or_else(|| "none".to_owned())
                ),
            );

            if item.virtual_kind() != VirtualKind::None
                || !self.directory_allows_mutation(source_directory)
                || !self.directory_allows_mutation(destination_directory)
            {
                reply.error(libc::EACCES);
                return;
            }

            let new_name = fs_filename_string(destination_name);
            let backend_item_id = item.backend_item_id();
            let new_parent_id = destination_directory.backend_item_id();
            let (client, generation) = match self.connected_client_for_item(item) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.reply((ptr::null_mut(), Retained::into_raw(error)));
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
                    reply.error(libc::ESTALE);
                    return;
                }
            }

            let destination_local_id = destination_directory.item_id();
            let over_item_id = over_item.and_then(as_rust_item).map(RustItem::item_id);
            let Some(volume) = RetainedObjc::retain(self) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(item) = RetainedObjc::retain(item) else {
                reply.error(libc::EIO);
                return;
            };
            runtime().spawn_blocking(move || {
                let result = run_vfs(VfsOp::Mutate, root_peer_state().subscribe_cancellation(), {
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
                        let volume = unsafe { volume.get() };
                        let item = unsafe { item.get() };
                        item.update_name_and_parent(destination_local_id, &new_name);
                        if let Some(over_item_id) = over_item_id {
                            volume.remove_cached_item(over_item_id);
                        }
                        reply.reply((Retained::into_raw(fs_filename(&new_name)), ptr::null_mut()))
                    }
                    Ok(VfsResult { error }) => {
                        reply.reply((ptr::null_mut(), Retained::into_raw(posix_error(error))))
                    }
                    Err(error) => {
                        reply.reply((ptr::null_mut(), Retained::into_raw(posix_error(error))))
                    }
                }
            });
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
            let reply =
                ReplyGuard::<dyn Fn(FSDirectoryVerifier, *mut NSError)>::copy("enumerate", reply);
            let Some(directory) = as_rust_item(directory) else {
                reply.error(libc::ENOENT);
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

            let Some(volume) = RetainedObjc::retain(self) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(directory) = RetainedObjc::retain(directory) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(packer) = RetainedObjc::retain(packer) else {
                reply.error(libc::EIO);
                return;
            };
            let provide_attributes = attributes.is_some();
            runtime().spawn_blocking(move || {
                let volume = unsafe { volume.get() };
                let directory = unsafe { directory.get() };
                let packer = unsafe { packer.get() };
                match volume.enumerate_directory(
                    directory,
                    cookie,
                    verifier,
                    provide_attributes,
                    packer,
                ) {
                    Ok(next_verifier) => reply.reply((next_verifier, ptr::null_mut())),
                    Err(error) => reply.reply((0, Retained::into_raw(error))),
                }
            });
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
            root_peer_state().cancel_inflight(libc::ESTALE, "volume deactivate");
            reply.call((ptr::null_mut(),));
        }
    }

    unsafe impl FSVolumePathConfOperations for RustVolume {
        #[unsafe(method(maximumLinkCount))]
        unsafe fn maximumLinkCount(&self) -> isize {
            log_event("trace", "vixen-fskit maximumLinkCount");
            1
        }

        #[unsafe(method(maximumNameLength))]
        unsafe fn maximumNameLength(&self) -> isize {
            log_event("trace", "vixen-fskit maximumNameLength");
            255
        }

        #[unsafe(method(restrictsOwnershipChanges))]
        unsafe fn restrictsOwnershipChanges(&self) -> bool {
            log_event("trace", "vixen-fskit restrictsOwnershipChanges");
            true
        }

        #[unsafe(method(truncatesLongNames))]
        unsafe fn truncatesLongNames(&self) -> bool {
            log_event("trace", "vixen-fskit truncatesLongNames");
            false
        }

        #[unsafe(method(maximumXattrSize))]
        unsafe fn maximumXattrSize(&self) -> isize {
            log_event("trace", "vixen-fskit maximumXattrSize");
            64 * 1024
        }

        #[unsafe(method(maximumXattrSizeInBits))]
        unsafe fn maximumXattrSizeInBits(&self) -> isize {
            log_event("trace", "vixen-fskit maximumXattrSizeInBits");
            17
        }

        #[unsafe(method(maximumFileSize))]
        unsafe fn maximumFileSize(&self) -> u64 {
            log_event("trace", "vixen-fskit maximumFileSize");
            i64::MAX as u64
        }

        #[unsafe(method(maximumFileSizeInBits))]
        unsafe fn maximumFileSizeInBits(&self) -> isize {
            log_event("trace", "vixen-fskit maximumFileSizeInBits");
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
            let reply = ReplyGuard::<dyn Fn(usize, *mut NSError)>::copy("read", reply);
            let Some(item) = as_rust_item(item) else {
                reply.error(libc::ENOENT);
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

            let Some(volume) = RetainedObjc::retain(self) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(item) = RetainedObjc::retain(item) else {
                reply.error(libc::EIO);
                return;
            };
            let Some(buffer) = RetainedObjc::retain(buffer) else {
                reply.error(libc::EIO);
                return;
            };
            runtime().spawn_blocking(move || {
                let volume = unsafe { volume.get() };
                let item = unsafe { item.get() };
                let buffer = unsafe { buffer.get() };
                match volume.read_item(item, offset, length, buffer) {
                    Ok(actual) => reply.reply((actual, ptr::null_mut())),
                    Err(error) => reply.reply((0, Retained::into_raw(error))),
                }
            });
        }

        #[unsafe(method(writeContents:toFile:atOffset:replyHandler:))]
        unsafe fn writeContents_toFile_atOffset_replyHandler(
            &self,
            contents: &NSData,
            item: &FSItem,
            offset: libc::off_t,
            reply: &block2::DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            let reply = ReplyGuard::<dyn Fn(usize, *mut NSError)>::copy("write", reply);
            let Some(item) = as_rust_item(item) else {
                reply.error(libc::ENOENT);
                return;
            };
            log_event(
                "info",
                format!(
                    "vixen-fskit writeContents offset={} length={} {}",
                    offset,
                    contents.length(),
                    item.debug_summary()
                ),
            );

            if item.virtual_kind() != VirtualKind::None {
                reply.error(libc::EACCES);
                return;
            }

            let bytes = contents.to_vec();
            let backend_item_id = item.backend_item_id();
            let offset = offset.max(0) as u64;
            let (client, _) = match self.connected_client_for_item(item) {
                Ok(connected) => connected,
                Err(error) => {
                    reply.reply((0, Retained::into_raw(error)));
                    return;
                }
            };
            runtime().spawn_blocking(move || {
                let result = run_vfs(
                    VfsOp::Write,
                    root_peer_state().subscribe_cancellation(),
                    async move {
                        let result = client
                            .write(backend_item_id, offset, bytes)
                            .await
                            .map_err(|error| vox_rpc_errno("write", error))?;
                        Ok::<_, i32>(result)
                    },
                );

                match result {
                    Ok(WriteResult {
                        bytes_written,
                        error: 0,
                    }) => reply.reply((bytes_written as usize, ptr::null_mut())),
                    Ok(WriteResult { error, .. }) => {
                        reply.reply((0, Retained::into_raw(posix_error(error))));
                    }
                    Err(error) => reply.reply((0, Retained::into_raw(posix_error(error)))),
                }
            });
        }
    }

    unsafe impl FSVolumeXattrOperations for RustVolume {
        #[unsafe(method(xattrOperationsInhibited))]
        unsafe fn xattrOperationsInhibited(&self) -> bool {
            log_event("trace", "vixen-fskit xattrOperationsInhibited");
            false
        }

        #[unsafe(method(getXattrNamed:ofItem:replyHandler:))]
        unsafe fn getXattrNamed_ofItem_replyHandler(
            &self,
            name: &FSFileName,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSData, *mut NSError)>,
        ) {
            let details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!(
                    "vixen-fskit getXattr unsupported name={} item={details}",
                    fs_filename_string(name)
                ),
            );
            reply.call((
                ptr::null_mut(),
                Retained::into_raw(posix_error(libc::ENOATTR)),
            ));
        }

        #[unsafe(method(setXattrNamed:toData:onItem:policy:replyHandler:))]
        unsafe fn setXattrNamed_toData_onItem_policy_replyHandler(
            &self,
            name: &FSFileName,
            value: Option<&NSData>,
            item: &FSItem,
            policy: FSSetXattrPolicy,
            reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event(
                "info",
                format!(
                    "vixen-fskit setXattr unsupported name={} length={} policy={:?} item={details}",
                    fs_filename_string(name),
                    value.map(|value| value.length()).unwrap_or(0),
                    policy
                ),
            );
            reply.call((Retained::into_raw(posix_error(libc::ENOTSUP)),));
        }

        #[unsafe(method(listXattrsOfItem:replyHandler:))]
        unsafe fn listXattrsOfItem_replyHandler(
            &self,
            item: &FSItem,
            reply: &block2::DynBlock<dyn Fn(*mut NSArray<FSFileName>, *mut NSError)>,
        ) {
            let details = as_rust_item(item)
                .map(|item| item.debug_summary())
                .unwrap_or_else(|| "<non-rust-item>".to_owned());
            log_event("info", format!("vixen-fskit listXattrs item={details}"));
            let empty = NSArray::<FSFileName>::from_slice(&[]);
            reply.call((Retained::into_raw(empty), ptr::null_mut()));
        }
    }

    unsafe impl FSVolumeOpenCloseOperations for RustVolume {
        #[unsafe(method(isOpenCloseInhibited))]
        unsafe fn isOpenCloseInhibited(&self) -> bool {
            log_event("trace", "vixen-fskit isOpenCloseInhibited");
            // We want the open/close lifecycle: it's the consistency boundary used to
            // freeze a stable snapshot of the synthetic status.json across getAttributes
            // and read. For every other item open/close are harmless no-ops.
            false
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
            // Freeze a consistent status.json snapshot for the lifetime of this open, so
            // getAttributes (size) and read (bytes) can't disagree mid-flight.
            if let Some(rust_item) = as_rust_item(item)
                && rust_item.virtual_kind() == VirtualKind::StatusJson
            {
                rust_item.status_open(root_peer_state().status_json_bytes());
            }
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
            if let Some(rust_item) = as_rust_item(item)
                && rust_item.virtual_kind() == VirtualKind::StatusJson
            {
                rust_item.status_close();
            }
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
);
