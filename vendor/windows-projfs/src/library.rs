use std::ffi::c_void;

use windows::{
    Win32::{
        Foundation::BOOLEAN,
        Storage::ProjectedFileSystem::{
            PRJ_CALLBACKS, PRJ_DIR_ENTRY_BUFFER_HANDLE, PRJ_EXTENDED_INFO, PRJ_FILE_BASIC_INFO,
            PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_PLACEHOLDER_INFO,
            PRJ_PLACEHOLDER_VERSION_INFO, PRJ_STARTVIRTUALIZING_OPTIONS,
        },
    },
    core::{GUID, PCWSTR},
};

pub trait ProjectedFSLibrary {
    unsafe fn prj_allocate_aligned_buffer(
        &self,
        namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        size: usize,
    ) -> *mut c_void;

    unsafe fn prj_free_aligned_buffer(&self, buffer: *const c_void);
    unsafe fn prj_file_name_compare(&self, filename1: PCWSTR, filename2: PCWSTR) -> i32;

    unsafe fn prj_file_name_match(&self, filenametocheck: PCWSTR, pattern: PCWSTR) -> BOOLEAN;

    unsafe fn prj_mark_directory_as_placeholder(
        &self,
        rootpathname: PCWSTR,
        targetpathname: PCWSTR,
        versioninfo: Option<*const PRJ_PLACEHOLDER_VERSION_INFO>,
        virtualizationinstanceid: *const GUID,
    ) -> windows::core::Result<()>;

    unsafe fn prj_start_virtualizing(
        &self,
        virtualizationrootpath: PCWSTR,
        callbacks: *const PRJ_CALLBACKS,
        instancecontext: Option<*const ::core::ffi::c_void>,
        options: Option<*const PRJ_STARTVIRTUALIZING_OPTIONS>,
    ) -> windows::core::Result<PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT>;

    unsafe fn prj_stop_virtualizing(
        &self,
        namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    );

    unsafe fn prj_fill_dir_entry_buffer2(
        &self,
        direntrybufferhandle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
        filename: PCWSTR,
        filebasicinfo: Option<*const PRJ_FILE_BASIC_INFO>,
        extendedinfo: Option<*const PRJ_EXTENDED_INFO>,
    ) -> windows::core::Result<()>;

    unsafe fn prj_write_file_data(
        &self,
        namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        datastreamid: *const GUID,
        buffer: *const c_void,
        byteoffset: u64,
        length: u32,
    ) -> windows::core::Result<()>;

    unsafe fn prj_write_placeholder_info(
        &self,
        namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        destinationfilename: PCWSTR,
        placeholderinfo: *const PRJ_PLACEHOLDER_INFO,
        placeholderinfosize: u32,
    ) -> windows::core::Result<()>;

    unsafe fn prj_write_placeholder_info2(
        &self,
        namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        destinationfilename: PCWSTR,
        placeholderinfo: *const PRJ_PLACEHOLDER_INFO,
        placeholderinfosize: u32,
        extendedinfo: ::core::option::Option<*const PRJ_EXTENDED_INFO>,
    ) -> windows::core::Result<()>;
}

#[cfg(not(feature = "dynamic-import"))]
mod lib_impl {
    use std::{ffi::c_void, sync::Arc};

    use windows::{
        Win32::{
            Foundation::BOOLEAN,
            Storage::ProjectedFileSystem::{
                PRJ_CALLBACKS, PRJ_DIR_ENTRY_BUFFER_HANDLE, PRJ_EXTENDED_INFO, PRJ_FILE_BASIC_INFO,
                PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_PLACEHOLDER_INFO,
                PRJ_PLACEHOLDER_VERSION_INFO, PRJ_STARTVIRTUALIZING_OPTIONS,
            },
        },
        core::{GUID, PCWSTR},
    };

    use super::ProjectedFSLibrary;

    pub struct StaticallyLinkedLibrary;

    impl ProjectedFSLibrary for StaticallyLinkedLibrary {
        unsafe fn prj_allocate_aligned_buffer(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            size: usize,
        ) -> *mut c_void {
            use windows::Win32::Storage::ProjectedFileSystem::PrjAllocateAlignedBuffer;
            PrjAllocateAlignedBuffer(namespacevirtualizationcontext, size)
        }

        unsafe fn prj_free_aligned_buffer(&self, buffer: *const c_void) {
            use windows::Win32::Storage::ProjectedFileSystem::PrjFreeAlignedBuffer;
            PrjFreeAlignedBuffer(buffer)
        }

