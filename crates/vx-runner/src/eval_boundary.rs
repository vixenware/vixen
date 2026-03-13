//! Executor-boundary conversion for shipped evaluator results.
//!
//! Values produced on the runner can still point at executor-local filesystem
//! state. Before they cross back over RPC, blobs and trees must be committed
//! into CAS-backed runtime values.

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::pin::Pin;
use std::sync::Arc;

use vx_eval::{
    BlobValue, DiscoveredValue, Engine, MapValue, PathValue, StructValue, TransportValue,
    TreeValue, Value, verify_host_obligations,
};
use vx_runner_proto::{DiscoverRequest, RunEvalPayload};
use vx_store_proto::chunking::{prepare_blob, store_prepared_blob};
use vx_store_proto::{
    BlobRef, CasNode, ContentHash, DirEntry, DirEntryKind, DirectoryNode, NodeHash, StoreClient,
};

type BoundaryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

// r[impl exec.observer.materialization]
pub fn commit_value_to_transport<'a>(
    cas: &'a StoreClient,
    value: Value,
) -> BoundaryFuture<'a, TransportValue> {
    Box::pin(async move {
        let value = commit_value(cas, value).await?;
        TransportValue::try_from(&value)
    })
}

// r[impl exec.observer.simple]
pub fn evaluate_shipped_observer<'a>(
    cas: &'a StoreClient,
    eval: &'a RunEvalPayload,
    exit_code: i32,
    stdout: &'a str,
    stderr: &'a str,
    sandbox_root: &'a PathValue,
) -> BoundaryFuture<'a, Option<TransportValue>> {
    Box::pin(async move {
        let Some(observer) = &eval.observer else {
            return Ok(None);
        };

        let observed = Engine::invoke_shipped(
            &eval.module,
            observer,
            vec![
                process_value(exit_code, stdout, stderr),
                sandbox_tree_value(sandbox_root),
            ],
        )
        .map_err(|error| error.to_string())?;

        commit_value_to_transport(cas, observed).await.map(Some)
    })
}

// r[exec.verifier]
pub fn evaluate_shipped_verification(eval: &RunEvalPayload) -> Result<(), String> {
    verify_host_obligations(&eval.verification_obligations)
}

// r[impl exec.discover]
pub fn evaluate_shipped_discover<'a>(
    cas: &'a StoreClient,
    request: &'a DiscoverRequest,
) -> BoundaryFuture<'a, TransportValue> {
    Box::pin(async move {
        let discovered = Engine::invoke_shipped_discover(&request.module, &request.probe)
            .map_err(|error| error.to_string())?;
        commit_value_to_transport(cas, discovered).await
    })
}

fn commit_value<'a>(cas: &'a StoreClient, value: Value) -> BoundaryFuture<'a, Value> {
    Box::pin(async move {
        match value {
            Value::Pending(_) => Err("pending values cannot cross the executor boundary".into()),
            Value::Path(path) => Err(format!(
                "Path values cannot cross the executor boundary: {}",
                path.as_str()
            )),
            Value::Host(_) => Err("Host values cannot cross the executor boundary".into()),
            Value::Discovered(discovered) => {
                let value = commit_value(cas, *discovered.value).await?;
                Ok(Value::Discovered(DiscoveredValue {
                    value: Box::new(value),
                    obligations: discovered.obligations,
                }))
            }
            Value::Blob(blob) => Ok(Value::Blob(commit_blob(cas, blob).await?)),
            Value::Tree(tree) => {
                let (root, _) = commit_tree(cas, tree).await?;
                Ok(Value::Tree(TreeValue::Store(root)))
            }
            Value::Tuple(values) => {
                let mut committed = Vec::with_capacity(values.len());
                for value in values.iter().cloned() {
                    committed.push(commit_value(cas, value).await?);
                }
                Ok(Value::Tuple(Arc::from(committed)))
            }
            Value::Array(values) => {
                let mut committed = Vec::with_capacity(values.len());
                for value in values.iter().cloned() {
                    committed.push(commit_value(cas, value).await?);
                }
                Ok(Value::Array(Arc::from(committed)))
            }
            Value::Map(map) => {
                let mut committed = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    committed.push((
                        commit_value(cas, key.clone()).await?,
                        commit_value(cas, value.clone()).await?,
                    ));
                }
                Ok(Value::Map(MapValue {
                    entries: Arc::from(committed),
                }))
            }
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields.iter() {
                    fields.push((name.clone(), commit_value(cas, value.clone()).await?));
                }
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
            other => Ok(other),
        }
    })
}

