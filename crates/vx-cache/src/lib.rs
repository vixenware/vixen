//! Cache key computation for vixen build artifacts.
//!
//! This crate centralizes all cache key computation logic to ensure
//! consistency between daemon (for cache lookups) and execd (for cache storage).
//!
//! Cache keys are blake3 hashes of all inputs that affect the build output.

use facet_postcard::to_vec;
use vx_runner_proto::{CcCompileRequest, CcLinkRequest, RunRequest, RustCompileRequest};
use vx_store_proto::Blake3Hash;

/// Current cache key schema version for Rust compilation.
/// Bump this when the cache key format changes.
pub const RUST_CACHE_KEY_VERSION: u32 = 1;

/// Current cache key schema version for C/C++ compilation.
/// Version 2: Added macos_commandlinetools_version to cache key
pub const CC_CACHE_KEY_VERSION: u32 = 2;

/// Current cache key schema version for generic tool execution.
/// Version 5: Added direct program hashing for executor-side exec requests.
/// Version 6: Hash shipped observers directly instead of serializing whole modules.
/// Version 7: Include executor selector identity.
pub const RUN_CACHE_KEY_VERSION: u32 = 7;

/// Compute a cache key for a Rust compile request.
///
/// The cache key includes all inputs that affect the build output:
/// - Toolchain manifest
/// - Source tree manifest
/// - Crate metadata (name, type, edition, root)
/// - Target triple and profile
/// - Dependencies (sorted by extern_name for determinism)
pub fn rust_compile_cache_key(request: &RustCompileRequest) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();

    hasher.update(b"rust-compile-v");
    hasher.update(&RUST_CACHE_KEY_VERSION.to_le_bytes());
    hasher.update(b"\n");

    hasher.update(b"toolchain:");
    hasher.update(&request.toolchain_manifest.0);
    hasher.update(b"\n");

    hasher.update(b"source:");
    hasher.update(&request.source_manifest.0);
    hasher.update(b"\n");

    hasher.update(b"crate_root:");
    hasher.update(request.crate_root.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"crate_name:");
    hasher.update(request.crate_name.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"crate_type:");
    hasher.update(request.crate_type.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"edition:");
    hasher.update(request.edition.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"target:");
    hasher.update(request.target_triple.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"profile:");
    hasher.update(request.profile.as_bytes());
    hasher.update(b"\n");

    // Hash deps (sorted by extern_name for determinism)
    let mut deps: Vec<_> = request.deps.iter().collect();
    deps.sort_by(|a, b| a.extern_name.cmp(&b.extern_name));

    for dep in deps {
        hasher.update(b"dep:");
        hasher.update(dep.extern_name.as_bytes());
        hasher.update(b":");
        hasher.update(&dep.manifest_hash.0);
        hasher.update(b"\n");
    }

    Blake3Hash(*hasher.finalize().as_bytes())
}

/// Compute a cache key for a C/C++ compile request.
///
/// The cache key includes all inputs that affect the build output:
/// - Toolchain manifest (Zig version)
/// - Source tree manifest
/// - Source file path
/// - Target triple and profile
/// - Include paths (sorted for determinism)
/// - Defines (sorted for determinism)
/// - CommandLineTools version on macOS (for system linker/headers)
pub fn cc_compile_cache_key(request: &CcCompileRequest) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();

    hasher.update(b"cc-compile-v");
    hasher.update(&CC_CACHE_KEY_VERSION.to_le_bytes());
    hasher.update(b"\n");

    hasher.update(b"toolchain:");
    hasher.update(&request.toolchain_manifest.0);
    hasher.update(b"\n");

    hasher.update(b"source:");
    hasher.update(&request.source_manifest.0);
    hasher.update(b"\n");

    hasher.update(b"source_path:");
    hasher.update(request.source_path.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"target:");
    hasher.update(request.target_triple.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"profile:");
    hasher.update(request.profile.as_bytes());
    hasher.update(b"\n");

    // Hash CommandLineTools version (macOS only)
    if let Some(ref version) = request.macos_commandlinetools_version {
        hasher.update(b"macos_clt:");
        hasher.update(version.as_bytes());
        hasher.update(b"\n");
    }

    // Hash include paths (sorted for determinism)
    let mut includes: Vec<_> = request.include_paths.iter().collect();
    includes.sort();
    for include in includes {
        hasher.update(b"include:");
        hasher.update(include.as_bytes());
        hasher.update(b"\n");
    }

    // Hash defines (sorted for determinism)
    let mut defines: Vec<_> = request.defines.iter().collect();
    defines.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, value) in defines {
        hasher.update(b"define:");
        hasher.update(key.as_bytes());
        if let Some(v) = value {
            hasher.update(b"=");
            hasher.update(v.as_bytes());
        }
        hasher.update(b"\n");
    }

    Blake3Hash(*hasher.finalize().as_bytes())
}

