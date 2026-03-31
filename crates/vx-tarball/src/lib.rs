//! Tarball extraction
//!
//! Provides streaming tar.xz and tar.gz extraction from any AsyncRead source.
//!
//! ## Tree extraction (CAS deduplication)
//!
//! For content-addressed storage, use [`extract_to_tree`] to extract a tarball
//! into a [`TreeManifest`], storing each file as a separate blob. This enables:
//! - Deduplication across toolchain versions
//! - Deduplication across crates
//! - Efficient partial materialization

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use std::future::Future;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWriteExt};
use vx_services::{BlobRef, ContentHash};

#[derive(Debug, thiserror::Error)]
pub enum TarballError {
    #[error("decompression failed: {0}")]
    Decompression(#[from] std::io::Error),

    #[error("tar error: {0}")]
    Tar(String),

    #[error("non-UTF8 path in tarball")]
    NonUtf8Path,

    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: Utf8PathBuf,
        source: std::io::Error,
    },

    #[error("failed to create file {path}: {source}")]
    CreateFile {
        path: Utf8PathBuf,
        source: std::io::Error,
    },

    #[error("failed to write file {path}: {source}")]
    WriteFile {
        path: Utf8PathBuf,
        source: std::io::Error,
    },

    #[error("failed to set permissions on {path}: {source}")]
    SetPermissions {
        path: Utf8PathBuf,
        source: std::io::Error,
    },

    #[error("failed to store blob: {0}")]
    BlobStore(String),

    #[error("failed to read entry data: {0}")]
    ReadEntry(String),
}

/// Result returned by streamed regular-file extraction callbacks.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    pub content_hash: ContentHash,
    pub blob_ref: BlobRef,
    pub size: u64,
}

pub type OwnedFileReader = Box<dyn AsyncRead + Unpin + Send>;
pub type FileStreamCallbackFuture =
    Pin<Box<dyn Future<Output = Result<ExtractedFile, String>> + Send>>;

/// Compression format for tarballs
#[derive(Debug, Clone, Copy)]
pub enum Compression {
    /// xz compression (tar.xz)
    Xz,
    /// gzip compression (tar.gz, .crate files)
    Gzip,
}

/// Extract a tarball from any AsyncRead source.
///
/// Entries with path traversal (`..`) are silently skipped.
///
/// # Arguments
/// * `reader` - Any async reader (HTTP response, CAS stream, file, etc.)
/// * `dest` - Destination directory
/// * `compression` - Compression format (Xz or Gzip)
/// * `strip_components` - Number of leading path components to strip
// TODO: extract in parallel
pub async fn extract<R: AsyncRead + Unpin>(
    reader: R,
    dest: &Utf8Path,
    compression: Compression,
    strip_components: u32,
) -> Result<(), TarballError> {
    // Create decompressor based on compression type
    match compression {
        Compression::Xz => {
            let decoder = async_compression::tokio::bufread::XzDecoder::new(
                tokio::io::BufReader::new(reader),
            );
            extract_tar(decoder, dest, strip_components).await
        }
        Compression::Gzip => {
            let decoder = async_compression::tokio::bufread::GzipDecoder::new(
                tokio::io::BufReader::new(reader),
            );
            extract_tar(decoder, dest, strip_components).await
        }
    }
}

async fn extract_tar<R: AsyncRead + Unpin>(
    reader: R,
    dest: &Utf8Path,
    strip_components: u32,
) -> Result<(), TarballError> {
    let mut archive = tokio_tar::Archive::new(reader);
    let mut entries = archive
        .entries()
        .map_err(|e| TarballError::Tar(e.to_string()))?;

    use futures_util::StreamExt;
    while let Some(entry_result) = entries.next().await {
        let mut entry = entry_result.map_err(|e| TarballError::Tar(e.to_string()))?;

        let path = entry.path().map_err(|e| TarballError::Tar(e.to_string()))?;

        // Strip components
        let components: Vec<_> = path.components().collect();
        if components.len() <= strip_components as usize {
            continue;
        }

        let stripped_path = Utf8PathBuf::from_path_buf(
            components[strip_components as usize..]
                .iter()
                .collect::<std::path::PathBuf>(),
        )
        .map_err(|_| TarballError::NonUtf8Path)?;

        // Skip entries with path traversal
        let has_parent_dir = stripped_path
            .components()
            .any(|c| matches!(c, Utf8Component::ParentDir));
        if has_parent_dir {
            continue;
        }

        let target_path = dest.join(&stripped_path);

        // Handle directories
        if entry.header().entry_type().is_dir() {
            moire::fs::create_dir_all(&target_path)
                .await
                .map_err(|e| TarballError::CreateDir {
                    path: target_path.clone(),
                    source: e,
                })?;
            continue;
        }

        // Create parent directories
        if let Some(parent) = target_path.parent() {
            moire::fs::create_dir_all(parent)
                .await
                .map_err(|e| TarballError::CreateDir {
                    path: parent.to_owned(),
                    source: e,
                })?;
        }

        // Extract file
        let mut output_file =
            moire::fs::File::create(&target_path)
                .await
                .map_err(|e| TarballError::CreateFile {
                    path: target_path.clone(),
                    source: e,
                })?;

        tokio::io::copy(&mut entry, &mut output_file)
            .await
            .map_err(|e| TarballError::WriteFile {
                path: target_path.clone(),
                source: e,
            })?;

        // Set executable bit if needed
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mode) = entry.header().mode()
                && mode & 0o111 != 0
            {
                let perms = std::fs::Permissions::from_mode(mode);
                moire::fs::set_permissions(&target_path, perms)
                    .await
                    .map_err(|e| TarballError::SetPermissions {
                        path: target_path.clone(),
                        source: e,
                    })?;
            }
        }
    }

    Ok(())
}

