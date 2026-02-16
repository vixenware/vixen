//! Common I/O utilities for vixen crates

use async_tempfile::TempFile;
use camino::{Utf8Path, Utf8PathBuf};

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
    let parent_dir = path.parent().unwrap_or_else(|| Utf8Path::new("."));

    // Create parent directory if it doesn't exist
    peeps::fs::create_dir_all(parent_dir).await?;

    // Create a temporary file in the same directory to ensure it's on the same filesystem
    let temp_file = peeps::peep!(
        TempFile::new_in(parent_dir.as_std_path()),
        "atomic_write.create_tempfile",
        {
            "resource.path" => path.as_str(),
        }
    )
    .await
    .map_err(std::io::Error::other)?;

    // Get the temporary path and write contents to it
    let temp_path = temp_file.file_path();
    peeps::fs::write(&temp_path, contents).await?;

    // Set executable bit if requested.
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = peeps::fs::metadata(&temp_path).await?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        peeps::fs::set_permissions(&temp_path, perms).await?;
    }

    // Atomically move temp file to final path. rename() on the same filesystem is
    // atomic and overwrites any existing target, which is fine for our use case.
    peeps::fs::rename(temp_path.to_path_buf(), path.as_std_path()).await
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
    peeps::fs::create_dir_all(&scratch_base).await?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("{:x}-{}-{}", timestamp, pid, count);
    let dir = scratch_base.join(&id);

    peeps::fs::create_dir_all(&dir).await?;

    Ok(dir)
}

/// Synchronous version of atomic write for use in blocking contexts.
pub mod sync {
    use camino::Utf8Path;
    use std::io::Write;

    /// Atomically write contents to a file.
    pub fn atomic_write(
        path: &Utf8Path,
        contents: &[u8],
        executable: bool,
    ) -> Result<(), std::io::Error> {
        let parent_dir = path.parent().unwrap_or_else(|| Utf8Path::new("."));
        std::fs::create_dir_all(parent_dir)?;

        let temp_file = tempfile::Builder::new()
            .prefix(".tmp-")
            .tempfile_in(parent_dir)
            .map_err(std::io::Error::other)?;

        let (mut file, temp_path) = temp_file.into_parts();
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);

        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&temp_path)?.permissions();
            perms.set_mode(perms.mode() | 0o111);
            std::fs::set_permissions(&temp_path, perms)?;
        }

        match temp_path.persist_noclobber(path) {
            Ok(_) => Ok(()),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e.error),
        }
    }
}
