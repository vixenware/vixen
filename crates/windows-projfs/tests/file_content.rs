use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{self, Cursor, ErrorKind},
    path::PathBuf,
};

use tempdir::TempDir;
use windows_projfs::{DirectoryInfo, FileInfo, ProjectedFileSystem, ProjectedFileSystemSource};

#[derive(Debug, Default)]
struct TestProjectionSource {
    content: BTreeMap<PathBuf, Vec<u8>>,
}

impl ProjectedFileSystemSource for TestProjectionSource {
    fn list_directory(&self, target: &std::path::Path) -> Vec<windows_projfs::DirectoryEntry> {
        self.content
            .iter()
            .filter(|(path, _)| path.starts_with(target))
            .map(|(path, value)| {
                let sub_path = path.strip_prefix(target).unwrap();
                let mut components = sub_path
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().to_string())
                    .collect::<VecDeque<_>>();

                if components.len() == 1 {
                    FileInfo {
                        file_name: components.pop_front().unwrap(),
                        file_size: value.len() as u64,

                        ..Default::default()
                    }
                    .into()
                } else {
                    DirectoryInfo {
                        directory_name: components.pop_front().unwrap(),

                        ..Default::default()
                    }
                    .into()
                }
            })
            .collect()
    }

    fn stream_file_content(
        &self,
        path: &std::path::Path,
        byte_offset: usize,
        length: usize,
    ) -> std::io::Result<Box<dyn std::io::prelude::Read>> {
        let content = match self.content.get(path) {
            Some(content) => content,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "target file not found",
                ));
            }
        };

        if byte_offset + length > content.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "invalid read operation",
            ));
        }

        Ok(Box::new(Cursor::new(
            content[byte_offset..(byte_offset + length)].to_owned(),
        )))
    }
}

#[test]
fn small_file_content() -> anyhow::Result<()> {
    let _ = env_logger::try_init();

    let target_dir = TempDir::new("test_file_content")?;
    let target_dir = target_dir.path();

    let mut pfs_source = TestProjectionSource::default();
    pfs_source.content.insert(
        PathBuf::from("test_file.txt"),
        "Hello World!".as_bytes().to_vec(),
    );

    pfs_source.content.insert(
        PathBuf::from("sub-dir/test_file.txt"),
        "Hows ya doing... Does everything work?".as_bytes().to_vec(),
    );

    let content = pfs_source.content.clone();
    let _pfs = ProjectedFileSystem::new(target_dir, pfs_source)?;

    for (path, expected_content) in content {
        let read_content = fs::read(target_dir.join(path))?;
        assert_eq!(read_content, expected_content);
    }
    Ok(())
}

#[test]
fn file_not_found() -> anyhow::Result<()> {
    let _ = env_logger::try_init();

    let target_dir = TempDir::new("test_file_not_found")?;
    let target_dir = target_dir.path();

    let mut pfs_source = TestProjectionSource::default();
    pfs_source.content.insert(
        PathBuf::from("file_existing.txt"),
        "Some random content!".as_bytes().to_vec(),
    );
    pfs_source.content.insert(
        PathBuf::from("sub-dir/file_existing.txt"),
        "Some random content!".as_bytes().to_vec(),
    );

    let _pfs = ProjectedFileSystem::new(target_dir, pfs_source)?;
    assert!(fs::read(target_dir.join("file_existing.txt")).is_ok());
    assert!(fs::read(target_dir.join("sub-dir/file_existing.txt")).is_ok());
    assert!(fs::read(target_dir.join("file_not_existing.txt")).is_err());
    assert!(fs::read(target_dir.join("sub-dir-x/file_existing.txt")).is_err());

    match fs::read_to_string(target_dir.join("file_not_existing.txt")) {
        Err(err) => {
            assert_eq!(err.kind(), ErrorKind::NotFound)
        }
        _ => unreachable!(),
    }

    match fs::read_to_string(target_dir.join("sub-dir-x/file_existing.txt")) {
        Err(err) => {
            assert_eq!(err.kind(), ErrorKind::NotFound)
        }
        _ => unreachable!(),
    }

    Ok(())
}

#[test]
fn file_content_large() -> anyhow::Result<()> {
    const CONTENT_LENGTH: usize = 1024 * 1024 * 64 + 766;
    let _ = env_logger::try_init();

    let target_dir = TempDir::new("test_file_content_large")?;
    let target_dir = target_dir.path();

    let mut file_content = Vec::with_capacity(CONTENT_LENGTH);
    for index in 0..CONTENT_LENGTH {
        file_content.push((index & 0xFF) as u8);
    }

    let mut pfs_source = TestProjectionSource::default();
    pfs_source
        .content
        .insert(PathBuf::from("large_file.bin"), file_content.clone());

    let _pfs = ProjectedFileSystem::new(target_dir, pfs_source)?;

    let read_content = fs::read(target_dir.join("large_file.bin"))?;
    for index in 0..CONTENT_LENGTH {
        if read_content[index] != file_content[index] {
            panic!(
                "large file content miss match at {}. Expected {:X}, Received {:X}",
                index, file_content[index], read_content[index]
            );
        }
    }

    Ok(())
}