/// Compute a cache key for a C/C++ link request.
///
/// The cache key includes all inputs that affect the build output:
/// - Toolchain manifest (Zig version)
/// - Object file manifests (sorted for determinism)
/// - Binary name
/// - Target triple
/// - Libraries (sorted for determinism)
/// - CommandLineTools version on macOS (for system linker)
pub fn cc_link_cache_key(request: &CcLinkRequest) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();

    hasher.update(b"cc-link-v");
    hasher.update(&CC_CACHE_KEY_VERSION.to_le_bytes());
    hasher.update(b"\n");

    hasher.update(b"toolchain:");
    hasher.update(&request.toolchain_manifest.0);
    hasher.update(b"\n");

    hasher.update(b"binary:");
    hasher.update(request.binary_name.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"target:");
    hasher.update(request.target_triple.as_bytes());
    hasher.update(b"\n");

    // Hash CommandLineTools version (macOS only)
    if let Some(ref version) = request.macos_commandlinetools_version {
        hasher.update(b"macos_clt:");
        hasher.update(version.as_bytes());
        hasher.update(b"\n");
    }

    // Hash object manifests (sorted for determinism)
    let mut objects: Vec<_> = request.object_manifests.iter().collect();
    objects.sort_by_key(|h| h.0);
    for obj in objects {
        hasher.update(b"object:");
        hasher.update(&obj.0);
        hasher.update(b"\n");
    }

    // Hash libraries (sorted for determinism)
    let mut libs: Vec<_> = request.libs.iter().collect();
    libs.sort();
    for lib in libs {
        hasher.update(b"lib:");
        hasher.update(lib.as_bytes());
        hasher.update(b"\n");
    }

    Blake3Hash(*hasher.finalize().as_bytes())
}

