//! Executor-boundary conversion for shipped evaluator results.
//!
//! Values produced on the runner can still point at executor-local filesystem
//! state. Before they cross back over RPC, blobs and trees must be committed
//! into CAS-backed runtime values.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::future::Future;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::future::try_join_all;
use vx_eval::{
    BlobValue, DemandChain, DiscoveredValue, Engine, MapValue, PathValue, PoisonError, PoisonKind,
    StructValue, TransportPoisonError, TransportValue, TreeValue, Value, verify_host_obligations,
};
use vx_runner_proto::{DiscoverRequest, RunEvalPayload};
use vx_store_proto::chunking::{
    PreparedBlob, list_missing_chunks, list_missing_nodes, prepare_blob, store_prepared_blob,
};
use vx_store_proto::{
    BlobRef, CasNode, ContentHash, DirEntry, DirEntryKind, DirectoryNode, NodeHash,
    RemoteTracePhase, RemoteTraceSpan, StoreClient, StoreProgress,
};

use crate::sandbox::send_progress_reliably;

type BoundaryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TransportPoisonError>> + Send + 'a>>;

static NEXT_REMOTE_TRACE_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct PreparedBlobUpload {
    prepared: PreparedBlob,
    data: Vec<u8>,
    label: String,
}

#[derive(Debug, Default)]
struct TreeCommitPlan {
    blobs: Vec<PreparedBlobUpload>,
    nodes: Vec<(NodeHash, Vec<u8>, String)>,
}

impl TreeCommitPlan {
    fn extend(&mut self, other: Self) {
        self.blobs.extend(other.blobs);
        self.nodes.extend(other.nodes);
    }
}

async fn emit_remote_trace(
    progress: Option<&roam::Tx<StoreProgress>>,
    span: RemoteTraceSpan,
    phase: RemoteTracePhase,
    instance_id: u64,
    label: Option<&str>,
) {
    if let Some(progress) = progress {
        let trace = match label {
            Some(label) => StoreProgress::trace_labeled_with_id(span, phase, instance_id, label),
            None => StoreProgress::trace_with_id(span, phase, instance_id),
        };
        let _ = send_progress_reliably(progress, trace).await;
    }
}

async fn with_remote_trace<T>(
    progress: Option<&roam::Tx<StoreProgress>>,
    span: RemoteTraceSpan,
    future: impl Future<Output = Result<T, TransportPoisonError>>,
) -> Result<T, TransportPoisonError> {
    let instance_id = NEXT_REMOTE_TRACE_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    emit_remote_trace(progress, span, RemoteTracePhase::Begin, instance_id, None).await;
    let result = future.await;
    emit_remote_trace(progress, span, RemoteTracePhase::End, instance_id, None).await;
    result
}

async fn with_remote_trace_labeled<T>(
    progress: Option<&roam::Tx<StoreProgress>>,
    span: RemoteTraceSpan,
    label: &str,
    future: impl Future<Output = Result<T, TransportPoisonError>>,
) -> Result<T, TransportPoisonError> {
    let instance_id = NEXT_REMOTE_TRACE_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    emit_remote_trace(
        progress,
        span,
        RemoteTracePhase::Begin,
        instance_id,
        Some(label),
    )
    .await;
    let result = future.await;
    emit_remote_trace(
        progress,
        span,
        RemoteTracePhase::End,
        instance_id,
        Some(label),
    )
    .await;
    result
}

// r[impl exec.observer.materialization]
pub fn commit_value_to_transport<'a>(
    cas: &'a StoreClient,
    value: Value,
    progress: Option<&'a roam::Tx<StoreProgress>>,
) -> BoundaryFuture<'a, TransportValue> {
    Box::pin(async move {
        let value = commit_value(cas, value, progress).await?;
        TransportValue::try_from(&value).map_err(boundary_user_poison)
    })
}

