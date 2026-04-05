use super::*;

unsafe impl FSVolumeOpenCloseOperations for RustVolume {
    unsafe fn isOpenCloseInhibited(&self) -> bool {
        true
    }

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
