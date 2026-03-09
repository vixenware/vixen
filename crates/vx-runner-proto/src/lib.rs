//! Runner service protocol definitions
//!
//! The runner service is a remote-capable worker that compiles code.
//! All inputs and outputs go through CAS - no filesystem paths are shared.
//!
//! Runner responsibilities:
//! - Materialize toolchains from CAS (cached locally)
//! - Materialize source trees from CAS
//! - Materialize dependency artifacts from CAS
//! - Run rustc/zig
//! - Ingest outputs to CAS
//! - Return output manifest hashes
//!
//! Daemon responsibilities:
//! - Ingest source files to CAS
//! - Orchestrate build order
//! - Compute cache keys
//! - Check cache hits
//! - Materialize final outputs locally (if needed)

use facet::Facet;
use vx_store_proto::{ManifestHash, ServiceVersion, StoreProgress};

/// Runner protocol version.
/// Bump this when making breaking changes to the Runner RPC interface.
pub const RUNNER_PROTOCOL_VERSION: u32 = 5;

// =============================================================================
// Rust Compilation
// =============================================================================

/// A dependency for Rust compilation
#[derive(Debug, Clone, Facet)]
pub struct RustDep {
    /// Extern crate name (used in --extern flag)
    pub extern_name: String,
    /// Manifest hash of the dependency's rlib in CAS
    pub manifest_hash: ManifestHash,
    /// For registry dependencies: manifest hash of the registry crate tarball
    /// If present, runner must materialize and compile the source before using the rlib
    pub registry_crate_manifest: Option<ManifestHash>,
}

/// Request to compile a Rust crate
///
/// All paths are relative to the source tree root.
/// All artifacts are identified by CAS manifest hashes.
#[derive(Debug, Clone, Facet)]
pub struct RustCompileRequest {
    /// Toolchain manifest hash (runner materializes locally)
    pub toolchain_manifest: ManifestHash,

    /// Source tree manifest hash (all source files).
    /// Ignored if registry_crate_manifest is set.
    pub source_manifest: ManifestHash,

    /// Crate root within source tree (e.g., "src/lib.rs").
    /// Ignored if registry_crate_manifest is set (runner reads Cargo.toml).
    pub crate_root: String,

    /// Crate name (with underscores, as rustc expects)
    pub crate_name: String,

    /// Crate type: "lib" or "bin"
    pub crate_type: String,

    /// Rust edition: "2015", "2018", "2021", "2024".
    /// Ignored if registry_crate_manifest is set (runner reads Cargo.toml).
    pub edition: String,

    /// Target triple (e.g., "aarch64-apple-darwin")
    pub target_triple: String,

    /// Profile: "debug" or "release"
    pub profile: String,

    /// Dependencies (runner materializes rlibs from CAS)
    pub deps: Vec<RustDep>,

    /// If set, this is a registry crate compilation.
    /// Runner will extract the tarball, parse Cargo.toml for edition/lib_path,
    /// and ignore source_manifest/crate_root/edition from above.
    pub registry_crate_manifest: Option<ManifestHash>,
}

/// Result of compiling a Rust crate
#[derive(Debug, Clone, Facet)]
pub struct RustCompileResult {
    /// Whether compilation succeeded
    pub success: bool,

    /// Exit code from rustc
    pub exit_code: i32,

    /// Captured stdout
    pub stdout: String,

    /// Captured stderr
    pub stderr: String,

    /// Duration in milliseconds
    pub duration_ms: u64,

    /// Manifest hash of the build output in CAS (if successful)
    /// For libs: contains the rlib blob
    /// For bins: contains the executable blob
    pub output_manifest: Option<ManifestHash>,

    /// Error message (if failed before rustc ran)
    pub error: Option<String>,
}

// =============================================================================
// C/C++ Compilation
// =============================================================================

/// Request to compile a C/C++ translation unit
#[derive(Debug, Clone, Facet)]
pub struct CcCompileRequest {
    /// Zig toolchain manifest hash
    pub toolchain_manifest: ManifestHash,

    /// Source tree manifest hash
    pub source_manifest: ManifestHash,

    /// Source file within tree (e.g., "src/main.c")
    pub source_path: String,