// r[impl exec.observer.simple]
pub fn invoke_shipped_observer<'a>(
    eval: &'a RunEvalPayload,
    exit_code: i32,
    stdout: &'a str,
    stderr: &'a str,
    sandbox_root: &'a PathValue,
) -> BoundaryFuture<'a, Option<Value>> {
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
        .await
        .map_err(|error| TransportPoisonError::from(&error))?;

        Ok(Some(observed))
    })
}

// r[impl exec.observer.simple]
#[cfg(test)]
pub fn evaluate_shipped_observer<'a>(
    cas: &'a StoreClient,
    eval: &'a RunEvalPayload,
    exit_code: i32,
    stdout: &'a str,
    stderr: &'a str,
    sandbox_root: &'a PathValue,
) -> BoundaryFuture<'a, Option<TransportValue>> {
    Box::pin(async move {
        let Some(observed) =
            invoke_shipped_observer(eval, exit_code, stdout, stderr, sandbox_root).await?
        else {
            return Ok(None);
        };

        commit_value_to_transport(cas, observed, None)
            .await
            .map(Some)
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
            .await
            .map_err(|error| TransportPoisonError::from(&error))?;
        commit_value_to_transport(cas, discovered, None).await
    })
}

fn boundary_user_poison(message: impl Into<String>) -> TransportPoisonError {
    TransportPoisonError::from(&PoisonError::new(
        PoisonKind::UserFail,
        message,
        DemandChain::default(),
    ))
}

fn commit_value<'a>(
    cas: &'a StoreClient,
    value: Value,
    progress: Option<&'a roam::Tx<StoreProgress>>,
) -> BoundaryFuture<'a, Value> {
    Box::pin(async move {
        match value {
            Value::Pending(_) => Err(boundary_user_poison(
                "pending values cannot cross the executor boundary",
            )),
            Value::Path(path) => Err(boundary_user_poison(format!(
                "Path values cannot cross the executor boundary: {}",
                path.as_str()
            ))),
            Value::Host(_) => Err(boundary_user_poison(
                "Host values cannot cross the executor boundary",
            )),
            Value::Discovered(discovered) => {
                let value = commit_value(cas, *discovered.value, progress).await?;
                Ok(Value::Discovered(DiscoveredValue {
                    value: Box::new(value),
                    obligations: discovered.obligations,
                }))
            }
            Value::Blob(blob) => Ok(Value::Blob(
                with_remote_trace(
                    progress,
                    RemoteTraceSpan::RunnerObserverCommitStoreBlob,
                    commit_blob(cas, blob, progress),
                )
                .await?,
            )),
            Value::Tree(tree) => {
                let (root, _) = with_remote_trace(
                    progress,
                    RemoteTraceSpan::RunnerObserverCommitTree,
                    commit_tree(cas, tree, progress),
                )
                .await?;
                Ok(Value::Tree(TreeValue::Store(root)))
            }
            Value::Tuple(values) => {
                let mut committed = Vec::with_capacity(values.len());
                for value in values.iter().cloned() {
                    committed.push(commit_value(cas, value, progress).await?);
                }
                Ok(Value::Tuple(Arc::from(committed)))
            }
            Value::Array(values) => {
                let mut committed = Vec::with_capacity(values.len());
                for value in values.iter().cloned() {
                    committed.push(commit_value(cas, value, progress).await?);
                }
                Ok(Value::Array(Arc::from(committed)))
            }
            Value::Map(map) => {
                let mut committed = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    committed.push((
                        commit_value(cas, key.clone(), progress).await?,
                        commit_value(cas, value.clone(), progress).await?,
                    ));
                }
                Ok(Value::Map(MapValue {
                    entries: Arc::from(committed),
                }))
            }
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields.iter() {
                    fields.push((
                        name.clone(),
                        commit_value(cas, value.clone(), progress).await?,
                    ));
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

fn commit_blob<'a>(
    cas: &'a StoreClient,
    blob: BlobValue,
    progress: Option<&'a roam::Tx<StoreProgress>>,
) -> BoundaryFuture<'a, BlobValue> {
    Box::pin(async move {
        let (blob_ref, executable) = match blob {
            BlobValue::Inline { bytes, executable } => (
                store_blob_bytes(cas, bytes.as_ref(), progress).await?,
                executable,
            ),
            BlobValue::FsFile { path, executable } => {
                let bytes = fs::read(path.as_str()).map_err(|error| {
                    boundary_user_poison(format!("failed to read {}: {error}", path.as_str()))
                })?;
                (store_blob_bytes(cas, &bytes, progress).await?, executable)
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

fn commit_tree<'a>(
    cas: &'a StoreClient,
    tree: TreeValue,
    progress: Option<&'a roam::Tx<StoreProgress>>,
) -> BoundaryFuture<'a, (NodeHash, u64)> {
    Box::pin(async move {
        match tree {
            TreeValue::Inline { entries, .. } => commit_inline_tree(cas, entries, progress).await,
            TreeValue::FsDir(path) => commit_fs_tree(cas, path, progress).await,
            TreeValue::Store(root) => Ok((root, directory_total_size(cas, root).await?)),
        }
    })
}

fn commit_inline_tree<'a>(
    cas: &'a StoreClient,
    entries: Arc<[(Arc<str>, Value)]>,
    progress: Option<&'a roam::Tx<StoreProgress>>,
) -> BoundaryFuture<'a, (NodeHash, u64)> {
    Box::pin(async move {
        let (hash, total_size, plan) = prepare_inline_tree(cas, entries, String::new()).await?;
        upload_tree_commit_plan(cas, plan, progress).await?;
        Ok((hash, total_size))
    })
}

fn prepare_tree<'a>(
    cas: &'a StoreClient,
    tree: TreeValue,
    display_path: String,
) -> BoundaryFuture<'a, (NodeHash, u64, TreeCommitPlan)> {
    Box::pin(async move {
        match tree {
            TreeValue::Inline { entries, .. } => {
                prepare_inline_tree(cas, entries, display_path).await
            }
            TreeValue::FsDir(path) => prepare_fs_tree(cas, path, display_path).await,
            TreeValue::Store(root) => {
                let total_size = directory_total_size(cas, root).await?;
                Ok((root, total_size, TreeCommitPlan::default()))
            }
        }
    })
}

