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
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use tempfile::TempDir;
use vx_eval::{
    BlobValue, Engine, MapValue, PathValue, StructValue, TransportValue, TreeValue, Value,
};
use vx_runner_proto::RunEvalPayload;
use vx_store_proto::chunking::{prepare_blob, store_prepared_blob};
use vx_store_proto::{
    BlobRef, CasNode, ContentHash, DirEntry, DirEntryKind, DirectoryNode, NodeHash, StoreClient,
};

type BoundaryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub fn commit_value_to_transport<'a>(
    cas: &'a StoreClient,
    value: Value,
) -> BoundaryFuture<'a, TransportValue> {
    Box::pin(async move {
        let value = commit_value(cas, value).await?;
        TransportValue::try_from(&value)
    })
}

pub fn evaluate_shipped_observer<'a>(
    cas: &'a StoreClient,
    eval: &'a RunEvalPayload,
    exit_code: i32,
    stdout: &'a str,
    stderr: &'a str,
    sandbox_root: &'a PathValue,
) -> BoundaryFuture<'a, Option<TransportValue>> {
    Box::pin(async move {
        if eval.verifier.is_some() {
            return Err("run verifiers are not implemented yet".into());
        }

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

pub async fn materialize_tree_snapshot(
    cas: &StoreClient,
    root: NodeHash,
) -> Result<TempDir, String> {
    let snapshot = tempfile::tempdir()
        .map_err(|error| format!("failed to create tempdir for observer snapshot: {error}"))?;
    materialize_store_tree(cas, root, snapshot.path()).await?;
    Ok(snapshot)
}

fn commit_value<'a>(cas: &'a StoreClient, value: Value) -> BoundaryFuture<'a, Value> {
    Box::pin(async move {
        match value {
            Value::Pending(_) => Err("pending values cannot cross the executor boundary".into()),
            Value::Path(path) => Err(format!(
                "Path values cannot cross the executor boundary: {}",
                path.as_str()
            )),
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

async fn materialize_store_tree(
    cas: &StoreClient,
    root: NodeHash,
    destination: &Path,
) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "failed to create observer snapshot dir {}: {error}",
            destination.display()
        )
    })?;

    let mut pending = vec![(root, destination.to_path_buf())];
    while let Some((node_hash, dir_path)) = pending.pop() {
        fs::create_dir_all(&dir_path).map_err(|error| {
            format!(
                "failed to create observer snapshot dir {}: {error}",
                dir_path.display()
            )
        })?;

        let directory = load_directory(cas, node_hash).await?;
        for entry in directory.entries {
            let entry_path = dir_path.join(&entry.name);
            match entry.kind {
                DirEntryKind::SmallFile {
                    content_hash,
                    size,
                    executable,
                } => {
                    let bytes = fetch_blob_bytes(
                        cas,
                        &BlobRef::Small {
                            chunk: content_hash.0,
                            size,
                        },
                    )
                    .await?;
                    write_materialized_blob(&entry_path, &bytes, executable)?;
                }
                DirEntryKind::LargeFile {
                    blob_node,
                    content_hash,
                    size,
                    executable,
                } => {
                    let bytes = fetch_blob_bytes(
                        cas,
                        &BlobRef::Large {
                            blob_node,
                            content_hash,
                            size,
                        },
                    )
                    .await?;
                    write_materialized_blob(&entry_path, &bytes, executable)?;
                }
                DirEntryKind::Dir { hash, .. } => pending.push((hash, entry_path)),
                DirEntryKind::Symlink { target } => {
                    return Err(format!(
                        "observer snapshots do not support store-backed symlinks yet (target: {target})"
                    ));
                }
            }
        }
    }

    Ok(())
}

fn process_value(exit_code: i32, stdout: &str, stderr: &str) -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("Process"),
        fields: Arc::from([
            (Arc::from("exit_code"), Value::U64(exit_code.max(0) as u64)),
            (
                Arc::from("stdout"),
                Value::Blob(BlobValue::Inline(Arc::from(stdout.as_bytes()))),
            ),
            (
                Arc::from("stderr"),
                Value::Blob(BlobValue::Inline(Arc::from(stderr.as_bytes()))),
            ),
        ]),
    })
}

