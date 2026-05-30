//! Vixen package resolution (compiler-side): parse a package **manifest** (the
//! styx frontmatter on `main.vx`) and **resolve an import string** to a
//! package's source, so the compiler front-end can pull the package into the
//! module graph. See `docs/design/package-resolution.md`.
//!
//! This is front-end / source-graph machinery — no evaluator. A [`PackageResolver`]
//! says *where source comes from*; everything above the trait is independent of
//! whether that's the filesystem (iterate without recompiling), a bundled
//! built-in, or (later) a registry + content store.

use facet::Facet;
use std::fmt;
use std::path::{Path, PathBuf};
use vx_parser::{SourceId, parse_source_file};

/// A package manifest — the `main.vx` frontmatter, written in styx.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Manifest {
    /// Scoped package id, e.g. `@vixen/grammar-cc`.
    pub id: String,
    /// Semver string; empty if unspecified.
    #[facet(default)]
    pub version: String,
    /// Dependency package ids. (Version constraints are ignored in v1.)
    #[facet(default)]
    pub dependencies: Vec<String>,
}

/// A resolved package: its canonical id and the source text of its `main.vx`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    pub id: String,
    pub main: String,
}

/// Why an import could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No package matched the import spec.
    NotFound(String),
    /// The spec was malformed (e.g. not `@scope/name` or `vixen:name`).
    BadSpec(String),
    /// The package source could not be read.
    Io { spec: String, message: String },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound(s) => write!(f, "no package found for import {s:?}"),
            ResolveError::BadSpec(s) => write!(f, "malformed import spec {s:?}"),
            ResolveError::Io { spec, message } => {
                write!(f, "reading package {spec:?}: {message}")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// An error parsing a manifest from frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub message: String,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ManifestError {}

/// Parse a package manifest from a `main.vx` source: extract its frontmatter
/// fence and deserialize the styx body. Returns `None` if there is no
/// frontmatter (a module without a manifest is allowed).
pub fn parse_manifest(main_vx: &str) -> Result<Option<Manifest>, ManifestError> {
    let parsed = parse_source_file(main_vx, SourceId(0));
    let Some(frontmatter) = parsed.tree().frontmatter() else {
        return Ok(None);
    };
    let manifest = facet_styx::from_str(frontmatter.content()).map_err(|e| ManifestError {
        message: format!("invalid manifest: {e}"),
    })?;
    Ok(Some(manifest))
}

/// Resolves an import string (`@scope/name`, `vixen:name`) to a package's
/// source. The one seam between "where source lives" and the rest of the
/// compiler.
pub trait PackageResolver {
    fn resolve(&self, spec: &str) -> Result<ResolvedPackage, ResolveError>;
}

/// Resolves `@scope/name` from a filesystem root, by the convention
/// `<root>/@scope/name/main.vx`. The point of this resolver: edit a package on
/// disk and rebuild *the project*, without recompiling the Vixen binary.
#[derive(Debug, Clone)]
pub struct FsResolver {
    pub root: PathBuf,
}

impl FsResolver {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl PackageResolver for FsResolver {
    fn resolve(&self, spec: &str) -> Result<ResolvedPackage, ResolveError> {
        if !spec.starts_with('@') || !spec.contains('/') {
            return Err(ResolveError::BadSpec(spec.to_string()));
        }
        let path = self.root.join(spec).join("main.vx");
        match std::fs::read_to_string(&path) {
            Ok(main) => Ok(ResolvedPackage {
                id: spec.to_string(),
                main,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(ResolveError::NotFound(spec.to_string()))
            }
            Err(e) => Err(ResolveError::Io {
                spec: spec.to_string(),
                message: e.to_string(),
            }),
        }
    }
}

/// Resolves `vixen:<name>` built-ins shipped *with the runtime* (the grammar
/// authoring vocabulary and other first-party modules), embedded at build time.
#[derive(Debug, Clone, Default)]
pub struct BundledResolver;

impl BundledResolver {
    fn lookup(spec: &str) -> Option<&'static str> {
        match spec {
            "vixen:command-grammar" => Some(include_str!(
                "../../../crates-plus/command-model/grammars/command-grammar.vx"
            )),
            _ => None,
        }
    }
}

impl PackageResolver for BundledResolver {
    fn resolve(&self, spec: &str) -> Result<ResolvedPackage, ResolveError> {
        match Self::lookup(spec) {
            Some(main) => Ok(ResolvedPackage {
                id: spec.to_string(),
                main: main.to_string(),
            }),
            None => Err(ResolveError::NotFound(spec.to_string())),
        }
    }
}

/// Tries the bundled built-ins first (`vixen:*`), then the filesystem
/// (`@scope/name`). The resolver the compiler holds in practice.
#[derive(Debug, Clone)]
pub struct CompositeResolver {
    pub bundled: BundledResolver,
    pub fs: FsResolver,
}

impl CompositeResolver {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            bundled: BundledResolver,
            fs: FsResolver::new(root),
        }
    }
}

impl PackageResolver for CompositeResolver {
    fn resolve(&self, spec: &str) -> Result<ResolvedPackage, ResolveError> {
        match self.bundled.resolve(spec) {
            Err(ResolveError::NotFound(_)) => self.fs.resolve(spec),
            other => other,
        }
    }
}