fn prepare_inline_tree<'a>(
    cas: &'a StoreClient,
    entries: Arc<[(Arc<str>, Value)]>,
    display_path: String,
) -> BoundaryFuture<'a, (NodeHash, u64, TreeCommitPlan)> {
    Box::pin(async move {
        let mut names = BTreeSet::new();
        let mut sorted: Vec<(Arc<str>, Value)> = entries.iter().cloned().collect();
        sorted.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));

        let mut dir_entries = Vec::with_capacity(sorted.len());
        let mut total_size = 0u64;
        let mut plan = TreeCommitPlan::default();
        for (name, value) in sorted {
            validate_tree_entry_name(name.as_ref()).map_err(boundary_user_poison)?;
            if !names.insert(name.to_string()) {
                return Err(boundary_user_poison(format!(
                    "duplicate tree entry '{name}'"
                )));
            }
            let entry_path = join_display_path(&display_path, &name);
            let (kind, size, entry_plan) = prepare_tree_entry(cas, value, entry_path).await?;
            total_size += size;
            plan.extend(entry_plan);
            dir_entries.push(DirEntry {
                name: name.to_string(),
                kind,
            });
        }
        let (hash, bytes) = serialize_directory_node(dir_entries, total_size);
        plan.nodes
            .push((hash, bytes, display_path_label(&display_path)));
        Ok((hash, total_size, plan))
    })
}

fn commit_fs_tree<'a>(
    cas: &'a StoreClient,
    root: PathValue,
    progress: Option<&'a roam::Tx<StoreProgress>>,
) -> BoundaryFuture<'a, (NodeHash, u64)> {
    Box::pin(async move {
        let (hash, total_size, plan) = prepare_fs_tree(cas, root, String::new()).await?;
        upload_tree_commit_plan(cas, plan, progress).await?;
        Ok((hash, total_size))
    })
}

