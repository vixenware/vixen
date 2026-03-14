use std::{ffi::c_void, sync::Arc};

use windows::Win32::Storage::ProjectedFileSystem::PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT;

use crate::library::ProjectedFSLibrary;

pub struct PrjAlignedBuffer {
    library: Arc<dyn ProjectedFSLibrary>,

    length: usize,
    raw_buffer: *mut c_void,
}

impl PrjAlignedBuffer {
    pub fn allocate(
        library: Arc<dyn ProjectedFSLibrary>,
        context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        length: usize,
    ) -> Option<Self> {
        let raw_buffer = unsafe { library.prj_allocate_aligned_buffer(context, length) };
        if raw_buffer.is_null() {
            None
        } else {
            Some(Self {
                library,
                length,
                raw_buffer,
            })
        }
    }

    pub fn buffer(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.raw_buffer as *mut u8, self.length) }
    }
}

impl Drop for PrjAlignedBuffer {
    fn drop(&mut self) {
        unsafe { self.library.prj_free_aligned_buffer(self.raw_buffer) };
    }
}
