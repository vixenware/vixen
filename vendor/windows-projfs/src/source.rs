use std::{
    self,
    ffi::OsStr,
    fs::DirEntry,
    io::{self, Read},
    ops::ControlFlow,
    path::{Path, PathBuf},
};

/// A `DirectoryEntry` represents all possible entry types
/// which can be contained within the file system.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DirectoryEntry {
    /// The entry is a directory
    Directory(DirectoryInfo),

    /// The entry is a single file
    File(FileInfo),
}

impl DirectoryEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Directory(dir) => &dir.directory_name,
            Self::File(file) => &file.file_name,
        }
    }
}

impl From<FileInfo> for DirectoryEntry {
    fn from(value: FileInfo) -> Self {
        Self::File(value)
    }
}

impl From<DirectoryInfo> for DirectoryEntry {
    fn from(value: DirectoryInfo) -> Self {
        Self::Directory(value)
    }
}

impl TryFrom<DirEntry> for DirectoryEntry {
    type Error = std::io::Error;

    fn try_from(value: DirEntry) -> Result<Self, Self::Error> {
        use std::os::windows::fs::MetadataExt;

        let file_name = value.file_name().to_string_lossy().to_string();
        let file_type = value.file_type()?;
        let metadata = value.metadata()?;
        if file_type.is_dir() {
            Ok(DirectoryInfo {
                directory_name: file_name,
                directory_attributes: metadata.file_attributes(),

                creation_time: metadata.creation_time(),
                last_access_time: metadata.last_access_time(),
                last_write_time: metadata.last_write_time(),
            }
            .into())
        } else if file_type.is_file() {
            Ok(FileInfo {
                file_name,
                file_size: metadata.len(),
                file_attributes: metadata.file_attributes(),

                creation_time: metadata.creation_time(),
                last_access_time: metadata.last_access_time(),
                last_write_time: metadata.last_write_time(),
            }
            .into())
        } else {
            Err(io::Error::other("file type is not supported"))
        }
    }
}

/// Supported attributes for files.
///
/// Note:
/// The file size should be matching else the client might expect more
/// or less content when trying to receive the file.
#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileInfo {
    pub file_name: String,
    pub file_size: u64,
    pub file_attributes: u32,

    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
}

/// Supported attributes for directories
#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DirectoryInfo {
    pub directory_name: String,
    pub directory_attributes: u32,

    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
}

/// Implementation for the data source of the projected file system.
pub trait ProjectedFileSystemSource {
    /// Return a list of directory entries contained at that specific path.
    /// Return an empty list to indicate that the directory is empty or does not exists.
    fn list_directory(&self, path: &Path) -> Vec<DirectoryEntry>;

    /// Return information about the target path.  
    /// The path can be any of the previously returned `DirectoryEntry`s.  
    ///  
    /// If the target entry does not exists, return `None`.  
    ///
    /// Note:  
    /// The default implementation is for convinience and should be overridden as  
    /// looping trough all directory entries might come with a performance penalty.
    fn get_directory_entry(&self, path: &Path) -> Option<DirectoryEntry> {
        let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let file_name = path.file_name().map(OsStr::to_string_lossy)?;

        self.list_directory(&directory)
            .into_iter()
            .find(|entry| entry.name() == file_name)
    }

    /// Return a stream to the file contents of `path`.  
    ///   
    /// Note:
    /// The returned Box<dyn Read> must respect the byte_offset and will not be read  
    /// past `length` bytes.
    fn stream_file_content(
        &self,
        path: &Path,
        byte_offset: usize,
        length: usize,
    ) -> std::io::Result<Box<dyn Read>>;

    /// Handle file system notifications.
    /// All pre-notifications can be cancelled.
    fn handle_notification(&self, _notification: &Notification) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileCloseAction {
    /// File has been closed and deleted
    Deleted,

    /// File has been close and the contents modified
    Modified,

    /// File has been closed but the contents have not changed
    NoModification,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectedFile {
    pub file_id: u128,
    pub is_directory: bool,
    pub path: PathBuf,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileRenameInfo {
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Notification {
    FileCreated(ProjectedFile),
    FileOpened(ProjectedFile),
    FileClosed(ProjectedFile, FileCloseAction),
    FileOverwritten(ProjectedFile),

    PreFileRename(FileRenameInfo),
    FileRenamed(FileRenameInfo),

    PreSetHardlink(ProjectedFile),
    HardlinkCreated(ProjectedFile),

    PreFileDelete(ProjectedFile),
    FilePreConvertToFull(ProjectedFile),
}

impl Notification {
    /// Returns `true` if the action can be cancelled  
    /// by returning `ControlFlow::Break`
    pub fn is_cancelable(&self) -> bool {
        #[allow(clippy::match_like_matches_macro)]
        match self {
            Self::PreFileRename(_) => true,
            Self::PreFileDelete(_) => true,
            Self::PreSetHardlink(_) => true,
            Self::FilePreConvertToFull(_) => true,
            _ => false,
        }
    }
}
