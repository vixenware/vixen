//! Tarball extraction

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use std::future::Future;
use std::pin::Pin;
use tokio::io::AsyncRead;
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