/// Compute a cache key for a generic `run` request.
///
/// The cache key includes:
/// - Toolchain manifest
/// - Mounted tree manifests (sorted by mount path)
/// - Mounted files (sorted by destination path)
/// - Program + args + cwd
/// - Environment variables (sorted by key/value)
/// - Declared outputs (sorted by logical/path)
pub fn run_cache_key(request: &RunRequest) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();

    hasher.update(b"run-v");
    hasher.update(&RUN_CACHE_KEY_VERSION.to_le_bytes());
    hasher.update(b"\n");

    hasher.update(b"executor:");
    hasher.update(request.executor.backend.as_tag().as_bytes());
    hasher.update(b":");
    hasher.update(request.executor.id.as_bytes());
    hasher.update(b"\n");

    hasher.update(b"toolchain:");
    hasher.update(&request.toolchain_manifest.0);
    hasher.update(b"\n");

    match &request.program {
        Some(program) => {
            hasher.update(b"program:direct:");
            hasher.update(match program {
                vx_runner_proto::RunArg::Literal(_) => b"literal:",
                vx_runner_proto::RunArg::Path(_) => b"path:",
            });
            hasher.update(program.as_str().as_bytes());
            hasher.update(b"\n");
        }
        None => {
            hasher.update(b"program:toolchain:");
            hasher.update(request.program_rel.as_bytes());
            hasher.update(b"\n");
        }
    }

    if let Some(cwd) = &request.cwd {
        hasher.update(b"cwd:");
        hasher.update(cwd.as_bytes());
        hasher.update(b"\n");
    }

    if let Some(v) = &request.macos_commandlinetools_version {
        hasher.update(b"macos_clt:");
        hasher.update(v.as_bytes());
        hasher.update(b"\n");
    }

    hasher.update(b"clear_env:");
    hasher.update(&[request.clear_env as u8]);
    hasher.update(b"\n");

    if let Some(eval) = &request.eval {
        hasher.update(b"eval:");
        if let Some(observer) = &eval.observer {
            hasher.update(b"observer:");
            let observer_bytes = to_vec(observer).expect("shipped observer must serialize");
            hasher.update(&observer_bytes);
        } else {
            hasher.update(b"module:");
            let module_bytes = to_vec(&eval.module).expect("transport module must serialize");
            hasher.update(&module_bytes);
        }
        hasher.update(b"\n");

        hasher.update(b"verify:");
        let verification_bytes = to_vec(&eval.verification_obligations)
            .expect("verification obligations must serialize");
        hasher.update(&verification_bytes);
        hasher.update(b"\n");
    }

    for arg in &request.args {
        hasher.update(b"arg:");
        hasher.update(match arg {
            vx_runner_proto::RunArg::Literal(_) => b"literal:",
            vx_runner_proto::RunArg::Path(_) => b"path:",
        });
        hasher.update(arg.as_str().as_bytes());
        hasher.update(b"\n");
    }

    let mut envs: Vec<_> = request.env.iter().collect();
    envs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (k, v) in envs {
        hasher.update(b"env:");
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(match v {
            vx_runner_proto::RunArg::Literal(_) => b"literal:",
            vx_runner_proto::RunArg::Path(_) => b"path:",
        });
        hasher.update(v.as_str().as_bytes());
        hasher.update(b"\n");
    }

    let mut tree_mounts: Vec<_> = request.tree_mounts.iter().collect();
    tree_mounts.sort_by(|a, b| {
        a.mount
            .cmp(&b.mount)
            .then_with(|| a.manifest.0.cmp(&b.manifest.0))
    });
    for m in tree_mounts {
        hasher.update(b"tree:");
        hasher.update(m.mount.as_bytes());
        hasher.update(b":");
        hasher.update(&m.manifest.0);
        hasher.update(b"\n");
    }

    let mut writable_roots: Vec<_> = request.writable_roots.iter().collect();
    writable_roots.sort();
    for root in writable_roots {
        hasher.update(b"writable:");
        hasher.update(root.as_bytes());
        hasher.update(b"\n");
    }

    let mut file_mounts: Vec<_> = request.file_mounts.iter().collect();
    file_mounts.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.logical.cmp(&b.logical))
            .then_with(|| a.manifest.0.cmp(&b.manifest.0))
    });
    for m in file_mounts {
        hasher.update(b"file:");
        hasher.update(m.path.as_bytes());
        hasher.update(b":");
        hasher.update(m.logical.as_bytes());
        hasher.update(b":");
        hasher.update(&m.manifest.0);
        hasher.update(b"\n");
    }

    let mut outputs: Vec<_> = request.outputs.iter().collect();
    outputs.sort_by(|a, b| a.logical.cmp(&b.logical).then_with(|| a.path.cmp(&b.path)));
    for o in outputs {
        hasher.update(b"out:");
        hasher.update(o.logical.as_bytes());
        hasher.update(b":");
        hasher.update(o.path.as_bytes());
        hasher.update(b":");
        hasher.update(&[o.executable as u8]);
        hasher.update(b"\n");
    }

    Blake3Hash(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vx_runner_proto::{
        ExecutorSelector, RunArg, RunEvalPayload, RunRequest, RustDep, ShippedFunction,
        TransportBlock, TransportExpr, TransportFnDef, TransportModule, TransportParam,
        TransportStmt,
    };

    #[test]
    fn test_cache_key_determinism() {
        let request = RustCompileRequest {
            toolchain_manifest: Blake3Hash([1; 32]),
            source_manifest: Blake3Hash([2; 32]),
            crate_root: "src/lib.rs".to_string(),
            crate_name: "mylib".to_string(),
            crate_type: "lib".to_string(),
            edition: "2021".to_string(),
            target_triple: "aarch64-apple-darwin".to_string(),
            profile: "release".to_string(),
            deps: vec![],
            registry_crate_manifest: None,
        };

        let key1 = rust_compile_cache_key(&request);
        let key2 = rust_compile_cache_key(&request);

        assert_eq!(key1, key2, "cache key should be deterministic");
    }

    #[test]
    fn test_cache_key_deps_order_independent() {
        let dep_a = RustDep {
            extern_name: "aaa".to_string(),
            manifest_hash: Blake3Hash([10; 32]),
            registry_crate_manifest: None,
        };
        let dep_b = RustDep {
            extern_name: "bbb".to_string(),
            manifest_hash: Blake3Hash([20; 32]),
            registry_crate_manifest: None,
        };

        let request1 = RustCompileRequest {
            toolchain_manifest: Blake3Hash([1; 32]),
            source_manifest: Blake3Hash([2; 32]),
            crate_root: "src/lib.rs".to_string(),
            crate_name: "mylib".to_string(),
            crate_type: "lib".to_string(),
            edition: "2021".to_string(),
            target_triple: "aarch64-apple-darwin".to_string(),
            profile: "release".to_string(),
            deps: vec![dep_a.clone(), dep_b.clone()],
            registry_crate_manifest: None,
        };

        let request2 = RustCompileRequest {
            toolchain_manifest: Blake3Hash([1; 32]),
            source_manifest: Blake3Hash([2; 32]),
            crate_root: "src/lib.rs".to_string(),
            crate_name: "mylib".to_string(),
            crate_type: "lib".to_string(),
            edition: "2021".to_string(),
            target_triple: "aarch64-apple-darwin".to_string(),
            profile: "release".to_string(),
            deps: vec![dep_b, dep_a], // reversed order
            registry_crate_manifest: None,
        };

        let key1 = rust_compile_cache_key(&request1);
        let key2 = rust_compile_cache_key(&request2);

        assert_eq!(key1, key2, "cache key should be independent of dep order");
    }

    #[test]
    fn run_cache_key_distinguishes_literal_and_path_args() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            toolchain_manifest: Blake3Hash([1; 32]),
            program_rel: "bin/rustc".to_string(),
            program: None,
            args: vec![RunArg::literal("src/main.rs")],
            tree_mounts: vec![],
            file_mounts: vec![],
            outputs: vec![],
            writable_roots: vec!["out".to_string()],
            create_dirs: vec![],
            cwd: None,
            env: vec![],
            clear_env: true,
            eval: None,
            macos_commandlinetools_version: None,
        };
        let path_request = RunRequest {
            args: vec![RunArg::path("src/main.rs")],
            ..request.clone()
        };

        assert_ne!(run_cache_key(&request), run_cache_key(&path_request));
    }

    #[test]
    fn run_cache_key_includes_evaluator_payload() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            toolchain_manifest: Blake3Hash([1; 32]),
            program_rel: "bin/tool".to_string(),
            program: None,
            args: vec![RunArg::literal("--version")],
            tree_mounts: vec![],
            file_mounts: vec![],
            outputs: vec![],
            writable_roots: vec!["out".to_string()],
            create_dirs: vec![],
            cwd: None,
            env: vec![],
            clear_env: true,
            eval: None,
            macos_commandlinetools_version: None,
        };
        let first_eval = RunEvalPayload {
            module: TransportModule {
                source_id: "test://run-cache-key".to_string(),
                functions: vec![TransportFnDef {
                    name: "observe".to_string(),
                    params: vec![],
                    keyword_params: vec![],
                    body: TransportBlock {
                        stmts: vec![TransportStmt::Expr(TransportExpr::LitInt(1))],
                    },
                }],
                top_level: TransportBlock { stmts: vec![] },
                loaded_modules: vec![],
            },
            observer: None,
            verification_obligations: vec![],
        };
        let second_eval = RunEvalPayload {
            module: TransportModule {
                source_id: "test://run-cache-key".to_string(),
                functions: vec![TransportFnDef {
                    name: "observe".to_string(),
                    params: vec![],
                    keyword_params: vec![],
                    body: TransportBlock {
                        stmts: vec![TransportStmt::Expr(TransportExpr::LitInt(2))],
                    },
                }],
                top_level: TransportBlock { stmts: vec![] },
                loaded_modules: vec![],
            },
            observer: None,
            verification_obligations: vec![],
        };

        let first_request = RunRequest {
            eval: Some(first_eval),
            ..request.clone()
        };
        let second_request = RunRequest {
            eval: Some(second_eval),
            ..request
        };

        assert_ne!(
            run_cache_key(&first_request),
            run_cache_key(&second_request)
        );
    }

    #[test]
    fn run_cache_key_includes_direct_program_identity() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            toolchain_manifest: Blake3Hash([1; 32]),
            program_rel: "bin/tool".to_string(),
            program: None,
            args: vec![RunArg::literal("--version")],
            tree_mounts: vec![],
            file_mounts: vec![],
            outputs: vec![],
            writable_roots: vec!["out".to_string()],
            create_dirs: vec![],
            cwd: None,
            env: vec![],
            clear_env: true,
            eval: None,
            macos_commandlinetools_version: None,
        };
        let direct_request = RunRequest {
            program: Some(RunArg::literal("/bin/sh")),
            ..request.clone()
        };

        assert_ne!(run_cache_key(&request), run_cache_key(&direct_request));
    }

    #[test]
    fn run_cache_key_handles_shipped_observer_payloads() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            toolchain_manifest: Blake3Hash([1; 32]),
            program_rel: String::new(),
            program: Some(RunArg::literal("/usr/bin/cc")),
            args: vec![
                RunArg::literal("-x"),
                RunArg::literal("c"),
                RunArg::path("probe.c"),
                RunArg::literal("-c"),
                RunArg::literal("-o"),
                RunArg::path("out/probe.o"),
            ],
            tree_mounts: vec![],
            file_mounts: vec![],
            outputs: vec![],
            writable_roots: vec!["out".to_string()],
            create_dirs: vec![],
            cwd: None,
            env: vec![],
            clear_env: false,
            eval: Some(RunEvalPayload {
                module: TransportModule {
                    source_id: "test://run-cache-key".to_string(),
                    functions: vec![],
                    top_level: TransportBlock { stmts: vec![] },
                    loaded_modules: vec![],
                },
                observer: Some(ShippedFunction {
                    module_id: "test://run-cache-key".to_string(),
                    code_identity: "observe:probe".to_string(),
                    def: TransportFnDef {
                        name: "observe".to_string(),
                        params: vec![
                            TransportParam {
                                name: "process".to_string(),
                            },
                            TransportParam {
                                name: "_tree".to_string(),
                            },
                        ],
                        keyword_params: vec![],
                        body: TransportBlock {
                            stmts: vec![TransportStmt::Expr(TransportExpr::Binary {
                                op: vx_runner_proto::TransportBinOp::Eq,
                                left: Box::new(TransportExpr::Field {
                                    receiver: Box::new(TransportExpr::Var("process".to_string())),
                                    field: "exit_code".to_string(),
                                    provenance: None,
                                }),
                                right: Box::new(TransportExpr::LitInt(0)),
                                provenance: None,
                            })],
                        },
                    },
                    captures: vec![],
                }),
                verification_obligations: vec![],
            }),
            macos_commandlinetools_version: None,
        };

        let first = run_cache_key(&request);
        let second = run_cache_key(&request);
        assert_eq!(first, second);
    }
}
