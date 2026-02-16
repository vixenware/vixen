//! Zig toolchain acquisition and management
//!
//! Downloads and manages Zig toolchains for hermetic C/C++ compilation.
//! Uses `zig cc` as a drop-in hermetic C/C++ compiler.

use camino::{Utf8Path, Utf8PathBuf};
use thiserror::Error;
use vx_store_proto::Blake3Hash;

/// Errors that can occur during Zig toolchain operations
#[derive(Debug, Error)]
pub enum ZigError {
    #[error("failed to fetch from {url}: {source}")]
    FetchError {
        url: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("unsupported host platform: {os}-{arch}")]
    UnsupportedPlatform { os: String, arch: String },

    #[error("checksum mismatch for {url}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        url: String,
        expected: String,
        actual: String,
    },

    #[error("extraction failed: {0}")]
    ExtractionFailed(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("zig executable not found in archive")]
    ZigNotFound,

    #[error("zig lib directory not found in archive")]
    LibNotFound,
}

/// Zig version specification
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ZigVersion {
    /// Version string (e.g., "0.13.0", "0.14.0-dev.1234+abcdef")
    pub version: String,
}

impl ZigVersion {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
        }
    }

    /// Check if this is a release version (not dev/nightly)
    pub fn is_release(&self) -> bool {
        !self.version.contains("-dev")
    }
}

impl std::fmt::Display for ZigVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.version)
    }
}

/// Host platform for the Zig toolchain
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostPlatform {
    /// OS: "linux", "macos", "windows"
    pub os: String,
    /// Architecture: "x86_64", "aarch64"
    pub arch: String,
}

impl HostPlatform {
    /// Detect the current host platform
    pub fn detect() -> Result<Self, ZigError> {
        let os = match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "macos",
            "windows" => "windows",
            other => {
                return Err(ZigError::UnsupportedPlatform {
                    os: other.to_string(),
                    arch: std::env::consts::ARCH.to_string(),
                });
            }
        };

        let arch = match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            other => {
                return Err(ZigError::UnsupportedPlatform {
                    os: os.to_string(),
                    arch: other.to_string(),
                });
            }
        };

        Ok(Self {
            os: os.to_string(),
            arch: arch.to_string(),
        })
    }

    /// Get the Zig download filename component for this platform
    pub fn zig_platform_name(&self) -> String {
        // Zig uses OS-ARCH format (e.g., "macos-aarch64", "linux-x86_64")
        format!("{}-{}", self.os, self.arch)
    }
}

impl std::fmt::Display for HostPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.arch, self.os)
    }
}

/// A downloaded and verified Zig toolchain
#[derive(Debug, Clone)]
pub struct ZigToolchain {
    /// Unique identifier for this toolchain (hash of all files)
    pub id: ToolchainId,
    /// Zig version
    pub version: ZigVersion,
    /// Host platform (what platform zig runs on)
    pub host: HostPlatform,
    /// Hash of the zig executable
    pub zig_exe_hash: Blake3Hash,
    /// Hash of the lib directory (as a tarball)
    pub lib_hash: Blake3Hash,
}

/// Unique identifier for a toolchain, derived from its contents
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolchainId(pub Blake3Hash);

impl ToolchainId {
    /// Create a ToolchainId from zig exe and lib hashes
    pub fn from_parts(zig_exe_hash: &Blake3Hash, lib_hash: &Blake3Hash) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&zig_exe_hash.0);
        hasher.update(&lib_hash.0);
        Self(Blake3Hash(*hasher.finalize().as_bytes()))
    }
}

impl std::fmt::Display for ToolchainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "zig:{}", self.0.to_hex())
    }
}

