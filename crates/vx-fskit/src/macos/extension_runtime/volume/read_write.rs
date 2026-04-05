use super::*;

unsafe impl FSVolumeReadWriteOperations for RustVolume {
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
