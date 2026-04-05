use super::*;

impl RustVolume {
    pub(crate) fn new(file_system: &FSUnaryFileSystem) -> Retained<Self> {
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

    pub(crate) fn get_cached_item(&self, item_id: u64) -> Option<Retained<RustItem>> {
        self.ivars()
            .items
            .lock()
            .expect("items mutex poisoned")
            .get(&item_id)
            .cloned()
    }

    pub(crate) fn cache_item(&self, item: Retained<RustItem>) -> Retained<RustItem> {
        self.ivars()
            .items
            .lock()
            .expect("items mutex poisoned")
            .insert(item.item_id(), item.clone());
        item
    }

    pub(crate) fn remove_cached_item(&self, item_id: u64) {
        if item_id == ROOT_LOCAL_ID || item_id == VIXENFS_DIR_ID || item_id == STATUS_JSON_ID {
            return;
        }

        self.ivars()
            .items
            .lock()
            .expect("items mutex poisoned")
            .remove(&item_id);
        self.ivars()
            .routed
            .lock()
            .expect("routed items mutex poisoned")
            .by_key
            .retain(|_, local_id| *local_id != item_id);
    }

    pub(crate) fn local_item_id(&self, generation: u64, backend_item_id: u64) -> u64 {
        if backend_item_id == ROOT_BACKEND_ID {
            log_event(
                "debug",
                format!(
                    "vixen-fskit local_item_id root backend={} generation={} -> local={}",
                    backend_item_id, generation, ROOT_LOCAL_ID
                ),
            );
            return ROOT_LOCAL_ID;
        }

        let current_generation = root_peer_state().generation();
        let live_local_ids = self
            .ivars()
            .items
            .lock()
            .expect("items mutex poisoned")
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();

        let mut routed = self
            .ivars()
            .routed
            .lock()
            .expect("routed items mutex poisoned");
        routed.by_key.retain(|key, local_id| {
            key.connection_generation == current_generation && live_local_ids.contains(local_id)
        });

        let key = RoutedItemKey {
            connection_generation: generation,
            backend_item_id,
        };
        if let Some(existing) = routed.by_key.get(&key) {
            log_event(
                "debug",
                format!(
                    "vixen-fskit local_item_id reuse backend={} generation={} -> local={}",
                    backend_item_id, generation, existing
                ),
            );
            return *existing;
        }

        let local_item_id = loop {
            let candidate = routed.next_local_id;
            routed.next_local_id += 1;
            if !live_local_ids.contains(&candidate) {
                break candidate;
            }
            log_event(
                "debug",
                format!("vixen-fskit local_item_id skip collision local={candidate}"),
            );
        };
        routed.by_key.insert(key, local_item_id);
        log_event(
            "debug",
            format!(
                "vixen-fskit local_item_id insert backend={} generation={} -> local={}",
                backend_item_id, generation, local_item_id
            ),
        );
        local_item_id
    }

    pub(crate) fn cached_backend_item(
        &self,
        local_item_id: u64,
        parent_local_id: u64,
        generation: u64,
        name: &str,
        attrs: &vx_services::ItemAttributes,
    ) -> Retained<RustItem> {
        if let Some(item) = self.get_cached_item(local_item_id) {
            log_event(
                "debug",
                format!(
                    "vixen-fskit cached_backend_item hit local={} backend={} parent={} generation={} requested_name={} existing={}",
                    local_item_id,
                    attrs.item_id,
                    parent_local_id,
                    generation,
                    name,
                    item.debug_summary()
                ),
            );
            item.update_from_backend(parent_local_id, name, attrs);
            log_event(
                "debug",
                format!(
                    "vixen-fskit cached_backend_item updated local={} backend={} now={}",
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
                "vixen-fskit cached_backend_item insert requested_name={} {}",
                name,
                item.debug_summary()
            ),
        );
        self.cache_item(item)
    }

    pub(crate) fn connected_client_for_item(
        &self,
        item: &RustItem,
    ) -> Result<(vx_services::VfsClient, u64), Retained<NSError>> {
        let (client, generation) = root_peer_state().connected_client().map_err(posix_error)?;
        if matches!(item.virtual_kind(), VirtualKind::Root | VirtualKind::None)
            && item.generation() != 0
            && item.generation() != generation
        {
            log_event(
                "warn",
                format!(
                    "vixen-fskit stale item generation item_gen={} live_gen={} {}",
                    item.generation(),
                    generation,
                    item.debug_summary()
                ),
            );
            return Err(posix_error(libc::ESTALE));
        }
        Ok((client, generation))
    }

    pub(crate) fn directory_allows_mutation(&self, directory: &RustItem) -> bool {
        matches!(
            directory.virtual_kind(),
            VirtualKind::Root | VirtualKind::None
        )
    }

    pub(crate) fn attributes_for_item(
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

    pub(crate) fn refresh_backend_item(
        &self,
        item: &RustItem,
        request: Option<&FSItemGetAttributesRequest>,
    ) -> Result<Retained<FSItemAttributes>, Retained<NSError>> {
        let backend_item_id = item.backend_item_id();
        let parent_local_id = item.parent_local_id();
        let item_name = fs_filename_string(&item.fs_name());
        let (client, current_generation) = self.connected_client_for_item(item)?;
        let result = block_on_vfs(async move {
            let result = client
                .get_attributes(backend_item_id)
                .await
                .map_err(|error| vox_rpc_errno("get_attributes", error))?;
            Ok::<_, i32>((result, current_generation))
        })?;

        let (result, _) = result;
        if result.error != 0 {
            return Err(posix_error(result.error));
        }

        log_event(
            "debug",
            format!(
                "vixen-fskit refresh_backend_item local={} backend={} generation={} result_backend={} type={:?} mode={:#o} size={}",
                item.item_id(),
                backend_item_id,
                item.generation(),
                result.attrs.item_id,
                result.attrs.item_type,
                result.attrs.mode,
                result.attrs.size
            ),
        );

        item.update_from_backend(parent_local_id, &item_name, &result.attrs);
        Ok(item.attributes_for_request(request))
    }

    pub(crate) fn lookup_item(
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
        let (client, generation) = self.connected_client_for_item(directory)?;
        let result = block_on_vfs(async move {
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
                "vixen-fskit lookup_item.rpc name={} parent_backend={} generation={} result_backend={} type={:?} mode={:#o} size={}",
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
                    "vixen-fskit lookup_item.rpc returned root for name={} parent_backend={}",
                    requested_name, parent_backend_id
                ),
            );
        }

        let local_item_id = self.local_item_id(generation, attrs.item_id);
        log_event(
            "debug",
            format!(
                "vixen-fskit lookup_item.map name={} backend={} generation={} -> local={}",
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

    pub(crate) fn enumerate_directory(
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
                    format!(
                        "vixen-fskit enumerate virtual entry {}",
                        item.debug_summary()
                    ),
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
            let initial = unsafe { FSDirectoryCookieInitial };
            let meta_dir = self
                .get_cached_item(VIXENFS_DIR_ID)
                .expect(".vixenfs dir should be cached");
            let backend_cookie = if cookie == initial || cookie == 1 {
                0
            } else {
                cookie.saturating_sub(1)
            };

            let mut backend_entries = Vec::<(Retained<RustItem>, FSDirectoryCookie)>::new();
            let mut next_verifier = verifier;
            let backend_result = root_peer_state()
                .connected_client()
                .map_err(posix_error)
                .and_then(|(client, generation)| {
                    block_on_vfs(async move {
                        let listing = client
                            .read_dir(ROOT_BACKEND_ID, backend_cookie, verifier)
                            .await
                            .map_err(|error| vox_rpc_errno("read_dir", error))?;
                        Ok::<_, i32>((listing, generation))
                    })
                });

            let backend_error = match backend_result {
                Ok((
                    ReadDirResult {
                        entries,
                        next_cookie,
                        verifier,
                        error: 0,
                    },
                    generation,
                )) => {
                    next_verifier = verifier;
                    let total_entries = entries.len();
                    for (index, entry) in entries.into_iter().enumerate() {
                        let local_item_id = self.local_item_id(generation, entry.item_id);
                        let item = self.cached_backend_item(
                            local_item_id,
                            ROOT_LOCAL_ID,
                            generation,
                            &entry.name,
                            &entry_attributes(&entry),
                        );
                        let entry_cookie = if index + 1 == total_entries && next_cookie == 0 {
                            0
                        } else {
                            backend_cookie + index as u64 + 2
                        };
                        backend_entries.push((item, entry_cookie));
                    }
                    None
                }
                Ok((ReadDirResult { error, .. }, _)) => Some(posix_error(error)),
                Err(error) => Some(error),
            };

            if cookie == initial {
                let attrs = meta_dir.attributes_for_request(attributes);
                let next_cookie = if backend_entries.is_empty() { 0 } else { 1 };
                let packed = unsafe {
                    packer.packEntryWithName_itemType_itemID_nextCookie_attributes(
                        &meta_dir.fs_name(),
                        attrs.r#type(),
                        FSItemID(meta_dir.item_id()),
                        next_cookie,
                        if attributes.is_some() {
                            Some(attrs.as_ref())
                        } else {
                            None
                        },
                    )
                };
                if !packed {
                    return Ok(next_verifier);
                }
            }

            if let Some(error) = backend_error {
                if cookie == initial || cookie == 1 {
                    return Ok(next_verifier);
                }
                return Err(error);
            }

            for (item, next_cookie) in backend_entries {
                let attrs = item.attributes_for_request(attributes);
                log_event(
                    "debug",
                    format!(
                        "vixen-fskit pack root entry next_cookie={} {}",
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

            return Ok(next_verifier);
        }

        let directory_backend_id = directory.backend_item_id();
        let parent_local_id = directory.item_id();
        let (client, generation) = self.connected_client_for_item(directory)?;
        let result = block_on_vfs(async move {
            let listing = client
                .read_dir(directory_backend_id, cookie, verifier)
                .await
                .map_err(|error| vox_rpc_errno("read_dir", error))?;
            Ok::<_, i32>((listing, generation))
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

        let total_entries = entries.len();
        for (index, entry) in entries.into_iter().enumerate() {
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
                    "vixen-fskit enumerate entry local={} backend={} name={} type={:?} size={}",
                    local_item_id, entry.item_id, entry.name, entry.item_type, entry.size
                ),
            );
            let entry_cookie = if index + 1 == total_entries && next_cookie == 0 {
                0
            } else {
                cookie + index as u64 + 1
            };
            let packed = unsafe {
                packer.packEntryWithName_itemType_itemID_nextCookie_attributes(
                    &item.fs_name(),
                    attrs.r#type(),
                    FSItemID(item.item_id()),
                    entry_cookie,
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

    pub(crate) fn read_item(
        &self,
        item: &RustItem,
        offset: libc::off_t,
        length: usize,
        buffer: &FSMutableFileDataBuffer,
    ) -> Result<usize, Retained<NSError>> {
        log_event(
            "debug",
            format!(
                "vixen-fskit read_item.begin offset={} length={} {}",
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
                    "vixen-fskit read_item.virtual actual={actual} {}",
                    item.debug_summary()
                ),
            );
            return Ok(actual);
        }

        let backend_item_id = item.backend_item_id();
        let offset = offset.max(0) as u64;
        let (client, _) = self.connected_client_for_item(item)?;
        let result = block_on_vfs(async move {
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
                        "vixen-fskit read item={} backend={} offset={offset} actual={actual}",
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
                        "vixen-fskit read_item.error errno={} {}",
                        error,
                        item.debug_summary()
                    ),
                );
                Err(posix_error(error))
            }
        }
    }
}