    /// Target triple
    pub target_triple: String,

    /// Profile: "debug" or "release"
    pub profile: String,

    /// Include paths (relative to source tree root)
    pub include_paths: Vec<String>,

    /// Preprocessor defines
    pub defines: Vec<(String, Option<String>)>,

    /// CommandLineTools version on macOS (for cache key)
    /// This ensures different CommandLineTools versions produce different cache keys
    pub macos_commandlinetools_version: Option<String>,
}

/// Result of compiling a C/C++ translation unit
#[derive(Debug, Clone, Facet)]
pub struct CcCompileResult {
    /// Whether compilation succeeded
    pub success: bool,

    /// Exit code
    pub exit_code: i32,

    /// Captured stdout
    pub stdout: String,

    /// Captured stderr
    pub stderr: String,

    /// Duration in milliseconds
    pub duration_ms: u64,

    /// Manifest hash of the object file in CAS
    pub output_manifest: Option<ManifestHash>,

    /// Discovered dependencies from depfile (paths relative to source tree)
    /// Used by daemon to update incremental state
    pub discovered_deps: Vec<String>,

    /// Error message if failed
    pub error: Option<String>,
}

/// Request to link C/C++ objects into a binary
#[derive(Debug, Clone, Facet)]
pub struct CcLinkRequest {
    /// Zig toolchain manifest hash
    pub toolchain_manifest: ManifestHash,

    /// Object file manifest hashes (from CAS)
    pub object_manifests: Vec<ManifestHash>,

    /// Binary name (without extension)
    pub binary_name: String,

    /// Target triple
    pub target_triple: String,

    /// Libraries to link against (e.g., "c", "m")
    pub libs: Vec<String>,

    /// CommandLineTools version on macOS (for cache key)
    /// This ensures different CommandLineTools versions produce different cache keys
    pub macos_commandlinetools_version: Option<String>,
}

/// Result of linking C/C++ objects
#[derive(Debug, Clone, Facet)]
pub struct CcLinkResult {
    /// Whether linking succeeded
    pub success: bool,

    /// Exit code
    pub exit_code: i32,

    /// Captured stdout
    pub stdout: String,

    /// Captured stderr
    pub stderr: String,

    /// Duration in milliseconds
    pub duration_ms: u64,

    /// Manifest hash of the binary in CAS
    pub output_manifest: Option<ManifestHash>,

    /// Error message if failed
    pub error: Option<String>,
}

// =============================================================================
// Generic Sandboxed Tool Execution
// =============================================================================

// r[impl exec.arg]
/// A single argument or env value for sandboxed execution.
///
/// `Literal` values are passed through exactly. `Path` values name VFS
/// locations and keep slash-delimited semantics until the runner rewrites
/// them for the host platform at process launch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
#[repr(u8)]
pub enum RunArg {
    Literal(String),
    Path(String),
}

impl RunArg {
    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    pub fn path(value: impl Into<String>) -> Self {
        Self::Path(value.into())
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Literal(value) | Self::Path(value) => value,
        }
    }

    pub fn render_for_host(&self) -> String {
        self.render_with_separator(std::path::MAIN_SEPARATOR)
    }

    fn render_with_separator(&self, separator: char) -> String {
        match self {
            Self::Literal(value) => value.clone(),
            Self::Path(value) if separator == '/' => value.clone(),
            Self::Path(value) => value.replace('/', &separator.to_string()),
        }
    }
}

/// Mount a CAS tree manifest into the action VFS prefix.
#[derive(Debug, Clone, Facet)]
pub struct RunTreeMount {
    /// Mount point within the action prefix (e.g. "src" or "").
    pub mount: String,
    /// CAS tree manifest hash.
    pub manifest: ManifestHash,
}

/// Materialize a single file output from a CAS node manifest into the action prefix.
#[derive(Debug, Clone, Facet)]
pub struct RunFileMount {
    /// Path within the action prefix (e.g. "obj/0000.o").
    pub path: String,
    /// Node manifest hash containing the output blob.
    pub manifest: ManifestHash,
    /// Logical output name within that node manifest (e.g. "object", "rlib", "bin").
    pub logical: String,
}