fn process_value(exit_code: i32, stdout: &str, stderr: &str) -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("Process"),
        fields: Arc::from([
            (Arc::from("exit_code"), Value::U64(exit_code.max(0) as u64)),
            (
                Arc::from("stdout"),
                Value::Bytes(Arc::from(stdout.as_bytes())),
            ),
            (
                Arc::from("stderr"),
                Value::Bytes(Arc::from(stderr.as_bytes())),
            ),
        ]),
    })
}

fn sandbox_tree_value(sandbox_root: &PathValue) -> Value {
    Value::Tree(TreeValue::FsDir(sandbox_root.clone()))
}

fn commit_blob<'a>(cas: &'a StoreClient, blob: BlobValue) -> BoundaryFuture<'a, BlobValue> {
    Box::pin(async move {
        let (blob_ref, executable) = match blob {
            BlobValue::Inline { bytes, executable } => {
                (store_blob_bytes(cas, bytes.as_ref()).await?, executable)
            }
            BlobValue::FsFile { path, executable } => {
                let bytes = fs::read(path.as_str())
                    .map_err(|error| format!("failed to read {}: {error}", path.as_str()))?;
                (store_blob_bytes(cas, &bytes).await?, executable)
            }
            BlobValue::Store {
                blob_ref,
                executable,
            } => (blob_ref, executable),
        };
        Ok(BlobValue::Store {
            blob_ref,
            executable,
        })
    })
}

fn commit_tree<'a>(cas: &'a StoreClient, tree: TreeValue) -> BoundaryFuture<'a, (NodeHash, u64)> {
    Box::pin(async move {
        match tree {
            TreeValue::Inline(entries) => commit_inline_tree(cas, entries).await,
            TreeValue::FsDir(path) => commit_fs_tree(cas, path).await,
            TreeValue::Store(root) => Ok((root, directory_total_size(cas, root).await?)),
        }
    })
}

fn commit_inline_tree<'a>(
    cas: &'a StoreClient,
    entries: Arc<[(Arc<str>, Value)]>,
) -> BoundaryFuture<'a, (NodeHash, u64)> {
    Box::pin(async move {
        let mut names = BTreeSet::new();
        let mut sorted: Vec<(Arc<str>, Value)> = entries.iter().cloned().collect();
        sorted.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));

        let mut dir_entries = Vec::with_capacity(sorted.len());
        let mut total_size = 0u64;
        for (name, value) in sorted {
            validate_tree_entry_name(name.as_ref())?;
            if !names.insert(name.to_string()) {
                return Err(format!("duplicate tree entry '{name}'"));
            }
            let (kind, size) = commit_tree_entry(cas, value).await?;
            total_size += size;
            dir_entries.push(DirEntry {
                name: name.to_string(),
                kind,
            });
        }
        store_directory_node(cas, dir_entries, total_size).await
    })
}

fn commit_fs_tree<'a>(
    cas: &'a StoreClient,
    root: PathValue,
) -> BoundaryFuture<'a, (NodeHash, u64)> {
    Box::pin(async move {
        let mut read_dir = fs::read_dir(root.as_str())
            .map_err(|error| format!("failed to read {}: {error}", root.as_str()))?;
        let mut names = Vec::new();
        while let Some(entry) = read_dir
            .next()
            .transpose()
            .map_err(|error| format!("failed to read {}: {error}", root.as_str()))?
        {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

        let mut dir_entries = Vec::with_capacity(names.len());
        let mut total_size = 0u64;
        for name in names {
            validate_tree_entry_name(&name)?;
            let child = root.join(&name);
            let metadata = fs::metadata(child.as_str())
                .map_err(|error| format!("failed to stat {}: {error}", child.as_str()))?;
            let (kind, size) = if metadata.is_dir() {
                let (hash, total_size) = commit_fs_tree(cas, child).await?;
                (DirEntryKind::Dir { hash, total_size }, total_size)
            } else {
                let bytes = fs::read(child.as_str())
                    .map_err(|error| format!("failed to read {}: {error}", child.as_str()))?;
                let blob_ref = store_blob_bytes(cas, &bytes).await?;
                let executable = is_executable_path(child.as_str())?;
                let size = blob_ref_size(&blob_ref);
                (dir_entry_kind_from_blob_ref(&blob_ref, executable), size)
            };
            total_size += size;
            dir_entries.push(DirEntry { name, kind });
        }
        store_directory_node(cas, dir_entries, total_size).await
    })
}

