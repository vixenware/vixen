//! Runner service protocol definitions
//!
//! The runner service is a remote-capable worker that compiles code.
//! All file inputs and outputs go through CAS/VFS, and shipped evaluator
//! payloads go over RPC. No filesystem paths are shared between daemon and runner.
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
pub use vx_eval::{
    ExecutorBackendKind, ExecutorSelector, HostObservation, ShippedFunction, TransportBinOp,
    TransportBlob, TransportBlock, TransportBuiltinValue, TransportExpr, TransportFnDef,
    TransportModule, TransportParam, TransportPoisonError, TransportStmt, TransportStruct,
    TransportTree, TransportUnaryOp, TransportValue,
};
use vx_store_proto::{ManifestHash, ServiceVersion, StoreProgress};

/// Runner protocol version.
/// Bump this when making breaking changes to the Runner RPC interface.
pub const RUNNER_PROTOCOL_VERSION: u32 = 17;

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

/// Executor-side evaluator payload shipped with a generic run request.
#[derive(Debug, Clone, Facet)]
pub struct RunEvalPayload {
    /// Lowered module used to resolve global calls from shipped closures.
    pub module: TransportModule,
    /// Optional observer closure to run after the process exits.
    pub observer: Option<ShippedFunction>,
    /// Hidden discovery obligations to recheck on the executor.
    pub verification_obligations: Vec<HostObservation>,
}

/// Executor-side discovery request shipped to the runner.
#[derive(Debug, Clone, Facet)]
pub struct DiscoverRequest {
    /// Target executor selector. Current runners may ignore this if they only expose one executor.
    pub executor: ExecutorSelector,
    /// Lowered module used to resolve global calls from the shipped closure.
    pub module: TransportModule,
    /// Discovery probe closure to run with Host access.
    pub probe: ShippedFunction,
}

/// Result of evaluating a shipped discovery probe on the executor.
#[derive(Debug, Clone, Facet)]
pub struct DiscoverResult {
    pub value: Option<TransportValue>,
    pub poison: Option<TransportPoisonError>,
}

#[derive(Debug, Clone, Facet, PartialEq, Eq)]
#[repr(u8)]
pub enum RunFailureKind {
    User,
    VerificationFailure,
}

/// Request to run a sandboxed command.
#[derive(Debug, Clone, Facet)]
pub struct RunRequest {
    /// Target executor selector. Current runners may ignore this if they are the selected executor.
    pub executor: ExecutorSelector,

    /// Program path for executor-side `exec` requests.
    /// Literal paths are passed through as-is; `Path` values are resolved inside the VFS.
    pub program: RunArg,

    /// Arguments passed to the program (no shell parsing).
    pub args: Vec<RunArg>,

    /// CAS trees to mount into the action prefix.
    pub tree_mounts: Vec<RunTreeMount>,

    /// Individual output files from other node manifests to materialize into the action prefix.
    pub file_mounts: Vec<RunFileMount>,

    // r[impl exec.writable]
    /// Writable directories to create within the action prefix.
    pub writable_roots: Vec<String>,

    /// Additional directories to create within writable roots before launch.
    pub create_dirs: Vec<String>,

    /// Optional working directory within the action prefix.
    /// If unset, runner uses the action prefix root.
    pub cwd: Option<String>,

    /// Key/value env vars to set for the process.
    pub env: Vec<(String, RunArg)>,

    /// Whether to clear the environment before setting `env`.
    pub clear_env: bool,

    /// Optional evaluator payload to run on the executor after process completion.
    pub eval: Option<RunEvalPayload>,

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
    pub observer_value: Option<TransportValue>,
    pub poison: Option<TransportPoisonError>,
    pub error_kind: Option<RunFailureKind>,
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
    /// Run a sandboxed command with hermetic inputs/outputs via CAS/VFS.
    async fn run(&self, request: RunRequest, progress: roam::Tx<StoreProgress>) -> RunResult;

    /// Evaluate a shipped discovery probe on the executor host.
    async fn discover(&self, request: DiscoverRequest) -> DiscoverResult;
}

#[cfg(test)]
mod tests {
    use super::{DiscoverResult, RunArg};
    use vx_eval::{HostObservation, TransportStruct, TransportValue};

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

    #[test]
    fn discover_result_with_nested_discovered_struct_fields_serializes() {
        let result = DiscoverResult {
            value: Some(TransportValue::Struct(TransportStruct {
                type_name: "Toolchain".to_string(),
                fields: vec![
                    (
                        "os".to_string(),
                        TransportValue::Discovered {
                            value: Box::new(TransportValue::String("linux".to_string())),
                            obligations: vec![HostObservation::Os {
                                value: "linux".to_string(),
                            }],
                        },
                    ),
                    (
                        "arch".to_string(),
                        TransportValue::Discovered {
                            value: Box::new(TransportValue::String("aarch64".to_string())),
                            obligations: vec![HostObservation::Arch {
                                value: "aarch64".to_string(),
                            }],
                        },
                    ),
                    (
                        "cc".to_string(),
                        TransportValue::Discovered {
                            value: Box::new(TransportValue::String("/usr/bin/cc".to_string())),
                            obligations: vec![HostObservation::Which {
                                name: "cc".to_string(),
                                expected_path: Some("/usr/bin/cc".to_string()),
                            }],
                        },
                    ),
                    (
                        "archive".to_string(),
                        TransportValue::Discovered {
                            value: Box::new(TransportValue::String("/usr/bin/ar".to_string())),
                            obligations: vec![HostObservation::Which {
                                name: "ar".to_string(),
                                expected_path: Some("/usr/bin/ar".to_string()),
                            }],
                        },
                    ),
                ],
            })),
            poison: None,
        };

        let bytes = facet_postcard::to_vec(&result).expect("discover result should serialize");
        let roundtrip: DiscoverResult =
            facet_postcard::from_slice(&bytes).expect("discover result should deserialize");
        assert!(matches!(roundtrip.value, Some(TransportValue::Struct(_))));
        assert!(roundtrip.poison.is_none());
    }
}