fn prepare_fs_tree<'a>(
    cas: &'a StoreClient,
    root: PathValue,
    display_path: String,
) -> BoundaryFuture<'a, (NodeHash, u64, TreeCommitPlan)> {
    Box::pin(async move {
        let mut read_dir = fs::read_dir(root.as_str()).map_err(|error| {
            boundary_user_poison(format!("failed to read {}: {error}", root.as_str()))
        })?;
        let mut names = Vec::new();
        while let Some(entry) = read_dir.next().transpose().map_err(|error| {
            boundary_user_poison(format!("failed to read {}: {error}", root.as_str()))
        })? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("._") {
                continue;
            }
            names.push(name);
        }
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

        let mut dir_entries = Vec::with_capacity(names.len());
        let mut total_size = 0u64;
        let mut plan = TreeCommitPlan::default();
        for name in names {
            validate_tree_entry_name(&name).map_err(boundary_user_poison)?;
            let child = root.join(&name);
            let metadata = fs::metadata(child.as_str()).map_err(|error| {
                boundary_user_poison(format!("failed to stat {}: {error}", child.as_str()))
            })?;
            let child_display_path = join_display_path(&display_path, &name);
            let (kind, size, child_plan) = if metadata.is_dir() {
                let (hash, child_total_size, child_plan) =
                    prepare_fs_tree(cas, child, child_display_path).await?;
                (DirEntryKind::Dir { hash }, child_total_size, child_plan)
            } else {
                let bytes = fs::read(child.as_str()).map_err(|error| {
                    boundary_user_poison(format!("failed to read {}: {error}", child.as_str()))
                })?;
                let executable =
                    is_executable_path(child.as_str()).map_err(boundary_user_poison)?;
                prepare_blob_entry(bytes, executable, child_display_path)
            };
            total_size += size;
            plan.extend(child_plan);
            dir_entries.push(DirEntry { name, kind });
        }
        let (hash, bytes) = serialize_directory_node(dir_entries, total_size);
        plan.nodes
            .push((hash, bytes, display_path_label(&display_path)));
        Ok((hash, total_size, plan))
    })
}

fn prepare_tree_entry<'a>(
    cas: &'a StoreClient,
    value: Value,
    display_path: String,
) -> BoundaryFuture<'a, (DirEntryKind, u64, TreeCommitPlan)> {
    Box::pin(async move {
        match value {
            Value::Blob(blob) => prepare_blob_value(blob, display_path),
            Value::Tree(tree) => {
                let (hash, total_size, plan) = prepare_tree(cas, tree, display_path).await?;
                Ok((DirEntryKind::Dir { hash }, total_size, plan))
            }
            other => Err(boundary_user_poison(format!(
                "tree entries must be Blob or Tree, got {}",
                other.type_name()
            ))),
        }
    })
}

fn prepare_blob_value(
    blob: BlobValue,
    display_path: String,
) -> Result<(DirEntryKind, u64, TreeCommitPlan), TransportPoisonError> {
    match blob {
        BlobValue::Inline { bytes, executable } => {
            let (blob_ref, blob_plan) =
                prepare_blob_upload(bytes.as_ref().to_vec(), display_path.clone());
            let size = blob_ref_size(&blob_ref);
            Ok((
                dir_entry_kind_from_blob_ref(&blob_ref, executable),
                size,
                TreeCommitPlan {
                    blobs: vec![blob_plan],
                    nodes: Vec::new(),
                },
            ))
        }
        BlobValue::FsFile { path, executable } => {
            let bytes = fs::read(path.as_str()).map_err(|error| {
                boundary_user_poison(format!("failed to read {}: {error}", path.as_str()))
            })?;
            let (blob_ref, blob_plan) = prepare_blob_upload(bytes, display_path.clone());
            let size = blob_ref_size(&blob_ref);
            Ok((
                dir_entry_kind_from_blob_ref(&blob_ref, executable),
                size,
                TreeCommitPlan {
                    blobs: vec![blob_plan],
                    nodes: Vec::new(),
                },
            ))
        }
        BlobValue::Store {
            blob_ref,
            executable,
        } => Ok((
            dir_entry_kind_from_blob_ref(&blob_ref, executable),
            blob_ref_size(&blob_ref),
            TreeCommitPlan::default(),
        )),
    }
}

