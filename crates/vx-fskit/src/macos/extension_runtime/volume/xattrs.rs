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
