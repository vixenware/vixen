//! macOS CommandLineTools version detection
//!
//! This module discovers the installed CommandLineTools/Xcode version on macOS.
//! The version string is used as part of cache keys for C/C++ compilation to
//! ensure that different toolchain versions produce different build artifacts.

use std::process::Command;

use crate::ToolchainError;

/// Represents the CommandLineTools version on macOS
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommandLineToolsVersion {
    /// Full version string (e.g., "26.2.0.0.1.1764812424")
    pub version: String,
    /// Build version if available (e.g., "17C52" from Xcode)
    pub build: Option<String>,
}

impl CommandLineToolsVersion {
    /// Format as a string for cache key inclusion
    pub fn as_cache_key_string(&self) -> String {
        match &self.build {
            Some(build) => format!("{}-{}", self.version, build),
            None => self.version.clone(),
        }
    }
}

/// Detect CommandLineTools version on macOS
///
/// This function tries multiple methods to discover the version:
/// 1. pkgutil for standalone CommandLineTools
/// 2. xcodebuild for Xcode installation
///
/// Returns None if not running on macOS or if no toolchain is found.
pub fn detect_command_line_tools_version() -> Result<Option<CommandLineToolsVersion>, ToolchainError>
{
    // Only run on macOS
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }

    // Try pkgutil first (for standalone CommandLineTools)
    if let Ok(version) = detect_via_pkgutil() {
        return Ok(Some(version));
    }

    // Fall back to xcodebuild (for Xcode)
    if let Ok(version) = detect_via_xcodebuild() {
        return Ok(Some(version));
    }

    // No toolchain found
    Ok(None)
}

/// Detect CommandLineTools version via pkgutil
fn detect_via_pkgutil() -> Result<CommandLineToolsVersion, ToolchainError> {
    let output = Command::new("pkgutil")
        .arg("--pkg-info=com.apple.pkg.CLTools_Executables")
        .output()
        .map_err(|e| ToolchainError::FetchError {
            url: "pkgutil --pkg-info".to_string(),
            source: Box::new(e),
        })?;

    if !output.status.success() {
        return Err(ToolchainError::FetchError {
            url: "pkgutil --pkg-info".to_string(),
            source: "pkgutil failed".into(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse version from output like:
    // package-id: com.apple.pkg.CLTools_Executables
    // version: 26.2.0.0.1.1764812424
    for line in stdout.lines() {
        if let Some(version) = line.strip_prefix("version: ") {
            return Ok(CommandLineToolsVersion {
                version: version.trim().to_string(),
                build: None,
            });
        }
    }

    Err(ToolchainError::FetchError {
        url: "pkgutil --pkg-info".to_string(),
        source: "could not parse version from pkgutil output".into(),
    })
}

/// Detect Xcode version via xcodebuild
fn detect_via_xcodebuild() -> Result<CommandLineToolsVersion, ToolchainError> {
    let output = Command::new("xcodebuild")
        .arg("-version")
        .output()
        .map_err(|e| ToolchainError::FetchError {
            url: "xcodebuild -version".to_string(),
            source: Box::new(e),
        })?;

    if !output.status.success() {
        return Err(ToolchainError::FetchError {
            url: "xcodebuild -version".to_string(),
            source: "xcodebuild failed".into(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse output like:
    // Xcode 26.2
    // Build version 17C52
    let mut version = None;
    let mut build = None;

    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("Xcode ") {
            version = Some(v.trim().to_string());
        } else if let Some(b) = line.strip_prefix("Build version ") {
            build = Some(b.trim().to_string());
        }
    }

    match version {
        Some(version) => Ok(CommandLineToolsVersion { version, build }),
        None => Err(ToolchainError::FetchError {
            url: "xcodebuild -version".to_string(),
            source: "could not parse version from xcodebuild output".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn test_detect_command_line_tools_version() {
        let result = detect_command_line_tools_version();
        assert!(result.is_ok());

        if let Ok(Some(version)) = result {
            println!("Detected CommandLineTools version: {:?}", version);
            assert!(!version.version.is_empty());

            let cache_key_str = version.as_cache_key_string();
            assert!(!cache_key_str.is_empty());
            println!("Cache key string: {}", cache_key_str);
        }
    }

    #[test]
    fn test_cache_key_string_format() {
        let version_with_build = CommandLineToolsVersion {
            version: "26.2".to_string(),
            build: Some("17C52".to_string()),
        };
        assert_eq!(version_with_build.as_cache_key_string(), "26.2-17C52");

        let version_without_build = CommandLineToolsVersion {
            version: "26.2.0.0.1.1764812424".to_string(),
            build: None,
        };
        assert_eq!(
            version_without_build.as_cache_key_string(),
            "26.2.0.0.1.1764812424"
        );
    }
}