async fn fetch_blob_bytes(cas: &StoreClient, blob_ref: &BlobRef) -> Result<Vec<u8>, String> {
    match blob_ref {
        BlobRef::Small { chunk, .. } => cas
            .get_chunk(*chunk)
            .await
            .map_err(|error| format!("failed to fetch chunk {}: {error:?}", chunk.to_hex()))?
            .ok_or_else(|| format!("chunk {} not found", chunk.to_hex())),
        BlobRef::Large {
            blob_node,
            content_hash: _,
            size: _,
        } => {
            let node_bytes = cas
                .get_node(*blob_node)
                .await
                .map_err(|error| format!("failed to fetch blob node {blob_node}: {error:?}"))?
                .ok_or_else(|| format!("blob node {blob_node} not found"))?;
            let blob_node = match CasNode::from_bytes(&node_bytes) {
                Ok(CasNode::Blob(blob)) => blob,
                Ok(_) => return Err(format!("node {blob_node} is not a Blob node")),
                Err(error) => {
                    return Err(format!(
                        "failed to deserialize blob node {blob_node}: {error}"
                    ));
                }
            };

            let mut result = Vec::with_capacity(blob_node.total_size as usize);
            for chunk in &blob_node.chunks {
                let chunk_bytes = cas
                    .get_chunk(chunk.hash)
                    .await
                    .map_err(|error| {
                        format!("failed to fetch chunk {}: {error:?}", chunk.hash.to_hex())
                    })?
                    .ok_or_else(|| format!("chunk {} not found", chunk.hash.to_hex()))?;
                result.extend_from_slice(&chunk_bytes);
            }
            Ok(result)
        }
    }
}

async fn load_directory(cas: &StoreClient, root: NodeHash) -> Result<DirectoryNode, String> {
    let node_bytes = cas
        .get_node(root)
        .await
        .map_err(|error| format!("failed to fetch directory node {root}: {error:?}"))?
        .ok_or_else(|| format!("directory node {root} not found"))?;
    match CasNode::from_bytes(&node_bytes) {
        Ok(CasNode::Directory(directory)) => Ok(directory),
        Ok(_) => Err(format!("node {root} is not a Directory node")),
        Err(error) => Err(format!(
            "failed to deserialize directory node {root}: {error}"
        )),
    }
}

fn write_materialized_blob(path: &Path, bytes: &[u8], executable: bool) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| {
        format!(
            "observer snapshot path {} has no parent directory",
            path.display()
        )
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    fs::write(path, bytes)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    set_executable_if_needed(path, executable)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable_if_needed(path: &Path, executable: bool) -> Result<(), String> {
    if !executable {
        return Ok(());
    }

    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("failed to chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_executable_if_needed(_path: &Path, _executable: bool) -> Result<(), String> {
    Ok(())
}

fn sandbox_tree_value(sandbox_root: &PathValue) -> Value {
    Value::Tree(TreeValue::FsDir(sandbox_root.clone()))
}

fn commit_blob<'a>(cas: &'a StoreClient, blob: BlobValue) -> BoundaryFuture<'a, BlobValue> {
    Box::pin(async move {
        let blob_ref = match blob {
            BlobValue::Inline(bytes) => store_blob_bytes(cas, bytes.as_ref()).await?,
            BlobValue::FsFile(path) => {
                let bytes = fs::read(path.as_str())
                    .map_err(|error| format!("failed to read {}: {error}", path.as_str()))?;
                store_blob_bytes(cas, &bytes).await?
            }
            BlobValue::Store(blob_ref) => blob_ref,
        };
        Ok(BlobValue::Store(blob_ref))
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
                    BlobValue::Inline(bytes) => {
                        (store_blob_bytes(cas, bytes.as_ref()).await?, false)
                    }
                    BlobValue::FsFile(path) => {
                        let bytes = fs::read(path.as_str()).map_err(|error| {
                            format!("failed to read {}: {error}", path.as_str())
                        })?;
                        (
                            store_blob_bytes(cas, &bytes).await?,
                            is_executable_path(path.as_str())?,
                        )
                    }
                    BlobValue::Store(blob_ref) => (blob_ref, false),
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
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use vx_eval::{
        BlobValue, Engine, PathValue, TransportTree, TreeValue, Value, lower_source_file,
    };
    use vx_io::spawn::{ServiceBuilder, ServiceHandle};
    use vx_parser::SourceFile;
    use vx_runner_proto::RunEvalPayload;
    use vx_store_proto::{CasNode, ChunkHash, DirectoryNode, StoreClient};

    use super::{commit_value_to_transport, evaluate_shipped_observer};

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
    async fn shipped_observer_results_commit_to_store_backed_transport() {
        let env = StoreTestEnv::new().await;
        let sandbox = TempDir::new().expect("failed to create sandbox tempdir");
        let out_dir = sandbox.path().join("out");
        fs::create_dir_all(&out_dir).expect("failed to create out dir");
        fs::write(out_dir.join("output.txt"), "hello from observer")
            .expect("failed to write observer output");

        let engine = shipped_function_engine(
            r#"fn observe(process: Process, tree: Tree) {
                   Tree.of(%{
                     "stdout.txt" => process.stdout,
                     "output.txt" => tree.file("out/output.txt"),
                   })
                 }

                 fn make() { observe }"#,
        );
        let eval = RunEvalPayload {
            module: engine.transport_module(),
            observer: Some(
                engine
                    .ship_function(engine.call("make", vec![]).expect("make should evaluate"))
                    .expect("observer should ship"),
            ),
            verifier: None,
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

    fn shipped_function_engine(source: &str) -> Engine {
        let parse = vx_parser::parse(source);
        assert_eq!(parse.errors(), []);
        let root = SourceFile::cast_root(parse.syntax());
        let module = lower_source_file(&root);
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