        unsafe fn prj_file_name_compare(&self, filename1: PCWSTR, filename2: PCWSTR) -> i32 {
            use windows::Win32::Storage::ProjectedFileSystem::PrjFileNameCompare;
            PrjFileNameCompare(filename1, filename2)
        }

        unsafe fn prj_file_name_match(&self, filenametocheck: PCWSTR, pattern: PCWSTR) -> BOOLEAN {
            use windows::Win32::Storage::ProjectedFileSystem::PrjFileNameMatch;
            PrjFileNameMatch(filenametocheck, pattern)
        }

        unsafe fn prj_mark_directory_as_placeholder(
            &self,
            rootpathname: PCWSTR,
            targetpathname: PCWSTR,
            versioninfo: Option<*const PRJ_PLACEHOLDER_VERSION_INFO>,
            virtualizationinstanceid: *const GUID,
        ) -> windows::core::Result<()> {
            use windows::Win32::Storage::ProjectedFileSystem::PrjMarkDirectoryAsPlaceholder;
            PrjMarkDirectoryAsPlaceholder(
                rootpathname,
                targetpathname,
                versioninfo,
                virtualizationinstanceid,
            )
        }

        unsafe fn prj_start_virtualizing(
            &self,
            virtualizationrootpath: PCWSTR,
            callbacks: *const PRJ_CALLBACKS,
            instancecontext: Option<*const core::ffi::c_void>,
            options: Option<*const PRJ_STARTVIRTUALIZING_OPTIONS>,
        ) -> windows::core::Result<PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT> {
            use windows::Win32::Storage::ProjectedFileSystem::PrjStartVirtualizing;
            PrjStartVirtualizing(virtualizationrootpath, callbacks, instancecontext, options)
        }

        unsafe fn prj_stop_virtualizing(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        ) {
            use windows::Win32::Storage::ProjectedFileSystem::PrjStopVirtualizing;
            PrjStopVirtualizing(namespacevirtualizationcontext)
        }

        unsafe fn prj_fill_dir_entry_buffer2(
            &self,
            direntrybufferhandle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
            filename: PCWSTR,
            filebasicinfo: Option<*const PRJ_FILE_BASIC_INFO>,
            extendedinfo: Option<*const PRJ_EXTENDED_INFO>,
        ) -> windows::core::Result<()> {
            use windows::Win32::Storage::ProjectedFileSystem::PrjFillDirEntryBuffer2;
            PrjFillDirEntryBuffer2(direntrybufferhandle, filename, filebasicinfo, extendedinfo)
        }

        unsafe fn prj_write_file_data(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            datastreamid: *const GUID,
            buffer: *const c_void,
            byteoffset: u64,
            length: u32,
        ) -> windows::core::Result<()> {
            use windows::Win32::Storage::ProjectedFileSystem::PrjWriteFileData;
            PrjWriteFileData(
                namespacevirtualizationcontext,
                datastreamid,
                buffer,
                byteoffset,
                length,
            )
        }

        unsafe fn prj_write_placeholder_info(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            destinationfilename: PCWSTR,
            placeholderinfo: *const PRJ_PLACEHOLDER_INFO,
            placeholderinfosize: u32,
        ) -> windows::core::Result<()> {
            use windows::Win32::Storage::ProjectedFileSystem::PrjWritePlaceholderInfo;
            PrjWritePlaceholderInfo(
                namespacevirtualizationcontext,
                destinationfilename,
                placeholderinfo,
                placeholderinfosize,
            )
        }

        unsafe fn prj_write_placeholder_info2(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            destinationfilename: PCWSTR,
            placeholderinfo: *const PRJ_PLACEHOLDER_INFO,
            placeholderinfosize: u32,
            extendedinfo: core::option::Option<*const PRJ_EXTENDED_INFO>,
        ) -> windows::core::Result<()> {
            use windows::Win32::Storage::ProjectedFileSystem::PrjWritePlaceholderInfo2;
            PrjWritePlaceholderInfo2(
                namespacevirtualizationcontext,
                destinationfilename,
                placeholderinfo,
                placeholderinfosize,
                extendedinfo,
            )
        }
    }

    pub fn load_library() -> crate::Result<Arc<dyn ProjectedFSLibrary>> {
        Ok(Arc::new(StaticallyLinkedLibrary))
    }
}

#[cfg(feature = "dynamic-import")]
mod lib_impl {
    use std::{ffi::c_void, ptr, sync::Arc};

