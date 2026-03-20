//! Cache key computation for Vixen exec requests.
//!
//! The daemon and runner share this crate so both sides agree on how a
//! sandboxed `run` request should be keyed.

use facet_postcard::to_vec;
use vx_runner_proto::{RunArg, RunMountMode, RunRequest, RunTrackedObservation};
use vx_store_proto::Blake3Hash;

/// Current cache key schema version for generic tool execution.
/// Version 10: tracked mounts contribute only structural identity; opaque mounts
/// keep whole-bundle identity in the request key.
pub const RUN_CACHE_KEY_VERSION: u32 = 10;

/// Compute a cache key for a generic `run` request.
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

    hasher.update(b"program:");
    update_run_arg(&mut hasher, &request.program);
    hasher.update(b"\n");

    if let Some(cwd) = &request.cwd {
        hasher.update(b"cwd:");
        hasher.update(cwd.as_bytes());
        hasher.update(b"\n");
    }

    if let Some(version) = &request.macos_commandlinetools_version {
        hasher.update(b"macos_clt:");
        hasher.update(version.as_bytes());
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
        update_run_arg(&mut hasher, arg);
        hasher.update(b"\n");
    }

    let mut envs: Vec<_> = request.env.iter().collect();
    envs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (key, value) in envs {
        hasher.update(b"env:");
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        update_run_arg(&mut hasher, value);
        hasher.update(b"\n");
    }

    let mut tree_mounts: Vec<_> = request.tree_mounts.iter().collect();
    tree_mounts.sort_by(|a, b| {
        a.mount
            .cmp(&b.mount)
            .then_with(|| a.mode.cmp(&b.mode))
            .then_with(|| a.manifest.0.cmp(&b.manifest.0))
    });
    for mount in tree_mounts {
        hasher.update(b"tree:");
        hasher.update(mount.mount.as_bytes());
        hasher.update(b":");
        match mount.mode {
            RunMountMode::Tracked => {
                hasher.update(b"tracked");
            }
            RunMountMode::Opaque => {
                hasher.update(b"opaque:");
                hasher.update(&mount.manifest.0);
            }
        }
        hasher.update(b"\n");
    }

    let mut writable_roots: Vec<_> = request.writable_roots.iter().collect();
    writable_roots.sort();
    for root in writable_roots {
        hasher.update(b"writable:");
        hasher.update(root.as_bytes());
        hasher.update(b"\n");
    }

    let mut create_dirs: Vec<_> = request.create_dirs.iter().collect();
    create_dirs.sort();
    for dir in create_dirs {
        hasher.update(b"mkdir:");
        hasher.update(dir.as_bytes());
        hasher.update(b"\n");
    }

    let mut file_mounts: Vec<_> = request.file_mounts.iter().collect();
    file_mounts.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.logical.cmp(&b.logical))
            .then_with(|| a.mode.cmp(&b.mode))
            .then_with(|| a.manifest.0.cmp(&b.manifest.0))
    });
    for mount in file_mounts {
        hasher.update(b"file:");
        hasher.update(mount.path.as_bytes());
        hasher.update(b":");
        hasher.update(mount.logical.as_bytes());
        hasher.update(b":");
        match mount.mode {
            RunMountMode::Tracked => {
                hasher.update(b"tracked");
            }
            RunMountMode::Opaque => {
                hasher.update(b"opaque:");
                hasher.update(&mount.manifest.0);
            }
        }
        hasher.update(b"\n");
    }

    Blake3Hash(*hasher.finalize().as_bytes())
}

/// Compute a canonical fingerprint for tracked observations returned by a run.
///
/// This is independent of observation order and stable across transports.
pub fn tracked_observations_fingerprint(observations: &[RunTrackedObservation]) -> Blake3Hash {
    let mut normalized = observations.to_vec();
    normalized.sort_by(compare_tracked_observations);

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tracked-observations-v1\n");

    for observation in &normalized {
        match observation {
            RunTrackedObservation::File {
                path,
                content_hash,
                blob_node,
                size,
            } => {
                hasher.update(b"file:");
                hasher.update(path.as_bytes());
                hasher.update(b":");
                hasher.update(&content_hash.0);
                hasher.update(b":");
                if let Some(blob_node) = blob_node {
                    hasher.update(&blob_node.0.0);
                } else {
                    hasher.update(b"-");
                }
                hasher.update(b":");
                hasher.update(&size.to_le_bytes());
                hasher.update(b"\n");
            }
            RunTrackedObservation::Directory {
                path,
                directory_node,
            } => {
                hasher.update(b"dir:");
                hasher.update(path.as_bytes());
                hasher.update(b":");
                hasher.update(&directory_node.0.0);
                hasher.update(b"\n");
            }
            RunTrackedObservation::LookupMiss {
                path,
                parent_path,
                directory_node,
            } => {
                hasher.update(b"miss:");
                hasher.update(path.as_bytes());
                hasher.update(b":");
                hasher.update(parent_path.as_bytes());
                hasher.update(b":");
                hasher.update(&directory_node.0.0);
                hasher.update(b"\n");
            }
        }
    }

    Blake3Hash(*hasher.finalize().as_bytes())
}

