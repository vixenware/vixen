use super::*;

unsafe impl FSVolumePathConfOperations for RustVolume {
    unsafe fn maximumLinkCount(&self) -> isize {
        1
    }

    unsafe fn maximumNameLength(&self) -> isize {
        255
    }

    unsafe fn restrictsOwnershipChanges(&self) -> bool {
        true
    }

    unsafe fn truncatesLongNames(&self) -> bool {
        false
    }

    unsafe fn maximumXattrSize(&self) -> isize {
        64 * 1024
    }

    unsafe fn maximumXattrSizeInBits(&self) -> isize {
        17
    }

    unsafe fn maximumFileSize(&self) -> u64 {
        i64::MAX as u64
    }

    unsafe fn maximumFileSizeInBits(&self) -> isize {
        64
    }
}
