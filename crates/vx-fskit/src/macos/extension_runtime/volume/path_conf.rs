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
    true
}

#[unsafe(method(truncatesLongNames))]
fn truncatesLongNames(&self) -> bool {
    false
}

#[unsafe(method(maximumXattrSize))]
fn maximumXattrSize(&self) -> isize {
    64 * 1024
}

#[unsafe(method(maximumXattrSizeInBits))]
fn maximumXattrSizeInBits(&self) -> isize {
    17
}

#[unsafe(method(maximumFileSize))]
fn maximumFileSize(&self) -> u64 {
    i64::MAX as u64
}

#[unsafe(method(maximumFileSizeInBits))]
fn maximumFileSizeInBits(&self) -> isize {
    64
}