fn prepare_blob_entry(
    bytes: Vec<u8>,
    executable: bool,
    display_path: String,
) -> (DirEntryKind, u64, TreeCommitPlan) {
    let (blob_ref, blob_plan) = prepare_blob_upload(bytes, display_path);
    let size = blob_ref_size(&blob_ref);
    (
        dir_entry_kind_from_blob_ref(&blob_ref, executable),
        size,
        TreeCommitPlan {
            blobs: vec![blob_plan],
            nodes: Vec::new(),
        },
    )
}

fn prepare_blob_upload(data: Vec<u8>, label: String) -> (BlobRef, PreparedBlobUpload) {
    let prepared = prepare_blob(&data);
    let blob_ref = prepared.to_blob_ref();
    (
        blob_ref,
        PreparedBlobUpload {
            prepared,
            data,
            label,
        },
    )
}

fn join_display_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn display_path_label(path: &str) -> String {
    if path.is_empty() {
        ".".to_string()
    } else {
        path.to_string()
    }
}

fn summarize_upload_label(kind: &str, labels: &[String]) -> String {
    let mut unique = BTreeSet::new();
    for label in labels {
        unique.insert(label.as_str());
    }
    let labels = unique.into_iter().collect::<Vec<_>>();
    match labels.as_slice() {
        [] => kind.to_string(),
        [only] => format!("{kind}({only})"),
        [first, second] => format!("{kind}({first}, {second})"),
        [first, second, ..] => format!("{kind}({first}, {second}, +{} more)", labels.len() - 2),
    }
}

fn serialize_directory_node(entries: Vec<DirEntry>, total_size: u64) -> (NodeHash, Vec<u8>) {
    let directory = CasNode::Directory(DirectoryNode {
        entries,
        total_size,
    });
    let (bytes, hash) = directory.to_bytes_and_hash();
    (hash, bytes)
}

async fn upload_tree_commit_plan(
    cas: &StoreClient,
    plan: TreeCommitPlan,
    progress: Option<&roam::Tx<StoreProgress>>,
) -> Result<(), TransportPoisonError> {
    let mut chunk_uploads = HashMap::new();
    let mut node_uploads = HashMap::new();
    let mut blob_labels = Vec::new();
    let mut node_labels = Vec::new();

    for blob in plan.blobs {
        blob_labels.push(display_path_label(&blob.label));
        for chunk in &blob.prepared.chunks {
            chunk_uploads.entry(chunk.hash).or_insert_with(|| {
                blob.data[chunk.offset..chunk.offset + chunk.size as usize].to_vec()
            });
        }
        if let Some(blob_node) = blob.prepared.blob_node {
            node_uploads
                .entry(blob_node.node_hash)
                .or_insert(blob_node.serialized_bytes);
        }
    }

    for (hash, bytes, label) in plan.nodes {
        node_labels.push(display_path_label(&label));
        node_uploads.entry(hash).or_insert(bytes);
    }

    let chunk_hashes: Vec<_> = chunk_uploads.keys().copied().collect();
    let node_hashes: Vec<_> = node_uploads.keys().copied().collect();

    let (missing_chunks, missing_nodes) = tokio::try_join!(
        async {
            list_missing_chunks(cas, chunk_hashes)
                .await
                .map_err(boundary_user_poison)
        },
        async {
            list_missing_nodes(cas, node_hashes)
                .await
                .map_err(boundary_user_poison)
        },
    )?;

    let missing_chunks: HashSet<_> = missing_chunks.into_iter().collect();
    let missing_nodes: HashSet<_> = missing_nodes.into_iter().collect();
    let blob_trace_label = summarize_upload_label("store_blob", &blob_labels);
    let node_trace_label = summarize_upload_label("store_node", &node_labels);

    tokio::try_join!(
        with_remote_trace_labeled(
            progress,
            RemoteTraceSpan::RunnerObserverCommitStoreBlob,
            &blob_trace_label,
            async {
                let uploads = chunk_uploads
                    .into_iter()
                    .filter(|(hash, _)| missing_chunks.contains(hash))
                    .map(|(hash, data)| async move {
                        cas.put_chunk(hash, data)
                            .await
                            .map_err(|error| boundary_user_poison(format!("{error:?}")))
                    });
                try_join_all(uploads).await?;
                Ok(())
            }
        ),
        with_remote_trace_labeled(
            progress,
            RemoteTraceSpan::RunnerObserverCommitStoreNode,
            &node_trace_label,
            async {
                let uploads = node_uploads
                    .into_iter()
                    .filter(|(hash, _)| missing_nodes.contains(hash))
                    .map(|(hash, data)| async move {
                        cas.put_node(hash, data)
                            .await
                            .map_err(|error| boundary_user_poison(format!("{error:?}")))
                    });
                try_join_all(uploads).await?;
                Ok(())
            }
        ),
    )?;

    Ok(())
}