fn commit_tree_entry<'a>(
    cas: &'a StoreClient,
    value: Value,
) -> BoundaryFuture<'a, (DirEntryKind, u64)> {
    Box::pin(async move {
        match value {
            Value::Blob(blob) => {
                let (blob_ref, executable) = match blob {
                    BlobValue::Inline { bytes, executable } => {
                        (store_blob_bytes(cas, bytes.as_ref()).await?, executable)
                    }
                    BlobValue::FsFile { path, executable } => {
                        let bytes = fs::read(path.as_str()).map_err(|error| {
                            format!("failed to read {}: {error}", path.as_str())
                        })?;
                        (store_blob_bytes(cas, &bytes).await?, executable)
                    }
                    BlobValue::Store {
                        blob_ref,
                        executable,
                    } => (blob_ref, executable),
                };
                let size = blob_ref_size(&blob_ref);
                Ok((dir_entry_kind_from_blob_ref(&blob_ref, executable), size))
            }
            Value::Tree(tree) => {
                let (hash, total_size) = commit_tree(cas, tree).await?;
                Ok((DirEntryKind::Dir { hash, total_size }, total_size))
            }
            other => Err(format!(
                "tree entries must be Blob or Tree, got {}",
                other.type_name()
            )),
        }
    })
}

async fn store_blob_bytes(cas: &StoreClient, bytes: &[u8]) -> Result<BlobRef, String> {
    let prepared = prepare_blob(bytes);
    store_prepared_blob(cas, &prepared, bytes).await
}

async fn store_directory_node(
    cas: &StoreClient,
    entries: Vec<DirEntry>,
    total_size: u64,
) -> Result<(NodeHash, u64), String> {
    let directory = CasNode::Directory(DirectoryNode {
        entries,
        total_size,
    });
    let (bytes, hash) = directory.to_bytes_and_hash();
    cas.put_node(hash, bytes)
        .await
        .map_err(|error| format!("{error:?}"))?;
    Ok((hash, total_size))
}

async fn directory_total_size(cas: &StoreClient, root: NodeHash) -> Result<u64, String> {
    let bytes = cas
        .get_node(root)
        .await
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| format!("directory node {root} not found in CAS"))?;
    let node = CasNode::from_bytes(&bytes).map_err(|error| error.to_string())?;
    match node {
        CasNode::Directory(directory) => Ok(directory.total_size),
        other => Err(format!("expected DirectoryNode at {root}, got {:?}", other)),
    }
}

fn validate_tree_entry_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(format!("invalid tree entry name '{name}'"));
    }
    Ok(())
}

fn blob_ref_size(blob_ref: &BlobRef) -> u64 {
    match blob_ref {
        BlobRef::Small { size, .. } | BlobRef::Large { size, .. } => *size,
    }
}

fn dir_entry_kind_from_blob_ref(blob_ref: &BlobRef, executable: bool) -> DirEntryKind {
    match blob_ref {
        BlobRef::Small { chunk, size } => DirEntryKind::SmallFile {
            content_hash: ContentHash(chunk.0),
            size: *size,
            executable,
        },
        BlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => DirEntryKind::LargeFile {
            content_hash: *content_hash,
            blob_node: *blob_node,
            size: *size,
            executable,
        },
    }
}