#[derive(Debug, Clone, Facet)]
pub struct RunOutput {
    /// Logical output name (e.g. "object", "bin").
    pub logical: String,
    /// Path within the action prefix (must be under "out/").
    pub path: String,
    /// Whether the output should be marked executable in CAS.
    pub executable: bool,
}

/// Request to run a sandboxed toolchain command.
///
/// This is the generic building block that language-specific front-ends can lower into.
#[derive(Debug, Clone, Facet)]
pub struct RunRequest {
    /// Toolchain manifest hash (runner materializes locally).
    pub toolchain_manifest: ManifestHash,

    /// Program path relative to the toolchain root (e.g. "zig").
    pub program_rel: String,

    /// Arguments passed to the program (no shell parsing).
    pub args: Vec<RunArg>,

    /// CAS trees to mount into the action prefix.
    pub tree_mounts: Vec<RunTreeMount>,

    /// Individual output files from other node manifests to materialize into the action prefix.
    pub file_mounts: Vec<RunFileMount>,

    /// Outputs that must be produced (and will be flushed to CAS).
    pub outputs: Vec<RunOutput>,

    /// Writable directories to create within the action prefix.
    pub writable_roots: Vec<String>,

    /// Optional working directory within the action prefix.
    /// If unset, runner uses the action prefix root.
    pub cwd: Option<String>,

    /// Key/value env vars to set for the process.
    pub env: Vec<(String, RunArg)>,

    /// Whether to clear the environment before setting `env`.
    pub clear_env: bool,

    /// Optional macOS CommandLineTools version for cache key stability (best-effort).
    pub macos_commandlinetools_version: Option<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct RunResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub output_manifest: Option<ManifestHash>,
    pub error: Option<String>,
}

// =============================================================================
// Runner Service Trait
// =============================================================================

/// Runner service trait
///
/// A remote-capable compilation service. All inputs/outputs go through CAS.
/// No filesystem paths are shared between the daemon and the runner.
#[roam::service]
pub trait Runner {
    /// Get service version information (for health checks and compatibility)
    async fn version(&self) -> ServiceVersion;

    /// Compile a Rust crate (lib or bin)
    ///
    /// Runner will:
    /// 1. Materialize the toolchain from CAS (cached locally)
    /// 2. Materialize the source tree from CAS
    /// 3. Materialize dependency rlibs from CAS
    /// 4. Run rustc
    /// 5. Ingest output to CAS
    /// 6. Return the output manifest hash
    ///
    /// Progress channel receives merged stdout/stderr lines during compilation.
    async fn compile_rust(
        &self,
        request: RustCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> RustCompileResult;

    /// Compile a C/C++ translation unit
    ///
    /// Similar to compile_rust but for C/C++ via zig cc.
    /// Returns discovered dependencies for incremental builds.
    ///
    /// Progress channel receives merged stdout/stderr lines during compilation.
    async fn compile_cc(
        &self,
        request: CcCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcCompileResult;

    /// Link C/C++ object files into a binary
    ///
    /// Runner will:
    /// 1. Materialize the Zig toolchain from CAS
    /// 2. Materialize all object files from CAS
    /// 3. Run zig cc to link them
    /// 4. Ingest the binary to CAS
    /// 5. Return the binary manifest hash
    ///
    /// Progress channel receives merged stdout/stderr lines during linking.
    async fn link_cc(
        &self,
        request: CcLinkRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcLinkResult;

    /// Run a sandboxed command with hermetic inputs/outputs via CAS/VFS.
    async fn run(&self, request: RunRequest, progress: roam::Tx<StoreProgress>) -> RunResult;
}

#[cfg(test)]
mod tests {
    use super::RunArg;

    #[test]
    // r[verify exec.arg]
    fn path_args_render_with_host_separators() {
        assert_eq!(
            RunArg::path("src/bin/main.rs").render_with_separator('\\'),
            "src\\bin\\main.rs"
        );
    }

    #[test]
    // r[verify exec.arg]
    fn literal_args_are_not_rewritten() {
        assert_eq!(
            RunArg::literal("--flag=a/b").render_with_separator('\\'),
            "--flag=a/b"
        );
    }
}
