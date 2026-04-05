use super::*;

unsafe impl FSVolumeOperations for RustVolume {
    unsafe fn supportedVolumeCapabilities(&self) -> Retained<FSVolumeSupportedCapabilities> {
        unsafe { FSVolumeSupportedCapabilities::new() }
    }

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

    unsafe fn mountWithOptions_replyHandler(
        &self,
        _options: &FSTaskOptions,
        reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
    ) {
        log_event("info", "vixen-fskit mountWithOptions");
        reply.call((ptr::null_mut(),));
    }

    unsafe fn unmountWithReplyHandler(&self, reply: &block2::DynBlock<dyn Fn()>) {
        log_event("info", "vixen-fskit unmount");
        let ready = unsafe { FSContainerStatus::ready() };
        unsafe { FSFileSystemBase::setContainerStatus(&*self.ivars().file_system, &ready) };
        reply.call(());
    }

    unsafe fn synchronizeWithFlags_replyHandler(
        &self,
        _flags: FSSyncFlags,
        reply: &block2::DynBlock<dyn Fn(*mut NSError)>,
    ) {
        log_event("info", "vixen-fskit synchronize");
        reply.call((ptr::null_mut(),));
    }

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
            Err(error) => reply.call((ptr::null_mut(), ptr::null_mut(), Retained::into_raw(error))),
        }
    }

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
}