    use windows::{
        Win32::{
            Foundation::BOOLEAN,
            Storage::ProjectedFileSystem::{
                PRJ_CALLBACKS, PRJ_DIR_ENTRY_BUFFER_HANDLE, PRJ_EXTENDED_INFO, PRJ_FILE_BASIC_INFO,
                PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_PLACEHOLDER_INFO,
                PRJ_PLACEHOLDER_VERSION_INFO, PRJ_STARTVIRTUALIZING_OPTIONS,
            },
        },
        core::{GUID, HRESULT, PCWSTR},
    };

    use super::ProjectedFSLibrary;
    use crate::{Error, Result};

    macro_rules! define_helper {
        (
            $name:ident {
                $(
                    fn $fn_name:ident ( $( $arg_name:ident : $arg_ty:ty ),* $(,)? ) $( -> $ret_ty:ty )?
                ),*
                $(,)?
            }
        ) => {
            #[allow(non_snake_case)]
            pub struct $name {
                _library: libloading::Library,

                $(
                    $fn_name: extern "system" fn($($arg_name: $arg_ty),*) $(-> $ret_ty)?,
                )*
            }

            impl $name {
                pub fn new(library: libloading::Library) -> crate::Result<Self> {
                    Ok(Self {
                        $(
                            $fn_name: unsafe { *library.get(concat!(stringify!($fn_name), "\0").as_bytes())? },
                        )*

                        _library: library,
                    })
                }
            }
        };
    }

    define_helper! {
        DynamicallyLoadedLibrary {
            fn PrjAllocateAlignedBuffer(namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, size: usize) -> *mut c_void,
            fn PrjFreeAlignedBuffer(buffer : *const c_void) -> (),

            fn PrjFileNameCompare(filename1: PCWSTR, filename2: PCWSTR) -> i32,
            fn PrjFileNameMatch(filenametocheck: PCWSTR, pattern: PCWSTR) -> BOOLEAN,

            fn PrjMarkDirectoryAsPlaceholder(rootpathname: PCWSTR, targetpathname: PCWSTR, versioninfo: *const PRJ_PLACEHOLDER_VERSION_INFO, virtualizationinstanceid : *const GUID) -> HRESULT,
            fn PrjStartVirtualizing(virtualizationrootpath: PCWSTR, callbacks: *const PRJ_CALLBACKS, instancecontext: *const ::core::ffi::c_void, options : *const PRJ_STARTVIRTUALIZING_OPTIONS, namespacevirtualizationcontext : *mut PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT) -> HRESULT,
            fn PrjStopVirtualizing(namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT) -> (),

            fn PrjFillDirEntryBuffer2(direntrybufferhandle: PRJ_DIR_ENTRY_BUFFER_HANDLE, filename: PCWSTR, filebasicinfo : *const PRJ_FILE_BASIC_INFO, extendedinfo : *const PRJ_EXTENDED_INFO) -> HRESULT,
            fn PrjWriteFileData(namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, datastreamid: *const GUID, buffer : *const ::core::ffi::c_void, byteoffset : u64, length : u32) -> HRESULT,
            fn PrjWritePlaceholderInfo(namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, destinationfilename: PCWSTR, placeholderinfo: *const PRJ_PLACEHOLDER_INFO, placeholderinfosize : u32) -> HRESULT,
            fn PrjWritePlaceholderInfo2(namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, destinationfilename: PCWSTR, placeholderinfo: *const PRJ_PLACEHOLDER_INFO, placeholderinfosize : u32, extendedinfo : *const PRJ_EXTENDED_INFO) -> HRESULT,
        }
    }

    impl ProjectedFSLibrary for DynamicallyLoadedLibrary {
        unsafe fn prj_allocate_aligned_buffer(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            size: usize,
        ) -> *mut c_void {
            (self.PrjAllocateAlignedBuffer)(namespacevirtualizationcontext, size)
        }

        unsafe fn prj_free_aligned_buffer(&self, buffer: *const c_void) {
            (self.PrjFreeAlignedBuffer)(buffer)
        }

        unsafe fn prj_file_name_compare(&self, filename1: PCWSTR, filename2: PCWSTR) -> i32 {
            (self.PrjFileNameCompare)(filename1, filename2)
        }

        unsafe fn prj_file_name_match(&self, filenametocheck: PCWSTR, pattern: PCWSTR) -> BOOLEAN {
            (self.PrjFileNameMatch)(filenametocheck, pattern)
        }

