//! Common I/O utilities for vixen crates

use camino::{Utf8Path, Utf8PathBuf};
use tokio::io::AsyncWriteExt;

pub mod boot;
pub mod git;
pub mod net;
pub mod spawn;

/// Resolve the VX_HOME directory: `$VX_HOME` if set, otherwise `~/.vx`.
pub fn vx_home() -> Utf8PathBuf {
    match std::env::var("VX_HOME") {
        Ok(v) => Utf8PathBuf::from(v),
        Err(_) => {
            let home = dirs::home_dir().expect("could not determine home directory");
            Utf8PathBuf::from(home.join(".vx").to_string_lossy().into_owned())
        }
    }
}

/// Atomically write contents to a file.
///
/// Creates a temporary file in the same directory, writes contents,
/// then atomically renames to the final path without clobbering an
/// existing file. `AlreadyExists` is treated as success.
pub async fn atomic_write(
    path: &Utf8Path,
    contents: &[u8],
    executable: bool,
) -> Result<(), std::io::Error> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent_dir = path.parent().unwrap_or_else(|| Utf8Path::new("."));

    // Create parent directory if it doesn't exist
    moire::fs::create_dir_all(parent_dir).await?;

    let temp_path = loop {
        let temp_name = format!(
            ".atomic-write-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        );
        let candidate = parent_dir.join(temp_name);
        let mut opts = moire::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        match opts.open(candidate.as_std_path()).await {
            Ok(mut file) => {
                file.write_all(contents).await?;
                file.sync_all().await?;
                drop(file);
                break candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };

    // Set executable bit if requested.
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = moire::fs::metadata(&temp_path).await?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        moire::fs::set_permissions(&temp_path, perms).await?;
    }

    // Atomically move temp file to final path. rename() on the same filesystem is
    // atomic and overwrites any existing target, which is fine for our use case.
    match moire::fs::rename(temp_path.as_std_path(), path.as_std_path()).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match moire::fs::read(path.as_std_path()).await {
                Ok(existing) if existing == contents => {
                    let _ = moire::fs::remove_file(temp_path.as_std_path()).await;
                    Ok(())
                }
                Ok(_) => {
                    moire::fs::remove_file(path.as_std_path()).await?;
                    moire::fs::rename(temp_path.as_std_path(), path.as_std_path()).await
                }
                Err(read_error) if read_error.kind() == std::io::ErrorKind::NotFound => {
                    moire::fs::rename(temp_path.as_std_path(), path.as_std_path()).await
                }
                Err(read_error) => {
                    let _ = moire::fs::remove_file(temp_path.as_std_path()).await;
                    Err(read_error)
                }
            }
        }
        Err(error) => {
            let _ = moire::fs::remove_file(temp_path.as_std_path()).await;
            Err(error)
        }
    }
}

/// Create a unique scratch directory under the given base path.
///
/// The directory name is derived from timestamp + PID + atomic counter,
/// ensuring uniqueness even under concurrent access.
///
/// The caller is responsible for cleaning up the directory when done.
pub async fn create_scratch_dir(base: &Utf8Path) -> Result<Utf8PathBuf, std::io::Error> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let scratch_base = base.join("scratch");
    moire::fs::create_dir_all(&scratch_base).await?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("{:x}-{}-{}", timestamp, pid, count);
    let dir = scratch_base.join(&id);

    moire::fs::create_dir_all(&dir).await?;

    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::atomic_write;
    use camino::Utf8PathBuf;

    #[tokio::test]
    async fn atomic_write_persists_bytes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("node.bin")).expect("utf8");
        let contents = b"hello atomic write";

        atomic_write(&path, contents, false)
            .await
            .expect("atomic_write");

        let actual = moire::fs::read(path.as_std_path()).await.expect("read");
        assert_eq!(actual, contents);
    }

    #[tokio::test]
    async fn atomic_write_repairs_corrupt_existing_bytes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("node.bin")).expect("utf8");

        moire::fs::write(path.as_std_path(), b"\0\0\0\0")
            .await
            .expect("seed corrupt bytes");

        let contents = b"hello atomic write";
        atomic_write(&path, contents, false)
            .await
            .expect("atomic_write");

        let actual = moire::fs::read(path.as_std_path()).await.expect("read");
        assert_eq!(actual, contents);
    }
}