fn compare_tracked_observations(
    left: &RunTrackedObservation,
    right: &RunTrackedObservation,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    fn kind_rank(observation: &RunTrackedObservation) -> u8 {
        match observation {
            RunTrackedObservation::File { .. } => 0,
            RunTrackedObservation::Directory { .. } => 1,
            RunTrackedObservation::LookupMiss { .. } => 2,
        }
    }

    fn path(observation: &RunTrackedObservation) -> &str {
        match observation {
            RunTrackedObservation::File { path, .. }
            | RunTrackedObservation::Directory { path, .. }
            | RunTrackedObservation::LookupMiss { path, .. } => path,
        }
    }

    fn parent_path(observation: &RunTrackedObservation) -> &str {
        match observation {
            RunTrackedObservation::LookupMiss { parent_path, .. } => parent_path,
            _ => "",
        }
    }

    fn primary_hash(observation: &RunTrackedObservation) -> &[u8; 32] {
        match observation {
            RunTrackedObservation::File { content_hash, .. } => &content_hash.0,
            RunTrackedObservation::Directory { directory_node, .. }
            | RunTrackedObservation::LookupMiss { directory_node, .. } => &directory_node.0.0,
        }
    }

    fn secondary_hash(observation: &RunTrackedObservation) -> Option<&[u8; 32]> {
        match observation {
            RunTrackedObservation::File { blob_node, .. } => {
                blob_node.as_ref().map(|node| &node.0.0)
            }
            _ => None,
        }
    }

    fn size(observation: &RunTrackedObservation) -> u64 {
        match observation {
            RunTrackedObservation::File { size, .. } => *size,
            _ => 0,
        }
    }

    path(left)
        .cmp(path(right))
        .then_with(|| kind_rank(left).cmp(&kind_rank(right)))
        .then_with(|| parent_path(left).cmp(parent_path(right)))
        .then_with(|| primary_hash(left).cmp(primary_hash(right)))
        .then_with(|| match (secondary_hash(left), secondary_hash(right)) {
            (Some(left), Some(right)) => left.cmp(right),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| size(left).cmp(&size(right)))
}

fn update_run_arg(hasher: &mut blake3::Hasher, arg: &RunArg) {
    match arg {
        RunArg::Literal(_) => hasher.update(b"literal:"),
        RunArg::Path(_) => hasher.update(b"path:"),
    };
    hasher.update(arg.as_str().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use vx_runner_proto::{
        ExecutorSelector, RunEvalPayload, RunMountMode, RunTrackedObservation, RunTreeMount,
        ShippedFunction, TransportBlock, TransportExpr, TransportFnDef, TransportModule,
        TransportParam, TransportStmt,
    };
    use vx_store_proto::NodeHash;

    fn sample_request() -> RunRequest {
        RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("tool"),
            args: vec![RunArg::literal("--version")],
            tree_mounts: vec![],
            file_mounts: vec![],
            writable_roots: vec!["out".to_string()],
            create_dirs: vec![],
            cwd: None,
            env: vec![],
            clear_env: true,
            eval: None,
            macos_commandlinetools_version: None,
        }
    }

    #[test]
    fn run_cache_key_is_deterministic() {
        let request = sample_request();
        assert_eq!(run_cache_key(&request), run_cache_key(&request));
    }

    #[test]
    fn run_cache_key_distinguishes_literal_and_path_args() {
        let request = RunRequest {
            args: vec![RunArg::literal("src/main.rs")],
            ..sample_request()
        };
        let path_request = RunRequest {
            args: vec![RunArg::path("src/main.rs")],
            ..sample_request()
        };

        assert_ne!(run_cache_key(&request), run_cache_key(&path_request));
    }

    #[test]
    fn run_cache_key_includes_evaluator_payload() {
        let request = sample_request();
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
                    provenance: None,
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
                    provenance: None,
                }],
                top_level: TransportBlock { stmts: vec![] },
                loaded_modules: vec![],
            },
            observer: None,
            verification_obligations: vec![],
        };

        let first = RunRequest {
            eval: Some(first_eval),
            ..request.clone()
        };
        let second = RunRequest {
            eval: Some(second_eval),
            ..request
        };

        assert_ne!(run_cache_key(&first), run_cache_key(&second));
    }

    #[test]
    fn run_cache_key_prefers_observer_payload_over_full_module() {
        let observer = ShippedFunction {
            module_id: "test://run-cache-key".to_string(),
            code_identity: "observe".to_string(),
            def: TransportFnDef {
                name: "observe".to_string(),
                params: vec![TransportParam {
                    name: "process".to_string(),
                }],
                keyword_params: vec![],
                body: TransportBlock {
                    stmts: vec![TransportStmt::Expr(TransportExpr::LitInt(7))],
                },
                provenance: None,
            },
            captures: vec![],
        };
        let request = RunRequest {
            eval: Some(RunEvalPayload {
                module: TransportModule {
                    source_id: "test://run-cache-key".to_string(),
                    functions: vec![],
                    top_level: TransportBlock { stmts: vec![] },
                    loaded_modules: vec![],
                },
                observer: Some(observer),
                verification_obligations: vec![],
            }),
            ..sample_request()
        };

        let first = run_cache_key(&request);
        let second = run_cache_key(&request);
        assert_eq!(first, second);
    }

    #[test]
    fn run_cache_key_distinguishes_mount_modes() {
        let tracked = RunRequest {
            tree_mounts: vec![RunTreeMount {
                mount: "toolchain".to_string(),
                manifest: Blake3Hash([7; 32]),
                mode: RunMountMode::Tracked,
            }],
            ..sample_request()
        };
        let opaque = RunRequest {
            tree_mounts: vec![RunTreeMount {
                mount: "toolchain".to_string(),
                manifest: Blake3Hash([7; 32]),
                mode: RunMountMode::Opaque,
            }],
            ..sample_request()
        };

        assert_ne!(run_cache_key(&tracked), run_cache_key(&opaque));
    }

    #[test]
    fn run_cache_key_ignores_tracked_mount_manifest_identity() {
        let first = RunRequest {
            tree_mounts: vec![RunTreeMount {
                mount: "src".to_string(),
                manifest: Blake3Hash([1; 32]),
                mode: RunMountMode::Tracked,
            }],
            ..sample_request()
        };
        let second = RunRequest {
            tree_mounts: vec![RunTreeMount {
                mount: "src".to_string(),
                manifest: Blake3Hash([2; 32]),
                mode: RunMountMode::Tracked,
            }],
            ..sample_request()
        };

        assert_eq!(run_cache_key(&first), run_cache_key(&second));
    }

    #[test]
    fn run_cache_key_keeps_opaque_mount_manifest_identity() {
        let first = RunRequest {
            tree_mounts: vec![RunTreeMount {
                mount: "toolchain".to_string(),
                manifest: Blake3Hash([1; 32]),
                mode: RunMountMode::Opaque,
            }],
            ..sample_request()
        };
        let second = RunRequest {
            tree_mounts: vec![RunTreeMount {
                mount: "toolchain".to_string(),
                manifest: Blake3Hash([2; 32]),
                mode: RunMountMode::Opaque,
            }],
            ..sample_request()
        };

        assert_ne!(run_cache_key(&first), run_cache_key(&second));
    }

    #[test]
    fn tracked_observations_fingerprint_is_order_independent() {
        let file = RunTrackedObservation::File {
            path: "src/main.c".to_string(),
            content_hash: Blake3Hash::from_bytes(b"main"),
            blob_node: None,
            size: 4,
        };
        let directory = RunTrackedObservation::Directory {
            path: "src/include".to_string(),
            directory_node: NodeHash(Blake3Hash::from_bytes(b"include-dir")),
        };
        let miss = RunTrackedObservation::LookupMiss {
            path: "src/generated/config.h".to_string(),
            parent_path: "src/generated".to_string(),
            directory_node: NodeHash(Blake3Hash::from_bytes(b"generated-dir")),
        };

        let left =
            tracked_observations_fingerprint(&[file.clone(), directory.clone(), miss.clone()]);
        let right = tracked_observations_fingerprint(&[miss, file, directory]);

        assert_eq!(left, right);
    }

    #[test]
    fn tracked_observations_fingerprint_distinguishes_observation_kind() {
        let file = RunTrackedObservation::File {
            path: "src/generated".to_string(),
            content_hash: Blake3Hash::from_bytes(b"payload"),
            blob_node: None,
            size: 7,
        };
        let directory = RunTrackedObservation::Directory {
            path: "src/generated".to_string(),
            directory_node: NodeHash(Blake3Hash::from_bytes(b"payload")),
        };

        assert_ne!(
            tracked_observations_fingerprint(&[file]),
            tracked_observations_fingerprint(&[directory])
        );
    }
}