/// Build the download URL for a Zig release
pub fn zig_download_url(version: &ZigVersion, platform: &HostPlatform) -> String {
    let ext = if platform.os == "windows" {
        "zip"
    } else {
        "tar.xz"
    };

    // Determine platform naming format based on version
    // Zig changed from OS-ARCH to ARCH-OS between 0.14.0 and 0.15.0
    let platform_name = if version_uses_new_format(&version.version) {
        // 0.15.0+ uses ARCH-OS format (e.g., "aarch64-macos")
        format!("{}-{}", platform.arch, platform.os)
    } else {
        // 0.14.0 and earlier use OS-ARCH format (e.g., "macos-aarch64")
        format!("{}-{}", platform.os, platform.arch)
    };

    // Official Zig releases are at ziglang.org/download/{version}/
    format!(
        "https://ziglang.org/download/{}/zig-{}-{}.{}",
        version.version, platform_name, version.version, ext
    )
}

/// Check if a version uses the new ARCH-OS naming format (>= 0.15.0)
fn version_uses_new_format(version_str: &str) -> bool {
    // Parse major.minor from version string
    // Examples: "0.15.2", "0.14.0", "0.16.0-dev.1234+abcdef"
    let parts: Vec<&str> = version_str.split(['.', '-']).collect();
    if parts.len() < 2 {
        return false;
    }

    if let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
        // Version >= 0.15.0 uses new format
        major > 0 || minor >= 15
    } else {
        false
    }
}

/// Extract a Zig tarball and return paths to the zig executable and lib directory
///
/// Returns (zig_exe_path, lib_dir_path) within the extraction directory
pub async fn extract_zig_tarball(
    tarball: &[u8],
    extract_dir: &Utf8Path,
) -> Result<(Utf8PathBuf, Utf8PathBuf), ZigError> {
    // Create extraction directory
    tokio::fs::create_dir_all(extract_dir).await?;

    // Decompress and extract off the async runtime threads.
    let extract_dir_buf = extract_dir.as_std_path().to_path_buf();
    let tarball_bytes = tarball.to_vec();
    tokio::task::spawn_blocking(move || -> Result<(), ZigError> {
        let mut decompressed = Vec::new();
        lzma_rs::xz_decompress(&mut std::io::Cursor::new(tarball_bytes), &mut decompressed)
            .map_err(|e| ZigError::ExtractionFailed(format!("xz decompression failed: {}", e)))?;

        let mut archive = tar::Archive::new(decompressed.as_slice());
        archive
            .unpack(&extract_dir_buf)
            .map_err(|e| ZigError::ExtractionFailed(format!("tar extraction failed: {}", e)))?;
        Ok(())
    })
    .await
    .map_err(|e| ZigError::ExtractionFailed(format!("extract task failed: {}", e)))??;

    // Find the zig directory (it's usually zig-{platform}-{version}/)
    let mut zig_dir: Option<Utf8PathBuf> = None;
    let mut entries = tokio::fs::read_dir(extract_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = Utf8PathBuf::try_from(entry.path())
            .map_err(|e| ZigError::ExtractionFailed(format!("non-UTF8 path: {}", e)))?;
        if path.file_name().is_some_and(|n| n.starts_with("zig-")) {
            zig_dir = Some(path);
            break;
        }
    }

    let zig_dir = zig_dir.ok_or(ZigError::ZigNotFound)?;

    // Locate zig executable
    let zig_exe = zig_dir.join("zig");
    if !tokio::fs::try_exists(&zig_exe).await? {
        return Err(ZigError::ZigNotFound);
    }

    // Locate lib directory
    let lib_dir = zig_dir.join("lib");
    if !tokio::fs::try_exists(&lib_dir).await? {
        return Err(ZigError::LibNotFound);
    }

    Ok((zig_exe, lib_dir))
}

/// Hash a file and return its Blake3 hash
pub async fn hash_file(path: &Utf8Path) -> Result<Blake3Hash, ZigError> {
    let contents = tokio::fs::read(path).await?;
    Ok(Blake3Hash::from_bytes(&contents))
}