        unsafe fn prj_mark_directory_as_placeholder(
            &self,
            rootpathname: PCWSTR,
            targetpathname: PCWSTR,
            versioninfo: Option<*const PRJ_PLACEHOLDER_VERSION_INFO>,
            virtualizationinstanceid: *const GUID,
        ) -> windows::core::Result<()> {
            (self.PrjMarkDirectoryAsPlaceholder)(
                rootpathname,
                targetpathname,
                versioninfo.unwrap_or(ptr::null()),
                virtualizationinstanceid,
            )
            .ok()
        }

        unsafe fn prj_start_virtualizing(
            &self,
            virtualizationrootpath: PCWSTR,
            callbacks: *const PRJ_CALLBACKS,
            instancecontext: Option<*const core::ffi::c_void>,
            options: Option<*const PRJ_STARTVIRTUALIZING_OPTIONS>,
        ) -> windows::core::Result<PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT> {
            let mut result = ::std::mem::zeroed();
            (self.PrjStartVirtualizing)(
                virtualizationrootpath,
                callbacks,
                instancecontext.unwrap_or(ptr::null()),
                options.unwrap_or(ptr::null()),
                &mut result,
            )
            .from_abi(result)
        }

        unsafe fn prj_stop_virtualizing(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        ) {
            (self.PrjStopVirtualizing)(namespacevirtualizationcontext)
        }

        unsafe fn prj_fill_dir_entry_buffer2(
            &self,
            direntrybufferhandle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
            filename: PCWSTR,
            filebasicinfo: Option<*const PRJ_FILE_BASIC_INFO>,
            extendedinfo: Option<*const PRJ_EXTENDED_INFO>,
        ) -> windows::core::Result<()> {
            (self.PrjFillDirEntryBuffer2)(
                direntrybufferhandle,
                filename,
                filebasicinfo.unwrap_or(ptr::null()),
                extendedinfo.unwrap_or(ptr::null()),
            )
            .ok()
        }

        unsafe fn prj_write_file_data(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            datastreamid: *const GUID,
            buffer: *const c_void,
            byteoffset: u64,
            length: u32,
        ) -> windows::core::Result<()> {
            (self.PrjWriteFileData)(
                namespacevirtualizationcontext,
                datastreamid,
                buffer,
                byteoffset,
                length,
            )
            .ok()
        }

        unsafe fn prj_write_placeholder_info(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            destinationfilename: PCWSTR,
            placeholderinfo: *const PRJ_PLACEHOLDER_INFO,
            placeholderinfosize: u32,
        ) -> windows::core::Result<()> {
            (self.PrjWritePlaceholderInfo)(
                namespacevirtualizationcontext,
                destinationfilename,
                placeholderinfo,
                placeholderinfosize,
            )
            .ok()
        }

        unsafe fn prj_write_placeholder_info2(
            &self,
            namespacevirtualizationcontext: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
            destinationfilename: PCWSTR,
            placeholderinfo: *const PRJ_PLACEHOLDER_INFO,
            placeholderinfosize: u32,
            extendedinfo: Option<*const PRJ_EXTENDED_INFO>,
        ) -> windows::core::Result<()> {
            (self.PrjWritePlaceholderInfo2)(
                namespacevirtualizationcontext,
                destinationfilename,
                placeholderinfo,
                placeholderinfosize,
                extendedinfo.unwrap_or(ptr::null()),
            )
            .ok()
        }
    }

    pub fn load_library() -> Result<Arc<dyn ProjectedFSLibrary>> {
        let library = match unsafe { libloading::Library::new("projectedfslib") } {
            Ok(library) => DynamicallyLoadedLibrary::new(library)?,
            Err(error) => {
                return Err(match &error {
                    libloading::Error::LoadLibraryExW { .. } => {
                        use std::error::Error as _;

                        error
                            .source()
                            // Get the underlying os error
                            .and_then(|error| error.downcast_ref::<std::io::Error>())
                            // Get the os error code
                            .and_then(|error| error.raw_os_error())
                            // check if it's 126: The specified module could not be found.
                            .filter(|code| *code == 126)
                            .map(|_| Error::WindowsFeatureNotEnabled)
                            // else return a library error
                            .unwrap_or(Error::LibraryError(error))
                    }
                    _ => Error::LibraryError(error),
                });
            }
        };

        Ok(Arc::new(library))
    }
}

pub use lib_impl::load_library;
