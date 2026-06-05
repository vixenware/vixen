use std::sync::Arc;

use vx_runtime::remote::{Part, PartKey, Producing};
use vx_services::cas_tree::read_tree_file;
use vx_services::{ExecOutput, NodeHash, StoreClient};

#[derive(Clone)]
pub(crate) struct CasOutputTreeService {
    store: Arc<StoreClient>,
    outputs: Arc<[OutputRoot]>,
}

#[derive(Clone)]
struct OutputRoot {
    path: String,
    tree: NodeHash,
}

impl CasOutputTreeService {
    pub(crate) fn new(store: Arc<StoreClient>, outputs: Vec<ExecOutput>) -> Self {
        let outputs = outputs
            .into_iter()
            .filter_map(|output| {
                let path = match normalize_output_root(&output.path) {
                    Ok(path) => path,
                    Err(reason) => {
                        tracing::warn!(%reason, "dropping invalid exec output root");
                        return None;
                    }
                };
                Some(OutputRoot {
                    path,
                    tree: NodeHash(output.tree),
                })
            })
            .collect::<Vec<_>>();
        Self {
            store,
            outputs: Arc::from(outputs),
        }
    }

    async fn force_path(&self, requested: &str) -> Part {
        let requested = match normalize_requested_path(requested) {
            Ok(path) => path,
            Err(reason) => return Part::Absent(reason),
        };
        let Some((root, path_in_root)) = self.select_root(&requested) else {
            return Part::Absent(format!(
                "output tree has no declared root for path `{requested}`"
            ));
        };
        match read_tree_file(&self.store, root.tree, path_in_root).await {
            Ok(Some(bytes)) => Part::Bytes(bytes),
            Ok(None) => Part::Absent(format!("output tree has no file `{requested}`")),
            Err(reason) => Part::Absent(format!("failed to read `{requested}` from CAS: {reason}")),
        }
    }

    fn select_root<'a>(&'a self, requested: &'a str) -> Option<(&'a OutputRoot, &'a str)> {
        self.outputs
            .iter()
            .filter_map(|root| {
                strip_root_prefix(&root.path, requested).map(|path_in_root| (root, path_in_root))
            })
            .max_by_key(|(root, _)| root.path.len())
    }
}

impl Producing for CasOutputTreeService {
    async fn force(&self, part: PartKey) -> Part {
        match part {
            PartKey::TreePath(path) => self.force_path(&path).await,
            PartKey::StreamItem(index) => {
                Part::Absent(format!("output tree has no stream item {index}"))
            }
        }
    }
}

fn normalize_output_root(path: &str) -> Result<String, String> {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(format!("output root `{path}` contains `..`")),
            _ => components.push(component),
        }
    }
    Ok(components.join("/"))
}

fn normalize_requested_path(path: &str) -> Result<String, String> {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(format!("output tree path `{path}` contains `..`")),
            _ => components.push(component),
        }
    }
    if components.is_empty() {
        return Err("output tree path is empty".to_string());
    }
    Ok(components.join("/"))
}

fn strip_root_prefix<'a>(root: &str, requested: &'a str) -> Option<&'a str> {
    if root.is_empty() {
        return Some(requested);
    }
    if requested == root {
        return Some("");
    }
    requested.strip_prefix(root)?.strip_prefix('/')
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use vx_runtime::remote::Producing;
    use vx_services::cas_tree::{MerkleTreeFile, build_merkle_tree, read_tree_file};
    use vx_services::chunking::{prepare_blob, store_prepared_blob};
    use vx_services::{ExecOutput, StoreClient, StoreDispatcher};

    use super::CasOutputTreeService;

    async fn store_client() -> (TempDir, StoreClient) {
        let temp = TempDir::new().expect("temp dir");
        let store = vx_store::open(temp.path().join("store").to_string_lossy().parse().unwrap())
            .await
            .expect("open store");
        let (client_link, server_link) = vox::memory_link_pair(64);
        tokio::spawn(async move {
            let acceptor = vox::acceptor_fn(
                move |request: &vox::ConnectionRequest, connection: vox::PendingConnection| {
                    match request.service() {
                        "Store" => {
                            connection.handle_with(StoreDispatcher::new(store.clone()));
                            Ok(())
                        }
                        _ => Err(vec![]),
                    }
                },
            );
            if let Ok(client) = vox::acceptor_on(server_link)
                .on_connection(acceptor)
                .establish::<vox::NoopClient>()
                .await
            {
                client.caller.closed().await;
            }
        });
        let client = vox::initiator_on(client_link, vox::TransportMode::Bare)
            .establish::<StoreClient>()
            .await
            .expect("establish store client");
        (temp, client)
    }

    async fn output_tree(store: &StoreClient) -> vx_services::Blake3Hash {
        let blob = prepare_blob(b"payload-bytes");
        let blob_ref = store_prepared_blob(store, &blob, b"payload-bytes")
            .await
            .expect("store blob");
        let (tree, _) = build_merkle_tree(
            store,
            vec![MerkleTreeFile {
                path: "y.txt".to_string(),
                kind: blob_ref.to_dir_entry_kind(false),
            }],
        )
        .await
        .expect("build output tree");
        tree.0
    }

    #[tokio::test]
    async fn reads_declared_output_root_from_cas() {
        let (_temp, store) = store_client().await;
        let tree = output_tree(&store).await;
        assert_eq!(
            read_tree_file(&store, vx_services::NodeHash(tree), "y.txt")
                .await
                .expect("read helper")
                .as_deref(),
            Some(&b"payload-bytes"[..])
        );

        let service = CasOutputTreeService::new(
            Arc::new(store),
            vec![ExecOutput {
                path: "out".to_string(),
                tree,
            }],
        );
        assert_eq!(
            service
                .force(vx_runtime::remote::PartKey::TreePath(
                    "out/y.txt".to_string()
                ))
                .await,
            vx_runtime::remote::Part::Bytes(b"payload-bytes".to_vec())
        );
        assert!(matches!(
            service
                .force(vx_runtime::remote::PartKey::TreePath("y.txt".to_string()))
                .await,
            vx_runtime::remote::Part::Absent(_)
        ));
    }
}
