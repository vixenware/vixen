use std::{collections::BTreeMap, fs, io, path::PathBuf};

use tempdir::TempDir;
use windows_projfs::{
    DirectoryEntry, DirectoryInfo, FileInfo, ProjectedFileSystem, ProjectedFileSystemSource,
};

#[derive(Debug, Default)]
struct TestProjectionSource {
    entries: BTreeMap<PathBuf, Vec<DirectoryEntry>>,
}

impl ProjectedFileSystemSource for TestProjectionSource {
    fn list_directory(&self, path: &std::path::Path) -> Vec<DirectoryEntry> {
        self.entries.get(path).cloned().unwrap_or_default()
    }
    fn stream_file_content(
        &self,
        _path: &std::path::Path,
        _byte_offset: usize,
        _length: usize,
    ) -> std::io::Result<Box<dyn std::io::prelude::Read>> {
        panic!("file contents should not be requested")
    }
}

#[test]
fn directory_metadata() -> anyhow::Result<()> {
    let _ = env_logger::try_init();

    let target_dir = TempDir::new("test_directory_entries")?;
    let target_dir = target_dir.path();

    let mut pfs_source = TestProjectionSource::default();
    pfs_source.entries.insert(
        PathBuf::from(""),
        vec![
            DirectoryInfo {
                directory_name: "Test-A".to_string(),
                directory_attributes: 4456464,

                creation_time: 133482410013464001,
                last_access_time: 133482410013464002,
                last_write_time: 133482410013464003,
            }
            .into(),
            DirectoryInfo {
                directory_name: "Test-B".to_string(),
                directory_attributes: 4456464,

                creation_time: 133482410013464001,
                last_access_time: 133482410013464002,
                last_write_time: 133482410013464003,
            }
            .into(),
            FileInfo {
                file_name: "My_File.txt".to_string(),
                file_attributes: 4456448,
                file_size: 667,

                creation_time: 133482410012464001,
                last_access_time: 133482410012464002,
                last_write_time: 133482410012464003,
            }
            .into(),
        ],
    );
    pfs_source.entries.insert(
        PathBuf::from("Test-A"),
        vec![
            FileInfo {
                file_name: "In-A File.txt".to_string(),
                file_attributes: 4456448,
                file_size: 123,

                creation_time: 133482410012464011,
                last_access_time: 133482410012464012,
                last_write_time: 133482410012464013,
            }
            .into(),
        ],
    );
    pfs_source.entries.insert(
        PathBuf::from("Test-B"),
        vec![
            DirectoryInfo {
                directory_name: "Test-C".to_string(),
                directory_attributes: 4456464,

                creation_time: 133482410013464001,
                last_access_time: 133482410013464002,
                last_write_time: 133482410013464003,
            }
            .into(),
        ],
    );
    pfs_source.entries.insert(
        PathBuf::from("Test-B/Test-C"),
        vec![
            FileInfo {
                file_name: "This_File_in_B_C.txt".to_string(),
                file_attributes: 4456448,
                file_size: 333,

                creation_time: 133482410012464021,
                last_access_time: 133482410012464022,
                last_write_time: 133482410012464023,
            }
            .into(),
        ],
    );

    let entries = pfs_source.entries.clone();
    let _pfs = ProjectedFileSystem::new(target_dir, pfs_source)?;

    assert!(target_dir.exists());
    assert!(target_dir.is_dir());

    for path in [
        PathBuf::from(""),
        PathBuf::from("Test-A"),
        PathBuf::from("Test-B"),
        PathBuf::from("Test-B/Test-C"),
    ] {
        let mut entries_found = fs::read_dir(target_dir.join(&path))?
            .map(|e| e?.try_into())
            .collect::<io::Result<Vec<DirectoryEntry>>>()?;
        entries_found.sort();

        let mut entries_expected = entries.get(&path).cloned().unwrap_or_default();
        entries_expected.sort();

        assert_eq!(entries_found.len(), entries_expected.len());
        assert_eq!(entries_found, entries_expected);
    }
    Ok(())
}