/// Create a deterministic tarball of a directory and return its hash
///
/// This creates a reproducible tarball by:
/// - Sorting entries
/// - Setting fixed mtime/uid/gid
/// - Using consistent permissions
pub async fn hash_directory_as_tar(dir: &Utf8Path) -> Result<(Vec<u8>, Blake3Hash), ZigError> {
    let mut stack = vec![dir.as_std_path().to_path_buf()];
    let mut paths = Vec::new();

    while let Some(current) = stack.pop() {
        paths.push(current.clone());

        let metadata = tokio::fs::symlink_metadata(&current)
            .await
            .map_err(|e| ZigError::ExtractionFailed(format!("failed to read metadata: {}", e)))?;
        if metadata.is_dir() {
            let mut entries = tokio::fs::read_dir(&current).await.map_err(|e| {
                ZigError::ExtractionFailed(format!("failed to read directory: {}", e))
            })?;
            while let Some(entry) = entries.next_entry().await.map_err(|e| {
                ZigError::ExtractionFailed(format!("failed to read directory entry: {}", e))
            })? {
                stack.push(entry.path());
            }
        }
    }

    let mut builder = tar::Builder::new(Vec::new());

    // Collect entries and sort them for determinism
    paths.sort();

    for path in paths {
        let rel_path = path
            .strip_prefix(dir.as_std_path())
            .map_err(|e| ZigError::ExtractionFailed(format!("path prefix error: {}", e)))?;

        if rel_path.as_os_str().is_empty() {
            continue; // Skip the root directory itself
        }

        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|e| ZigError::ExtractionFailed(format!("failed to read metadata: {}", e)))?;

        let mut header = tar::Header::new_gnu();
        header.set_mtime(0); // Fixed mtime for reproducibility
        header.set_uid(0);
        header.set_gid(0);

        if metadata.is_file() {
            let contents = tokio::fs::read(&path).await?;
            header.set_size(contents.len() as u64);
            header.set_mode(if is_executable(&metadata) {
                0o755
            } else {
                0o644
            });
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();

            builder
                .append_data(&mut header, rel_path, contents.as_slice())
                .map_err(|e| {
                    ZigError::ExtractionFailed(format!("failed to add file to tar: {}", e))
                })?;
        } else if metadata.is_dir() {
            header.set_size(0);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_cksum();

            builder
                .append_data(&mut header, rel_path, std::io::empty())
                .map_err(|e| {
                    ZigError::ExtractionFailed(format!("failed to add dir to tar: {}", e))
                })?;
        } else if metadata.is_symlink() {
            let target = tokio::fs::read_link(&path).await?;
            header.set_size(0);
            header.set_mode(0o777);
            header.set_entry_type(tar::EntryType::Symlink);

            let target_str = target.to_string_lossy();
            builder
                .append_link(&mut header, rel_path, target_str.as_ref())
                .map_err(|e| {
                    ZigError::ExtractionFailed(format!("failed to add symlink to tar: {}", e))
                })?;
        }
    }

    let tarball = builder
        .into_inner()
        .map_err(|e| ZigError::ExtractionFailed(format!("failed to finish tar: {}", e)))?;

    let hash = Blake3Hash::from_bytes(&tarball);

    Ok((tarball, hash))
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Result of acquiring a Zig toolchain (before CAS storage)
pub struct AcquiredToolchain {
    /// Toolchain identifier
    pub id: ToolchainId,
    /// Zig version
    pub version: ZigVersion,
    /// Host platform
    pub host: HostPlatform,
    /// Hash of the zig executable
    pub zig_exe_hash: Blake3Hash,
    /// Contents of the zig executable (for CAS storage)
    pub zig_exe_contents: Vec<u8>,
    /// Hash of the lib directory tarball
    pub lib_hash: Blake3Hash,
    /// Lib directory tarball (for CAS storage)
    pub lib_tarball: Vec<u8>,
    /// Extraction directory (for cleanup)
    pub extract_dir: Utf8PathBuf,
}

