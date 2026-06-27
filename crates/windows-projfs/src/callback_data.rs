use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

use windows::{
    Win32::{
        Foundation::STATUS_SUCCESS,
        Storage::ProjectedFileSystem::{
            PRJ_CALLBACK_DATA, PRJ_CALLBACK_DATA_FLAGS, PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        },
    },
    core::{GUID, HRESULT},
};

#[allow(unused)]
pub struct CallbackData<'a, C> {
    pub flags: PRJ_CALLBACK_DATA_FLAGS,

    pub namespace_virtualization_context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    pub command_id: i32,

    pub file_id: GUID,
    pub data_stream_id: GUID,

    // Unknown how to handle (yet)
    // pub VersionInfo: *mut PRJ_PLACEHOLDER_VERSION_INFO,
    pub file_path: Option<PathBuf>,

    pub triggering_process_id: u32,
    pub triggering_process_image_file_name: Option<String>,

    /// The projection context should be a valid reference as long
    /// as this callback data exists.
    pub context: &'a C,
    // pub extended_parameters: Mutex<Option<PRJ_COMPLETE_COMMAND_EXTENDED_PARAMETERS>>,
}

impl<'a, C> CallbackData<'a, C> {
    pub fn execute<F>(self, executor: F) -> HRESULT
    where
        F: FnOnce(&Self) -> Result<(), HRESULT>,
    {
        match executor(&self) {
            Ok(_) => STATUS_SUCCESS.to_hresult(),
            Err(code) => code,
        }
    }
}

impl<'a, C> From<*const PRJ_CALLBACK_DATA> for CallbackData<'a, C> {
    fn from(value: *const PRJ_CALLBACK_DATA) -> Self {
        let value = unsafe { value.as_ref() }.expect("callback data should never be null");
        let file_path = if value.FilePathName.is_null() {
            None
        } else {
            Some(PathBuf::from(OsString::from_wide(unsafe {
                value.FilePathName.as_wide()
            })))
        };

        let triggering_process_image_file_name = if value.TriggeringProcessImageFileName.is_null() {
            None
        } else {
            Some(String::from_utf16_lossy(unsafe {
                value.TriggeringProcessImageFileName.as_wide()
            }))
        };

        let context = unsafe { &mut *(value.InstanceContext as *mut C) };
        Self {
            flags: value.Flags,

            namespace_virtualization_context: value.NamespaceVirtualizationContext,
            command_id: value.CommandId,

            file_path,
            file_id: value.FileId,
            data_stream_id: value.DataStreamId,

            triggering_process_id: value.TriggeringProcessId,
            triggering_process_image_file_name,

            context,
        }
    }
}
