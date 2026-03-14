use std::{
    self,
    cell::RefCell,
    collections::{BTreeMap, btree_map::Entry},
    ffi::c_void,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use parking_lot::Mutex;
use windows::{
    Win32::Storage::ProjectedFileSystem::{
        PRJ_CALLBACKS, PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_NOTIFICATION_MAPPING,
        PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED, PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED,
        PRJ_NOTIFY_FILE_HANDLE_CLOSED_NO_MODIFICATION, PRJ_NOTIFY_FILE_OPENED,
        PRJ_NOTIFY_FILE_OVERWRITTEN, PRJ_NOTIFY_FILE_PRE_CONVERT_TO_FULL, PRJ_NOTIFY_FILE_RENAMED,
        PRJ_NOTIFY_HARDLINK_CREATED, PRJ_NOTIFY_NEW_FILE_CREATED, PRJ_NOTIFY_PRE_DELETE,
        PRJ_NOTIFY_PRE_RENAME, PRJ_NOTIFY_PRE_SET_HARDLINK, PRJ_NOTIFY_TYPES,
        PRJ_STARTVIRTUALIZING_OPTIONS,
    },
    core::{GUID, PCWSTR},
};

use crate::{
    DirectoryEntry, Error, ProjectedFileSystemSource, Result,
    library::{ProjectedFSLibrary, load_library},
};

#[derive(Default)]
struct FileNameU16Cache {
    cache: BTreeMap<String, Vec<u16>>,
}

impl FileNameU16Cache {
    pub fn get_or_cache(&mut self, name: String) -> &[u16] {
        match self.cache.entry(name) {
            Entry::Occupied(entry) => entry.into_mut().as_slice(),
            Entry::Vacant(entry) => {
                let mut name = entry.key().encode_utf16().collect::<Vec<_>>();
                name.push(0);

                entry.insert(name).as_slice()
            }
        }
    }
}

struct DirectoryIteration {
    id: u128,

    entries: Vec<DirectoryEntry>,
    current_entry: usize,

    name_cache: Rc<RefCell<FileNameU16Cache>>,
    search_expression: Option<Vec<u16>>,
}

impl DirectoryIteration {
    pub fn from_unsorted(
        library: &dyn ProjectedFSLibrary,
        id: u128,
        mut entries: Vec<DirectoryEntry>,
    ) -> Self {
        let name_cache: Rc<RefCell<FileNameU16Cache>> = Default::default();
        entries.sort_unstable_by({
            let name_cache = name_cache.clone();
            move |a, b| {
                let mut name_cache = name_cache.borrow_mut();
                let name_a = name_cache.get_or_cache(a.name().to_string()).as_ptr();
                let name_b = name_cache.get_or_cache(b.name().to_string()).as_ptr();

                let result =
                    unsafe { library.prj_file_name_compare(PCWSTR(name_a), PCWSTR(name_b)) };
                result.cmp(&0)
            }
        });

        Self {
            id,

            entries,
            current_entry: 0,

            name_cache,
            search_expression: None,
        }
    }

    pub fn peek_entry(&mut self) -> Option<&DirectoryEntry> {
        let index = self.current_entry;
        if index < self.entries.len() {
            Some(&self.entries[index])
        } else {
            None
        }
    }

    pub fn consume_entry(&mut self) {
        self.current_entry += 1;
    }

    pub fn reset_enumeration(&mut self) {
        self.search_expression = None;
        self.current_entry = 0;
    }
}

pub type RawProjectionContext = Mutex<ProjectionContext>;
pub struct ProjectionContext {
    library: Arc<dyn ProjectedFSLibrary>,
    source: Box<dyn ProjectedFileSystemSource>,
    directory_enumerations: BTreeMap<u128, DirectoryIteration>,
}

impl ProjectionContext {
    pub fn register_enumeration(&mut self, target: PathBuf, id: u128) {
        let entries = self.source.list_directory(&target);
        tracing::info!(
            target: "vx_vfs::windows",
            "ProjFS register_enumeration id={:X} path={} entry_count={}",
            id,
            target.to_string_lossy(),
            entries.len()
        );
        let old_enumeration = self.directory_enumerations.insert(
            id,
            DirectoryIteration::from_unsorted(&*self.library, id, entries),
        );

        if let Some(enumeration) = old_enumeration {
            log::warn!("Duplicate enumeration id {:X}", enumeration.id);
        }
    }

    pub fn finish_enumeration(&mut self, id: u128) -> bool {
        tracing::info!(target: "vx_vfs::windows", "ProjFS finish_enumeration id={:X}", id);
        self.directory_enumerations.remove(&id).is_some()
    }
}

pub struct ProjectedFileSystem {
    library: Arc<dyn ProjectedFSLibrary>,
    instance_id: GUID,

    raw_context: *mut RawProjectionContext,
    virtualization_context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
}

static EMPTY_U16_STRING: &[u16] = &[0];
impl ProjectedFileSystem {
    pub fn new(root: &Path, source: impl ProjectedFileSystemSource + 'static) -> Result<Self> {
        let instance_id = GUID::new()?;
        let mut root_encoded = root.to_string_lossy().encode_utf16().collect::<Vec<_>>();
        root_encoded.push(0);

        let library = load_library()?;
        unsafe {
            library.prj_mark_directory_as_placeholder(
                PCWSTR(root_encoded.as_ptr()),
                PCWSTR::null(),
                None,
                &instance_id,
            )
        }
        .map_err(Error::MarkProjectionRoot)?;

        let context = Box::new(Mutex::new(ProjectionContext {
            library: library.clone(),
            source: Box::new(source),
            directory_enumerations: Default::default(),
        }));

        let callbacks = Box::new(PRJ_CALLBACKS {
            StartDirectoryEnumerationCallback: Some(native::start_directory_enumeration_callback),
            GetDirectoryEnumerationCallback: Some(native::get_directory_enumeration_callback),
            EndDirectoryEnumerationCallback: Some(native::end_directory_enumeration_callback),

            GetPlaceholderInfoCallback: Some(native::get_placeholder_information_callback),
            GetFileDataCallback: Some(native::get_file_data_callback),

            NotificationCallback: Some(native::notification_callback),
            ..Default::default()
        });

        let raw_context = Box::into_raw(context);
        let virtualization_context = {
            #[allow(clippy::identity_op)]
            let notification_mask = 0
                | PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED.0
                | PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED.0
                | PRJ_NOTIFY_FILE_HANDLE_CLOSED_NO_MODIFICATION.0
                | PRJ_NOTIFY_FILE_OPENED.0
                | PRJ_NOTIFY_FILE_OVERWRITTEN.0
                | PRJ_NOTIFY_FILE_PRE_CONVERT_TO_FULL.0
                | PRJ_NOTIFY_FILE_RENAMED.0
                | PRJ_NOTIFY_HARDLINK_CREATED.0
                | PRJ_NOTIFY_NEW_FILE_CREATED.0
                | PRJ_NOTIFY_PRE_DELETE.0
                | PRJ_NOTIFY_PRE_RENAME.0
                | PRJ_NOTIFY_PRE_SET_HARDLINK.0;

            let mut notification_mapping = PRJ_NOTIFICATION_MAPPING {
                NotificationBitMask: PRJ_NOTIFY_TYPES(notification_mask),
                NotificationRoot: PCWSTR(EMPTY_U16_STRING.as_ptr()),
            };

            let options = PRJ_STARTVIRTUALIZING_OPTIONS {
                NotificationMappings: &mut notification_mapping,
                NotificationMappingsCount: 1,

                ..Default::default()
            };

            let result = unsafe {
                library.prj_start_virtualizing(
                    PCWSTR(root_encoded.as_ptr()),
                    &*callbacks,
                    Some(raw_context as *const c_void),
                    Some(&options),
                )
            };
            match result {
                Ok(virtualization_context) => virtualization_context,
                Err(err) => {
                    unsafe { drop(Box::from_raw(raw_context)) }
                    return Err(Error::StartProjection(err));
                }
            }
        };

        log::debug!(
            "Started projection {:X} at {}",
            instance_id.to_u128(),
            root.to_string_lossy()
        );
        Ok(Self {
            library,
            instance_id,

            raw_context,
            virtualization_context,
        })
    }
}

impl Drop for ProjectedFileSystem {
    fn drop(&mut self) {
        log::trace!("Stopping projection for {:X}", self.instance_id.to_u128());

        /* Shutdown projection and wait for all callbacks to finish. */
        unsafe {
            self.library
                .prj_stop_virtualizing(self.virtualization_context)
        };

        /*
         * PrjStopVirtualizing waits untill all callbacks have been processed.
         * Therefore it's safe to assume that no one else will use the raw_context.
         */
        unsafe { drop(Box::from_raw(self.raw_context)) };

        log::debug!("Stopped projection for {:X}", self.instance_id.to_u128());
    }
}

#[cfg(test)]
mod tests {
    fn simulate_single_entry_scan(matches: &[bool]) -> (usize, usize) {
        let mut current_entry = 0;
        let mut emitted = 0;
        let return_single_entry = true;

        while let Some(file_match) = matches.get(current_entry).copied() {
            let mut emitted_entry = false;
            if file_match {
                emitted += 1;
                emitted_entry = true;
            }

            current_entry += 1;
            if return_single_entry && emitted_entry {
                break;
            }
        }

        (current_entry, emitted)
    }

    #[test]
    fn single_entry_enumeration_skips_non_matches_until_first_emit() {
        let (current_entry, emitted) = simulate_single_entry_scan(&[false, false, true, true]);
        assert_eq!(current_entry, 3);
        assert_eq!(emitted, 1);
    }
}

mod native {
    use std::{
        ffi::{OsString, c_void},
        mem,
        ops::ControlFlow,
        os::windows::ffi::OsStringExt,
        path::PathBuf,
    };

    use windows::{
        Win32::{
            Foundation::{
                BOOLEAN, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_OUTOFMEMORY,
                STATUS_CANNOT_DELETE, STATUS_SUCCESS,
            },
            Storage::ProjectedFileSystem::{
                PRJ_CALLBACK_DATA, PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN,
                PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY, PRJ_DIR_ENTRY_BUFFER_HANDLE,
                PRJ_EXTENDED_INFO, PRJ_FILE_BASIC_INFO, PRJ_NOTIFICATION,
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED,
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED,
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_NO_MODIFICATION, PRJ_NOTIFICATION_FILE_OPENED,
                PRJ_NOTIFICATION_FILE_OVERWRITTEN, PRJ_NOTIFICATION_FILE_PRE_CONVERT_TO_FULL,
                PRJ_NOTIFICATION_FILE_RENAMED, PRJ_NOTIFICATION_HARDLINK_CREATED,
                PRJ_NOTIFICATION_NEW_FILE_CREATED, PRJ_NOTIFICATION_PARAMETERS,
                PRJ_NOTIFICATION_PRE_DELETE, PRJ_NOTIFICATION_PRE_RENAME,
                PRJ_NOTIFICATION_PRE_SET_HARDLINK, PRJ_PLACEHOLDER_INFO,
            },
        },
        core::{GUID, HRESULT, PCWSTR},
    };

    use super::{FileNameU16Cache, RawProjectionContext};
    use crate::{
        DirectoryEntry, FileCloseAction, FileRenameInfo, Notification, ProjectedFile,
        aligned_buffer::PrjAlignedBuffer, utils::io_result_to_hresult,
    };

    impl DirectoryEntry {
        fn get_basic_info(&self) -> PRJ_FILE_BASIC_INFO {
            let mut basic_info = PRJ_FILE_BASIC_INFO::default();

            match self {
                Self::Directory(directory) => {
                    basic_info.IsDirectory = BOOLEAN::from(true);
                    basic_info.FileAttributes = directory.directory_attributes;

                    basic_info.CreationTime = directory.creation_time as i64;
                    basic_info.LastAccessTime = directory.last_access_time as i64;
                    basic_info.LastWriteTime = directory.last_write_time as i64;

                    /* same reasoning as for the file entry */
                    basic_info.ChangeTime = directory.last_write_time as i64;
                }
                Self::File(file) => {
                    basic_info.IsDirectory = BOOLEAN::from(false);

                    basic_info.FileSize = file.file_size as i64;
                    basic_info.FileAttributes = file.file_attributes;

                    basic_info.CreationTime = file.creation_time as i64;
                    basic_info.LastAccessTime = file.last_access_time as i64;
                    basic_info.LastWriteTime = file.last_write_time as i64;

                    /*
                     * ChangeTime includes metadata changes, but we merge these together.
                     * Source: https://web.archive.org/web/20230404085857/https://devblogs.microsoft.com/oldnewthing/20100709-00/?p=13463
                     */
                    basic_info.ChangeTime = file.last_write_time as i64;
                }
            };

            basic_info
        }

        fn get_extended_info(&self) -> Option<PRJ_EXTENDED_INFO> {
            None
        }
    }

    type CallbackData = crate::CallbackData<'static, RawProjectionContext>;
    pub unsafe extern "system" fn start_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let enumeration_id = &*enumeration_id;
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let target = callback_data.file_path.clone().unwrap_or_default();
            tracing::info!(
                target: "vx_vfs::windows",
                "ProjFS start_directory_enumeration id={:X} path={}",
                enumeration_id.to_u128(),
                target.to_string_lossy()
            );
            let mut context = callback_data.context.lock();
            context.register_enumeration(target, enumeration_id.to_u128());

            Ok(())
        })
    }

    pub unsafe extern "system" fn end_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let enumeration_id = &*enumeration_id;
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let mut context = callback_data.context.lock();
            if !context.finish_enumeration(enumeration_id.to_u128()) {
                log::warn!(
                    "Tried to end an non existing enumeration with id {:X}",
                    enumeration_id.to_u128()
                );
            }

            Ok(())
        })
    }

    pub unsafe extern "system" fn get_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
        search_expression: PCWSTR,
        dir_entry_buffer_handle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
    ) -> HRESULT {
        let enumeration_id = &*enumeration_id;
        let callback_data: CallbackData = callback_data.into();
        let search_expression = if search_expression.is_null() {
            None
        } else {
            let mut expression = search_expression.as_wide().to_vec();
            if expression.is_empty() {
                None
            } else {
                /* adding a zero just to ensure it's zero terminated */
                expression.push(0);
                Some(expression)
            }
        };
        let search_expression_text = search_expression.as_ref().map(|expression| {
            let expression = if expression.last() == Some(&0) {
                &expression[..expression.len().saturating_sub(1)]
            } else {
                expression.as_slice()
            };
            OsString::from_wide(expression)
                .to_string_lossy()
                .into_owned()
        });

        callback_data.execute(move |callback_data| {
            let mut context = callback_data.context.lock();
            let library = context.library.clone();

            let enumeration = context
                .directory_enumerations
                .get_mut(&enumeration_id.to_u128())
                /* Return STATUS_SUCCESS to indicate that the enumeration has ended (as it can not be found). */
                .ok_or(STATUS_SUCCESS.to_hresult())?;

            tracing::info!(
                target: "vx_vfs::windows",
                "ProjFS get_directory_enumeration id={:X} path={} flags=0x{:X} search_expression={:?} current_entry={} total_entries={}",
                enumeration_id.to_u128(),
                callback_data.file_path.clone().unwrap_or_default().to_string_lossy(),
                callback_data.flags.0,
                search_expression_text,
                enumeration.current_entry,
                enumeration.entries.len()
            );

            if callback_data.flags.0 & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN.0 > 0 {
                enumeration.reset_enumeration();
            }
            if let Some(search_expression) = search_expression {
                /*
                 * Windows can update the search expression without explicitly
                 * requesting a restart scan. If we keep the old cursor, later
                 * lookups can miss entries that were already consumed by an
                 * earlier enumeration of the same directory.
                 */
                if enumeration.search_expression.as_ref() != Some(&search_expression) {
                    enumeration.reset_enumeration();
                }
                enumeration.search_expression = Some(search_expression);
            } else if enumeration.search_expression.is_some() {
                enumeration.reset_enumeration();
            }

            let return_single_entry =
                callback_data.flags.0 & PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY.0 > 0;
            let name_cache = enumeration.name_cache.clone();
            while let Some(entry) = enumeration.peek_entry() {
                let basic_info = entry.get_basic_info();
                let extended_info = entry.get_extended_info();

                let mut name_cache = name_cache.borrow_mut();
                let name = name_cache.get_or_cache(entry.name().to_string());

                let file_match = if let Some(search_expression) = enumeration.search_expression.as_ref() {
                    unsafe {
                        library.prj_file_name_match(PCWSTR(name.as_ptr()), PCWSTR(search_expression.as_ptr())).as_bool()
                    }
                } else {
                    true
                };

                let mut emitted_entry = false;
                if file_match {
                    tracing::info!(
                        target: "vx_vfs::windows",
                        "ProjFS get_directory_enumeration emit id={:X} path={} entry={} index={} flags=0x{:X}",
                        enumeration_id.to_u128(),
                        callback_data.file_path.clone().unwrap_or_default().to_string_lossy(),
                        entry.name(),
                        enumeration.current_entry,
                        callback_data.flags.0
                    );
                    let result = unsafe {
                        library.prj_fill_dir_entry_buffer2(
                            dir_entry_buffer_handle,
                            PCWSTR(name.as_ptr()),
                            Some(&basic_info),
                            extended_info.map(|v| &v as *const _),
                        )
                    };

                    if let Err(err) = result {
                        if err.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() {
                            /* buffer full */
                            break;
                        }

                        /* unexpected... */
                        return Err(err.code());
                    }

                    emitted_entry = true;
                }

                enumeration.consume_entry();
                if return_single_entry && emitted_entry {
                    break;
                }
            }

            Ok(())
        })
    }

    pub unsafe extern "system" fn get_placeholder_information_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
    ) -> HRESULT {
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let path = callback_data.file_path.clone().unwrap_or_default();

            let context = callback_data.context.lock();
            let entry = context
                .source
                .get_directory_entry(&path)
                .ok_or(ERROR_FILE_NOT_FOUND.to_hresult())?;

            let mut name_cache = FileNameU16Cache::default();
            let name = name_cache.get_or_cache(path.display().to_string());

            let placeholder_info = PRJ_PLACEHOLDER_INFO {
                FileBasicInfo: entry.get_basic_info(),
                ..PRJ_PLACEHOLDER_INFO::default()
            };

            if let Some(extended_info) = entry.get_extended_info() {
                unsafe {
                    context
                        .library
                        .prj_write_placeholder_info2(
                            callback_data.namespace_virtualization_context,
                            PCWSTR(name.as_ptr()),
                            &placeholder_info,
                            mem::size_of_val(&placeholder_info) as u32,
                            Some(&extended_info),
                        )
                        .map_err(|err| err.code())?;
                }
            } else {
                unsafe {
                    context
                        .library
                        .prj_write_placeholder_info(
                            callback_data.namespace_virtualization_context,
                            PCWSTR(name.as_ptr()),
                            &placeholder_info,
                            mem::size_of_val(&placeholder_info) as u32,
                        )
                        .map_err(|err| err.code())?;
                }
            };

            Ok(())
        })
    }

    pub unsafe extern "system" fn get_file_data_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        byte_offset: u64,
        length: u32,
    ) -> HRESULT {
        let length = length as usize;
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let path = callback_data.file_path.clone().unwrap_or_default();

            let context = callback_data.context.lock();
            let mut source = context
                .source
                .stream_file_content(&path, byte_offset as usize, length)
                .map_err(io_result_to_hresult)?;

            let chunk_length = if length <= 1024 * 1024 {
                length
            } else {
                1024 * 1024
            };

            let mut buffer = PrjAlignedBuffer::allocate(
                context.library.clone(),
                callback_data.namespace_virtualization_context,
                chunk_length,
            )
            .ok_or(ERROR_OUTOFMEMORY.to_hresult())?;
            let buffer = buffer.buffer();

            let mut bytes_written = 0;
            while bytes_written < length {
                let bytes_pending = length - bytes_written;
                let chunk_length = bytes_pending.min(buffer.len());

                source
                    .read_exact(&mut buffer[0..chunk_length])
                    .map_err(io_result_to_hresult)?;

                let write_result = unsafe {
                    context.library.prj_write_file_data(
                        callback_data.namespace_virtualization_context,
                        &callback_data.data_stream_id,
                        buffer.as_ptr() as *const c_void,
                        byte_offset + bytes_written as u64,
                        chunk_length as u32,
                    )
                };
                if let Err(err) = write_result {
                    log::warn!(
                        "Failed to write projected file data for {}: {}",
                        path.display(),
                        err
                    );
                    return Err(err.code());
                }

                bytes_written += chunk_length;
            }

            Ok(())
        })
    }

    pub unsafe extern "system" fn notification_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        is_directory: BOOLEAN,
        notification: PRJ_NOTIFICATION,
        destination_filename: PCWSTR,
        _operation_parameters: *mut PRJ_NOTIFICATION_PARAMETERS,
    ) -> HRESULT {
        let callback_data: CallbackData = callback_data.into();

        let destination_filename = if destination_filename.is_null() {
            None
        } else {
            Some(PathBuf::from(OsString::from_wide(
                destination_filename.as_wide(),
            )))
        };

        callback_data.execute(move |callback_data| {
            let target_file = ProjectedFile {
                file_id: callback_data.file_id.to_u128(),
                is_directory: is_directory.as_bool(),
                path: callback_data.file_path.clone().unwrap_or_default(),
            };

            let notification = match notification {
                PRJ_NOTIFICATION_NEW_FILE_CREATED => Notification::FileCreated(target_file),
                PRJ_NOTIFICATION_FILE_OPENED => Notification::FileOpened(target_file),
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED => {
                    Notification::FileClosed(target_file, FileCloseAction::Deleted)
                }
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED => {
                    Notification::FileClosed(target_file, FileCloseAction::Modified)
                }
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_NO_MODIFICATION => {
                    Notification::FileClosed(target_file, FileCloseAction::NoModification)
                }
                PRJ_NOTIFICATION_FILE_OVERWRITTEN => Notification::FileOverwritten(target_file),

                PRJ_NOTIFICATION_PRE_RENAME => Notification::PreFileRename(FileRenameInfo {
                    source: callback_data.file_path.clone(),
                    destination: destination_filename,
                }),
                PRJ_NOTIFICATION_FILE_RENAMED => Notification::FileRenamed(FileRenameInfo {
                    source: callback_data.file_path.clone(),
                    destination: destination_filename,
                }),

                PRJ_NOTIFICATION_PRE_SET_HARDLINK => Notification::PreSetHardlink(target_file),
                PRJ_NOTIFICATION_HARDLINK_CREATED => Notification::HardlinkCreated(target_file),

                PRJ_NOTIFICATION_FILE_PRE_CONVERT_TO_FULL => {
                    Notification::FilePreConvertToFull(target_file)
                }
                PRJ_NOTIFICATION_PRE_DELETE => Notification::PreFileDelete(target_file),

                notification => {
                    log::warn!("Invalid notification {}", notification.0);
                    return Ok(());
                }
            };

            let context = callback_data.context.lock();
            let action = context.source.handle_notification(&notification);
            if matches!(action, ControlFlow::Break(_)) {
                if notification.is_cancelable() {
                    return Err(STATUS_CANNOT_DELETE.to_hresult());
                }

                log::warn!(
                    "Tried to cancel a non cancelable action: {:?}",
                    notification
                );
            }

            Ok(())
        })
    }
}
