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
        format!("vixen-fskit checkAccess access={:#x} {}", access.0, details),
    );
    reply.call((Bool::YES, ptr::null_mut()));
}