impl AcquiredToolchain {
    /// Create a ZigToolchain from this acquired toolchain
    pub fn into_toolchain(self) -> ZigToolchain {
        ZigToolchain {
            id: self.id,
            version: self.version,
            host: self.host,
            zig_exe_hash: self.zig_exe_hash,
            lib_hash: self.lib_hash,
        }
    }

    /// Clean up the extraction directory
    pub async fn cleanup(&self) -> Result<(), ZigError> {
        if tokio::fs::try_exists(&self.extract_dir).await? {
            tokio::fs::remove_dir_all(&self.extract_dir).await?;
        }
        Ok(())
    }
}

/// Materialize a Zig toolchain from CAS to a local directory
///
/// Given the zig executable and lib tarball from CAS, extract them to a directory
/// suitable for running zig commands.
pub async fn materialize_toolchain(
    zig_exe: &[u8],
    lib_tarball: &[u8],
    target_dir: &Utf8Path,
) -> Result<MaterializedToolchain, ZigError> {
    // Create target directory
    tokio::fs::create_dir_all(target_dir).await?;

    // Write zig executable
    let zig_path = target_dir.join("zig");
    tokio::fs::write(&zig_path, zig_exe).await?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&zig_path, std::fs::Permissions::from_mode(0o755)).await?;
    }

    // Extract lib tarball
    let lib_dir = target_dir.join("lib");
    tokio::fs::create_dir_all(&lib_dir).await?;

    let lib_dir_buf = lib_dir.as_std_path().to_path_buf();
    let lib_bytes = lib_tarball.to_vec();
    tokio::task::spawn_blocking(move || -> Result<(), ZigError> {
        let mut archive = tar::Archive::new(lib_bytes.as_slice());
        archive
            .unpack(&lib_dir_buf)
            .map_err(|e| ZigError::ExtractionFailed(format!("failed to unpack lib: {}", e)))?;
        Ok(())
    })
    .await
    .map_err(|e| ZigError::ExtractionFailed(format!("materialize task failed: {}", e)))??;

    Ok(MaterializedToolchain { zig_path, lib_dir })
}

/// A toolchain materialized to the filesystem, ready for use
pub struct MaterializedToolchain {
    /// Path to the zig executable
    pub zig_path: Utf8PathBuf,
    /// Path to the lib directory
    pub lib_dir: Utf8PathBuf,
}

impl MaterializedToolchain {
    /// Get the path to use for `zig cc` commands
    pub fn cc_path(&self) -> &Utf8Path {
        &self.zig_path
    }

    /// Build command-line arguments for compiling a C file
    pub fn cc_compile_args(
        &self,
        source: &Utf8Path,
        output: &Utf8Path,
        target_triple: &str,
    ) -> Vec<String> {
        vec![
            "cc".to_string(),
            "-target".to_string(),
            target_triple.to_string(),
            "-c".to_string(),
            source.to_string(),
            "-o".to_string(),
            output.to_string(),
        ]
    }

    /// Build command-line arguments for linking object files
    pub fn cc_link_args(
        &self,
        objects: &[&Utf8Path],
        output: &Utf8Path,
        target_triple: &str,
    ) -> Vec<String> {
        let mut args = vec![
            "cc".to_string(),
            "-target".to_string(),
            target_triple.to_string(),
        ];

        for obj in objects {
            args.push(obj.to_string());
        }

        args.push("-o".to_string());
        args.push(output.to_string());

        args
    }
}

