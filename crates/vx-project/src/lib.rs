//! vx.styx project manifest parsing
//!
//! Provides unified project configuration for Rust, C, and mixed projects.
//! Falls back to Cargo.toml if vx.styx doesn't exist.

use camino::{Utf8Path, Utf8PathBuf};
use thiserror::Error;

/// Errors during vx.styx parsing
#[derive(Debug, Error)]
pub enum VxProjectError {
    #[error("failed to read {path}: {source}")]
    ReadError {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse vx.styx: {0}")]
    ParseError(String),

    #[error("missing required node: {0}")]
    MissingNode(&'static str),

    #[error("missing required property: {0}")]
    MissingProperty(&'static str),

    #[error("invalid value for {property}: {reason}")]
    InvalidValue {
        property: &'static str,
        reason: String,
    },

    #[error("no targets found (expected bin or lib)")]
    NoTargets,
}

/// Project metadata
#[derive(Debug, Clone, facet::Facet)]
pub struct Project {
    pub name: String,
    pub lang: String,
}

impl Project {
    /// Get the language as an enum
    pub fn language(&self) -> Option<Language> {
        match self.lang.as_str() {
            "rust" => Some(Language::Rust),
            "c" => Some(Language::C),
            _ => None,
        }
    }
}

/// Project language
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    C,
}

/// Binary target
#[derive(Debug, Clone, facet::Facet)]
pub struct BinTarget {
    pub name: String,
    /// Source file(s) - single file for now
    pub sources: String,
}

/// Library target (future)
#[derive(Debug, Clone, facet::Facet)]
pub struct LibTarget {
    pub name: String,
    pub sources: String,
}

/// Top-level vx.styx manifest
#[derive(Debug, Clone, facet::Facet)]
pub struct VxManifest {
    pub project: Project,
    #[facet(default)]
    pub bins: Vec<BinTarget>,
    #[facet(default)]
    pub libs: Vec<LibTarget>,
}

impl VxManifest {
    /// Parse vx.styx from a file path
    pub fn from_path(path: &Utf8Path) -> Result<Self, VxProjectError> {
        let contents = std::fs::read_to_string(path).map_err(|e| VxProjectError::ReadError {
            path: path.to_owned(),
            source: e,
        })?;

        Self::parse(&contents)
    }

    /// Parse vx.styx from a string
    pub fn parse(source: &str) -> Result<Self, VxProjectError> {
        facet_styx::from_str(source).map_err(|e| VxProjectError::ParseError(e.to_string()))
    }

    /// Check if vx.styx exists in the given directory
    pub fn exists_in(dir: &Utf8Path) -> bool {
        dir.join("vx.styx").exists()
    }

    /// Find vx.styx starting from the given directory and searching upward
    pub fn find(start_path: &Utf8Path) -> Option<Utf8PathBuf> {
        let mut current = start_path;
        loop {
            let vx_styx = current.join("vx.styx");
            if vx_styx.exists() {
                return Some(vx_styx);
            }

            current = current.parent()?;
        }
    }

    /// Validate the manifest (check for required fields, consistency, etc.)
    pub fn validate(&self) -> Result<(), VxProjectError> {
        // Must have at least one target
        if self.bins.is_empty() && self.libs.is_empty() {
            return Err(VxProjectError::NoTargets);
        }

        // Project name must not be empty
        if self.project.name.is_empty() {
            return Err(VxProjectError::InvalidValue {
                property: "project.name",
                reason: "name cannot be empty".to_string(),
            });
        }

        // Binary names must not be empty
        for bin in &self.bins {
            if bin.name.is_empty() {
                return Err(VxProjectError::InvalidValue {
                    property: "bin.name",
                    reason: "name cannot be empty".to_string(),
                });
            }
            if bin.sources.is_empty() {
                return Err(VxProjectError::InvalidValue {
                    property: "bin.sources",
                    reason: "sources cannot be empty".to_string(),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_c_project() {
        let styx = r#"
            project {
                name hello
                lang c
            }
            bins ({
                name hello
                sources main.c
            })
        "#;

        let manifest = VxManifest::parse(styx).unwrap();
        assert_eq!(manifest.project.name, "hello");
        assert_eq!(manifest.project.lang, "c");
        assert_eq!(manifest.project.language(), Some(Language::C));
        assert_eq!(manifest.bins.len(), 1);
        assert_eq!(manifest.bins[0].name, "hello");
        assert_eq!(manifest.bins[0].sources, "main.c");
    }

    #[test]
    fn test_parse_rust_project() {
        let styx = r#"
            project {
                name my-app
                lang rust
            }
            bins ({
                name my-app
                sources src/main.rs
            })
        "#;

        let manifest = VxManifest::parse(styx).unwrap();
        assert_eq!(manifest.project.name, "my-app");
        assert_eq!(manifest.project.lang, "rust");
        assert_eq!(manifest.project.language(), Some(Language::Rust));
    }

    #[test]
    fn test_validate_no_targets() {
        let styx = r#"
            project {
                name empty
                lang c
            }
        "#;

        let manifest = VxManifest::parse(styx).unwrap();
        let err = manifest.validate().unwrap_err();
        assert!(matches!(err, VxProjectError::NoTargets));
    }

    #[test]
    fn test_validate_empty_name() {
        let styx = r#"
            project {
                name ""
                lang c
            }
            bins ({
                name hello
                sources main.c
            })
        "#;

        let manifest = VxManifest::parse(styx).unwrap();
        let err = manifest.validate().unwrap_err();
        assert!(matches!(err, VxProjectError::InvalidValue { .. }));
    }
}
