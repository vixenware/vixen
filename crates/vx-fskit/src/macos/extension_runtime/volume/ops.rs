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
    log_event("info", "vixen-fskit activateWithOptions");
    let active = unsafe { FSContainerStatus::active() };
    unsafe { FSFileSystemBase::setContainerStatus(&*self.ivars().file_system, &active) };
    let root = self.root_item();
    if let Some(root_item) = as_rust_item(root.as_ref()) {
        log_event(
            "info",
            format!(
                "vixen-fskit activateWithOptions root {}",
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
    log_event("info", "vixen-fskit mountWithOptions");
    reply.call((ptr::null_mut(),));
}

#[unsafe(method(deactivateWithOptions:replyHandler:))]
fn deactivateWithOptions_replyHandler(
    &self,
    _options: FSDeactivateOptions,
    reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
) {
    log_event("info", "vixen-fskit deactivateWithOptions");
    reply.call((ptr::null_mut(),));
}

#[unsafe(method(unmountWithReplyHandler:))]
fn unmountWithReplyHandler(&self, reply: &block2::DynBlock<dyn Fn()>) {
    log_event("info", "vixen-fskit unmount");
    let ready = unsafe { FSContainerStatus::ready() };
    unsafe { FSFileSystemBase::setContainerStatus(&*self.ivars().file_system, &ready) };
    reply.call(());
}

#[unsafe(method(synchronizeWithFlags:replyHandler:))]
fn synchronizeWithFlags_replyHandler(
    &self,
    _flags: FSSyncFlags,
    reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
) {
    log_event("info", "vixen-fskit synchronize");
    reply.call((ptr::null_mut(),));
}

#[unsafe(method(getAttributes:ofItem:replyHandler:))]
fn getAttributes_ofItem_replyHandler(
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
                .isValid(FSItemAttribute::Mode)
                .then(|| new_attributes.mode() as u32)
        },
        modified_time: None,
        size: unsafe {
            new_attributes
                .isValid(FSItemAttribute::Size)
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
        Err(error) => reply.call((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error))),
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

    if directory.virtual_kind() != VirtualKind::None || directory.item_id() == ROOT_LOCAL_ID {
        reply.call((
            ptr::null_mut(),
            ptr::null_mut(),
            Retained::into_raw(posix_error(libc::EACCES)),
        ));
        return;
    }

    let requested_name = fs_filename_string(name);
    let mode = if unsafe { new_attributes.isValid(FSItemAttribute::Mode) } {
        unsafe { new_attributes.mode() as u32 }
    } else if item_type == FSItemType::Directory {
        0o755
    } else {
        0o644
    };

    let create_type = if item_type == FSItemType::Directory {
        vx_services::ItemType::Directory
    } else {
        vx_services::ItemType::File
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
                    reply.call((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error)));
                }
            }
        }
        Ok(CreateResult { error, .. }) => reply.call((
            ptr::null_mut(),
            ptr::null_mut(),
            Retained::into_raw(posix_error(error)),
        )),
        Err(error) => reply.call((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error))),
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

#[unsafe(method(reclaimItem:replyHandler:))]
fn reclaimItem_replyHandler(&self, item: &FSItem, reply: &block2::DynBlock<dyn Fn(*mut NSError)>) {
    if let Some(item) = as_rust_item(item) {
        log_event(
            "info",
            format!("vixen-fskit reclaimItem item={}", item.item_id()),
        );
        self.remove_cached_item(item.item_id());
    }
    reply.call((ptr::null_mut(),));
}