async fn store_blob_bytes(
    cas: &StoreClient,
    bytes: &[u8],
    _progress: Option<&roam::Tx<StoreProgress>>,
) -> Result<BlobRef, TransportPoisonError> {
    let prepared = prepare_blob(bytes);
    store_prepared_blob(cas, &prepared, bytes)
        .await
        .map_err(boundary_user_poison)
}

async fn directory_total_size(
    cas: &StoreClient,
    root: NodeHash,
) -> Result<u64, TransportPoisonError> {
    let bytes = cas
        .get_node(root)
        .await
        .map_err(|error| boundary_user_poison(format!("{error:?}")))?
        .ok_or_else(|| boundary_user_poison(format!("directory node {root} not found in CAS")))?;
    let node =
        CasNode::from_bytes(&bytes).map_err(|error| boundary_user_poison(error.to_string()))?;
    match node {
        CasNode::Directory(directory) => Ok(directory.total_size),
        other => Err(boundary_user_poison(format!(
            "expected DirectoryNode at {root}, got {:?}",
            other
        ))),
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
        let value = Value::Tree(TreeValue::inline(Arc::from([(
            Arc::from("hello.txt"),
            Value::Blob(BlobValue::from_string("hello")),
        )])));

        let transport = commit_value_to_transport(&env.client, value, None)
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
        let error =
            commit_value_to_transport(&env.client, Value::Path(PathValue::new("src")), None)
                .await
                .expect_err("path should be rejected");

        assert!(error.message.contains("Path values cannot cross"));
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
                   Tree %{
                     @stdout.txt => process.stdout.collect().blob(),
                     @output.txt => tree.file(@out/output.txt),
                   }
                 )

                 fn make() ( observe )"#,
        );
        let eval = RunEvalPayload {
            module: engine.transport_module(),
            observer: Some(
                engine
                    .ship_function(
                        engine
                            .call("make", vec![])
                            .await
                            .expect("make should evaluate"),
                    )
                    .await
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
                   Tree %{
                     @lua => tree.file(@out/lua),
                   }
                 )

                 fn make() ( observe )"#,
        );
        let eval = RunEvalPayload {
            module: engine.transport_module(),
            observer: Some(
                engine
                    .ship_function(
                        engine
                            .call("make", vec![])
                            .await
                            .expect("make should evaluate"),
                    )
                    .await
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
                    .ship_function(
                        engine
                            .call("make", vec![])
                            .await
                            .expect("make should evaluate"),
                    )
                    .await
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
                            .await
                            .expect("make should evaluate"),
                    )
                    .await
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
                .ship_function(
                    engine
                        .call("make", vec![])
                        .await
                        .expect("make should evaluate"),
                )
                .await
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

    fn shipped_function_engine(source: &str) -> Arc<Engine> {
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