#[cfg(unix)]
fn is_executable_path(path: &str) -> Result<bool, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("failed to stat {path}: {error}"))?;
    Ok(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_path(path: &str) -> Result<bool, String> {
    let _ = path;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use vx_eval::{
        BlobValue, Engine, ExecutorSelector, HostObservation, PathValue, TransportTree, TreeValue,
        Value, lower_source_file_with_source_id,
    };
    use vx_io::spawn::{ServiceBuilder, ServiceHandle};
    use vx_parser::SourceFile;
    use vx_runner_proto::{DiscoverRequest, RunEvalPayload};
    use vx_store_proto::{CasNode, ChunkHash, DirectoryNode, StoreClient};

    use super::{
        commit_value_to_transport, evaluate_shipped_discover, evaluate_shipped_observer,
        evaluate_shipped_verification,
    };

    struct StoreTestEnv {
        _root: TempDir,
        _vx_home: TempDir,
        _service: ServiceHandle,
        client: StoreClient,
    }

    impl StoreTestEnv {
        async fn new() -> Self {
            let root = TempDir::new().expect("failed to create store root tempdir");
            let vx_home = TempDir::new().expect("failed to create vx_home tempdir");
            let endpoint = free_local_endpoint();
            let binary_path = resolve_vx_store_binary();
            let root_str = root.path().to_string_lossy().into_owned();

            let service = ServiceBuilder::new(binary_path, endpoint)
                .caller_name("vx-runner-boundary-tests")
                .args(&["--root", &root_str])
                .env("VX_TEST", "1")
                .timeout(Duration::from_secs(10))
                .connect()
                .await
                .expect("failed to spawn/connect vx-store");

            let virtual_handle = service
                .boot_and_connect()
                .await
                .expect("failed to boot/connect vx-store virtual connection");

            Self {
                _root: root,
                _vx_home: vx_home,
                _service: service,
                client: StoreClient::new(virtual_handle),
            }
        }
    }

    #[tokio::test]
    async fn inline_trees_commit_to_store_backed_transport() {
        let env = StoreTestEnv::new().await;
        let value = Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("hello.txt"),
            Value::Blob(BlobValue::from_string("hello")),
        )])));

        let transport = commit_value_to_transport(&env.client, value)
            .await
            .expect("tree should commit");

        let root = match transport {
            vx_eval::TransportValue::Tree(TransportTree::Store(root)) => root,
            other => panic!("expected store-backed tree, got {other:?}"),
        };

        let node_bytes = env
            .client
            .get_node(root)
            .await
            .expect("store RPC should succeed")
            .expect("directory node should exist");
        let node = CasNode::from_bytes(&node_bytes).expect("directory should decode");
        let CasNode::Directory(DirectoryNode {
            entries,
            total_size,
        }) = node
        else {
            panic!("expected directory node");
        };

        assert_eq!(total_size, 5);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.txt");

        let chunk = match &entries[0].kind {
            vx_store_proto::DirEntryKind::SmallFile {
                content_hash, size, ..
            } => {
                assert_eq!(*size, 5);
                ChunkHash(content_hash.0)
            }
            other => panic!("expected small file entry, got {other:?}"),
        };

        let bytes = env
            .client
            .get_chunk(chunk)
            .await
            .expect("store RPC should succeed")
            .expect("chunk should exist");
        assert_eq!(bytes, b"hello");
    }

    #[tokio::test]
    async fn bare_paths_do_not_cross_the_executor_boundary() {
        let env = StoreTestEnv::new().await;
        let error = commit_value_to_transport(&env.client, Value::Path(PathValue::new("src")))
            .await
            .expect_err("path should be rejected");

        assert!(error.contains("Path values cannot cross"));
    }

    #[tokio::test]
    // r[verify exec.observer.simple]
    // r[verify exec.observer.materialization]
    async fn shipped_observer_results_commit_to_store_backed_transport() {
        let env = StoreTestEnv::new().await;
        let sandbox = TempDir::new().expect("failed to create sandbox tempdir");
        let out_dir = sandbox.path().join("out");
        fs::create_dir_all(&out_dir).expect("failed to create out dir");
        fs::write(out_dir.join("output.txt"), "hello from observer")
            .expect("failed to write observer output");

        let engine = shipped_function_engine(
            r#"fn observe(process: Process, tree: Tree) (
                   Tree {
                     @stdout.txt: process.stdout.collect().blob(),
                     @output.txt: tree.file(@out/output.txt),
                   }
                 )

                 fn make() ( observe )"#,
        );
        let eval = RunEvalPayload {
            module: engine.transport_module(),
            observer: Some(
                engine
                    .ship_function(engine.call("make", vec![]).expect("make should evaluate"))
                    .expect("observer should ship"),
            ),
            verification_obligations: Vec::new(),
        };

        let observed = evaluate_shipped_observer(
            &env.client,
            &eval,
            0,
            "done",
            "",
            &PathValue::new(sandbox.path().to_string_lossy()),
        )
        .await
        .expect("observer should evaluate")
        .expect("observer value should be present");

        let root = match observed {
            vx_eval::TransportValue::Tree(TransportTree::Store(root)) => root,
            other => panic!("expected store-backed tree, got {other:?}"),
        };

        let node_bytes = env
            .client
            .get_node(root)
            .await
            .expect("store RPC should succeed")
            .expect("directory node should exist");
        let node = CasNode::from_bytes(&node_bytes).expect("directory should decode");
        let CasNode::Directory(DirectoryNode {
            entries,
            total_size,
        }) = node
        else {
            panic!("expected directory node");
        };

        assert_eq!(total_size, 23);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "output.txt");
        assert_eq!(entries[1].name, "stdout.txt");
    }

    #[tokio::test]
    async fn shipped_observer_preserves_executable_file_entries() {
        let env = StoreTestEnv::new().await;
        let sandbox = TempDir::new().expect("failed to create sandbox tempdir");
        let out_dir = sandbox.path().join("out");
        fs::create_dir_all(&out_dir).expect("failed to create out dir");
        let exe_path = out_dir.join("lua");
        fs::write(&exe_path, "fake exe").expect("failed to write executable output");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&exe_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&exe_path, perms).expect("chmod");
        }

        let engine = shipped_function_engine(
            r#"fn observe(_process: Process, tree: Tree) (
                   Tree {
                     @lua: tree.file(@out/lua),
                   }
                 )

                 fn make() ( observe )"#,
        );
        let eval = RunEvalPayload {
            module: engine.transport_module(),
            observer: Some(
                engine
                    .ship_function(engine.call("make", vec![]).expect("make should evaluate"))
                    .expect("observer should ship"),
            ),
            verification_obligations: Vec::new(),
        };

        let observed = evaluate_shipped_observer(
            &env.client,
            &eval,
            0,
            "",
            "",
            &PathValue::new(sandbox.path().to_string_lossy()),
        )
        .await
        .expect("observer should evaluate")
        .expect("observer value should be present");

        let root = match observed {
            vx_eval::TransportValue::Tree(TransportTree::Store(root)) => root,
            other => panic!("expected store-backed tree, got {other:?}"),
        };

        let node_bytes = env
            .client
            .get_node(root)
            .await
            .expect("store RPC should succeed")
            .expect("directory node should exist");
        let node = CasNode::from_bytes(&node_bytes).expect("directory should decode");
        let CasNode::Directory(DirectoryNode { entries, .. }) = node else {
            panic!("expected directory node");
        };

        let executable = match &entries[0].kind {
            vx_store_proto::DirEntryKind::SmallFile { executable, .. }
            | vx_store_proto::DirEntryKind::LargeFile { executable, .. } => *executable,
            other => panic!("expected file entry, got {other:?}"),
        };
        assert!(executable, "expected executable bit on committed file");
    }

    #[tokio::test]
    async fn shipped_inline_closure_observer_can_return_bool() {
        let env = StoreTestEnv::new().await;
        let engine = shipped_function_engine(
            r#"fn make() {
                   (process, _tree) => process.exit_code == 0
                 }"#,
        );
        let eval = RunEvalPayload {
            module: engine.transport_module(),
            observer: Some(
                engine
                    .ship_function(engine.call("make", vec![]).expect("make should evaluate"))
                    .expect("observer should ship"),
            ),
            verification_obligations: Vec::new(),
        };

        let observed =
            evaluate_shipped_observer(&env.client, &eval, 0, "", "", &PathValue::new("."))
                .await
                .expect("observer should evaluate")
                .expect("observer value should be present");

        assert_eq!(observed, vx_eval::TransportValue::Bool(true));
    }

    #[tokio::test]
    async fn shipped_nested_inline_closure_observer_can_return_bool() {
        let env = StoreTestEnv::new().await;
        let engine = shipped_function_engine(
            r#"fn make(flag: Bool) {
                   let suffix = if flag ("") else ("!");
                   fn probe_suffix(_value: String) { suffix }
                   (process, _tree) => {
                     let _ = probe_suffix(process.stdout.utf8_lossy());
                     process.exit_code == 0
                   }
                 }"#,
        );
        let eval = RunEvalPayload {
            module: engine.transport_module(),
            observer: Some(
                engine
                    .ship_function(
                        engine
                            .call("make", vec![Value::Bool(true)])
                            .expect("make should evaluate"),
                    )
                    .expect("observer should ship"),
            ),
            verification_obligations: Vec::new(),
        };

        let observed =
            evaluate_shipped_observer(&env.client, &eval, 0, "", "", &PathValue::new("."))
                .await
                .expect("observer should evaluate")
                .expect("observer value should be present");

        assert_eq!(observed, vx_eval::TransportValue::Bool(true));
    }

    #[test]
    fn shipped_verification_reports_changed_host_files() {
        let tempdir = TempDir::new().expect("failed to create host tempdir");
        let tool_path = tempdir.path().join("tool.txt");
        fs::write(&tool_path, "toolchain-v1").expect("failed to write host file");
        let expected_hash = blake3::hash(b"toolchain-v1").to_hex().to_string();
        let eval = RunEvalPayload {
            module: shipped_function_engine(r#"fn make() {}"#).transport_module(),
            observer: None,
            verification_obligations: vec![HostObservation::Read {
                path: tool_path.to_string_lossy().to_string(),
                content_hash: expected_hash,
            }],
        };

        fs::write(&tool_path, "toolchain-v2").expect("failed to rewrite host file");

        let error =
            evaluate_shipped_verification(&eval).expect_err("verification should report drift");
        assert!(error.contains("host.read"));
    }

    #[tokio::test]
    async fn shipped_discover_probes_receive_host_access() {
        let env = StoreTestEnv::new().await;
        let tempdir = TempDir::new().expect("failed to create host tempdir");
        let tool_path = tempdir.path().join("tool.txt");
        fs::write(&tool_path, "toolchain-v1").expect("failed to write host file");
        let expected_hash = blake3::hash(b"toolchain-v1").to_hex().to_string();
        let source = format!(
            r#"fn probe(host: Host) {{
                   host.read({path:?});
                   host.hash({path:?});
                   host.read({path:?})
                 }}

                 fn make() {{ probe }}"#,
            path = tool_path.to_string_lossy(),
        );
        let engine = shipped_function_engine(&source);
        let request = DiscoverRequest {
            executor: ExecutorSelector::class("default"),
            module: engine.transport_module(),
            probe: engine
                .ship_function(engine.call("make", vec![]).expect("make should evaluate"))
                .expect("probe should ship"),
        };

        let transport = evaluate_shipped_discover(&env.client, &request)
            .await
            .expect("discover should evaluate");

        match transport {
            vx_eval::TransportValue::Discovered { value, obligations } => {
                assert_eq!(
                    obligations,
                    vec![HostObservation::Read {
                        path: tool_path.to_string_lossy().to_string(),
                        content_hash: expected_hash,
                    }]
                );
                match *value {
                    vx_eval::TransportValue::Blob(vx_eval::TransportBlob::Store { .. }) => {}
                    other => panic!("expected committed blob, got {other:?}"),
                }
            }
            other => panic!("expected discovered transport value, got {other:?}"),
        }
    }

    fn shipped_function_engine(source: &str) -> Engine {
        let parse = vx_parser::parse(source);
        assert_eq!(parse.errors(), []);
        let root = SourceFile::cast_root(parse.syntax());
        let module = lower_source_file_with_source_id(&root, "<test>");
        Engine::from_module(&module)
    }

    fn free_local_endpoint() -> vx_io::net::Endpoint {
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
        let addr = listener
            .local_addr()
            .expect("failed to read ephemeral port");
        drop(listener);
        vx_io::net::Endpoint::parse(&addr.to_string()).expect("failed to parse test endpoint")
    }

    fn resolve_vx_store_binary() -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/vx-store")
            .to_string_lossy()
            .into_owned()
    }
}