// =============================================================================
// Tree Extraction (CAS Deduplication)
// =============================================================================

/// Extract a tarball to a TreeManifest, storing each regular file via a streaming callback.
///
/// Instead of buffering each file entry in memory, this function:
/// 1. Streams each regular file body directly from the tar entry
/// 2. Calls `store_blob` while the entry is still being read
/// 3. Records the path → content-hash mapping in a TreeManifest
/// 4. Returns both the TreeManifest and the per-file streamed blob results
///
/// # Arguments
/// * `reader` - Any async reader (HTTP response, CAS stream, file, etc.)
/// * `compression` - Compression format (Xz or Gzip)
/// * `strip_components` - Number of leading path components to strip
/// * `store_blob` - Async callback that consumes a streamed tar entry body
///
/// # Returns
/// A TreeManifest plus the per-file streamed blob results in path order
pub async fn extract_to_tree<R, F>(
    reader: R,
    compression: Compression,
    strip_components: u32,
    store_blob: F,
) -> Result<(TreeManifest, Vec<(String, ExtractedFile, bool)>), TarballError>
where
    R: AsyncRead + Unpin + Send,
    F: Fn(String, OwnedFileReader, bool) -> FileStreamCallbackFuture,
{
    match compression {
        Compression::Xz => {
            let decoder = async_compression::tokio::bufread::XzDecoder::new(
                tokio::io::BufReader::new(reader),
            );
            extract_tar_to_tree(decoder, strip_components, store_blob).await
        }
        Compression::Gzip => {
            let decoder = async_compression::tokio::bufread::GzipDecoder::new(
                tokio::io::BufReader::new(reader),
            );
            extract_tar_to_tree(decoder, strip_components, store_blob).await
        }
    }
}

async fn extract_tar_to_tree<R, F>(
    reader: R,
    strip_components: u32,
    store_blob: F,
) -> Result<(TreeManifest, Vec<(String, ExtractedFile, bool)>), TarballError>
where
    R: AsyncRead + Unpin + Send,
    F: Fn(String, OwnedFileReader, bool) -> FileStreamCallbackFuture,
{
    let mut manifest = TreeManifest::new();
    let mut extracted_files = Vec::new();
    let mut pending_files = Vec::new();
    let mut archive = tokio_tar::Archive::new(reader);
    let mut entries = archive
        .entries()
        .map_err(|e| TarballError::Tar(e.to_string()))?;

    // Track non-empty directories (we only need to record empty ones)
    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

    use futures_util::StreamExt;
    while let Some(entry_result) = entries.next().await {
        let mut entry = entry_result.map_err(|e| TarballError::Tar(e.to_string()))?;

        let path = entry.path().map_err(|e| TarballError::Tar(e.to_string()))?;

        // Strip components
        let components: Vec<_> = path.components().collect();
        if components.len() <= strip_components as usize {
            continue;
        }

        let stripped_path = Utf8PathBuf::from_path_buf(
            components[strip_components as usize..]
                .iter()
                .collect::<std::path::PathBuf>(),
        )
        .map_err(|_| TarballError::NonUtf8Path)?;

        // Skip entries with path traversal
        let has_parent_dir = stripped_path
            .components()
            .any(|c| matches!(c, Utf8Component::ParentDir));
        if has_parent_dir {
            continue;
        }

        // Use forward slashes for consistency
        let path_str = stripped_path.as_str().replace('\\', "/");

        // Mark parent directories as non-empty
        if let Some(parent) = stripped_path.parent() {
            let parent_str = parent.as_str().replace('\\', "/");
            if !parent_str.is_empty() {
                seen_dirs.insert(parent_str);
            }
        }

        let entry_type = entry.header().entry_type();

        if entry_type.is_dir() {
            // We'll handle empty dirs at the end
            continue;
        }

        if entry_type.is_symlink() {
            // Handle symlinks
            let target = entry
                .link_name()
                .map_err(|e| TarballError::Tar(e.to_string()))?
                .ok_or_else(|| TarballError::Tar("symlink without target".to_string()))?;

            let target_str = target
                .to_str()
                .ok_or(TarballError::NonUtf8Path)?
                .to_string();

            manifest.add_symlink(path_str, target_str);
            continue;
        }

        if entry_type.is_file() || entry_type.is_hard_link() {
            let executable = entry
                .header()
                .mode()
                .map(|m| m & 0o111 != 0)
                .unwrap_or(false);
            let (mut writer, reader) = tokio::io::duplex(256 * 1024);
            let path_for_task = path_str.clone();
            let callback = store_blob(path_for_task.clone(), Box::new(reader), executable);

            pending_files.push(tokio::spawn(async move {
                let extracted = callback.await.map_err(TarballError::BlobStore)?;
                Ok::<_, TarballError>((path_for_task, extracted, executable))
            }));

            tokio::io::copy(&mut entry, &mut writer)
                .await
                .map_err(|error| TarballError::ReadEntry(error.to_string()))?;
            writer
                .shutdown()
                .await
                .map_err(|error| TarballError::ReadEntry(error.to_string()))?;
        }
        // Skip other entry types (devices, fifos, etc.)
    }

    for pending in pending_files {
        let (path_str, extracted, executable) = pending
            .await
            .map_err(|error| TarballError::ReadEntry(error.to_string()))??;
        manifest.add_file(
            path_str.clone(),
            extracted.content_hash.0,
            extracted.size,
            executable,
        );
        extracted_files.push((path_str, extracted, executable));
    }

    // Finalize: sort entries and compute stats
    manifest.finalize();
    extracted_files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok((manifest, extracted_files))
}

#[cfg(test)]
mod tests;