/// Acquire a Zig toolchain by downloading and extracting it
///
/// Downloads the tarball from ziglang.org, extracts it to a temp directory,
/// hashes the contents, and returns an AcquiredToolchain ready for CAS storage.
pub async fn acquire_zig_toolchain(
    version: ZigVersion,
    platform: HostPlatform,
) -> Result<AcquiredToolchain, ZigError> {
    let url = zig_download_url(&version, &platform);

    // Download the tarball
    let tarball = vx_http::get_bytes(&url)
        .await
        .map_err(|e| ZigError::FetchError {
            url: url.clone(),
            source: Box::new(e),
        })?;

    // Create a temp directory for extraction
    let extract_dir =
        camino::Utf8PathBuf::from(format!("/tmp/zig-acquire-{}", uuid::Uuid::new_v4()));

    // Extract tarball
    let (zig_exe_path, lib_dir_path) = extract_zig_tarball(&tarball, &extract_dir).await?;

    // Hash the zig executable
    let zig_exe_contents = tokio::fs::read(&zig_exe_path).await?;
    let zig_exe_hash = Blake3Hash::from_bytes(&zig_exe_contents);

    // Hash the lib directory as tarball
    let (lib_tarball, lib_hash) = hash_directory_as_tar(&lib_dir_path).await?;

    // Create toolchain ID
    let id = ToolchainId::from_parts(&zig_exe_hash, &lib_hash);

    Ok(AcquiredToolchain {
        id,
        version,
        host: platform,
        zig_exe_hash,
        zig_exe_contents,
        lib_hash,
        lib_tarball,
        extract_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zig_version_display() {
        let v = ZigVersion::new("0.13.0");
        assert_eq!(v.to_string(), "0.13.0");
        assert!(v.is_release());

        let v_dev = ZigVersion::new("0.14.0-dev.1234+abcdef");
        assert!(!v_dev.is_release());
    }

    #[test]
    fn zig_download_url_linux() {
        let v = ZigVersion::new("0.13.0");
        let p = HostPlatform {
            os: "linux".into(),
            arch: "x86_64".into(),
        };

        assert_eq!(
            zig_download_url(&v, &p),
            "https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz"
        );
    }

    #[test]
    fn zig_download_url_macos() {
        let v = ZigVersion::new("0.13.0");
        let p = HostPlatform {
            os: "macos".into(),
            arch: "aarch64".into(),
        };

        assert_eq!(
            zig_download_url(&v, &p),
            "https://ziglang.org/download/0.13.0/zig-macos-aarch64-0.13.0.tar.xz"
        );
    }

    #[test]
    fn zig_download_url_new_format_macos() {
        // 0.15.0+ uses ARCH-OS format
        let v = ZigVersion::new("0.15.2");
        let p = HostPlatform {
            os: "macos".into(),
            arch: "aarch64".into(),
        };

        assert_eq!(
            zig_download_url(&v, &p),
            "https://ziglang.org/download/0.15.2/zig-aarch64-macos-0.15.2.tar.xz"
        );
    }

    #[test]
    fn zig_download_url_new_format_linux() {
        // 0.15.0+ uses ARCH-OS format
        let v = ZigVersion::new("0.15.2");
        let p = HostPlatform {
            os: "linux".into(),
            arch: "x86_64".into(),
        };

        assert_eq!(
            zig_download_url(&v, &p),
            "https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz"
        );
    }

    #[test]
    fn toolchain_id_deterministic() {
        let exe_hash = Blake3Hash::from_bytes(b"zig executable contents");
        let lib_hash = Blake3Hash::from_bytes(b"lib tarball contents");

        let id1 = ToolchainId::from_parts(&exe_hash, &lib_hash);
        let id2 = ToolchainId::from_parts(&exe_hash, &lib_hash);

        assert_eq!(id1, id2);

        // Different content = different ID
        let lib_hash2 = Blake3Hash::from_bytes(b"different lib contents");
        let id3 = ToolchainId::from_parts(&exe_hash, &lib_hash2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn host_platform_detection() {
        // This just tests that detection works on the current platform
        let platform = HostPlatform::detect().unwrap();
        assert!(!platform.os.is_empty());
        assert!(!platform.arch.is_empty());
    }
}
