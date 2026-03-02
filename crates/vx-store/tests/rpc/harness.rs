use std::net::TcpListener;
use std::time::Duration;

use tempfile::TempDir;
use vx_io::spawn::{ServiceBuilder, ServiceHandle};
use vx_store_proto::{Blake3Hash, CasNode, NodeHash, NodeId, NodeManifest, StoreClient};

type Caller = roam_core::DriverCaller;

pub struct StoreTestEnv {
    _root: TempDir,
    _vx_home: TempDir,
    _service: ServiceHandle,
    pub client: StoreClient<Caller>,
}

impl StoreTestEnv {
    pub async fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();

        let root = TempDir::new().expect("failed to create store root tempdir");
        let vx_home = TempDir::new().expect("failed to create vx_home tempdir");
        let endpoint = free_local_endpoint();

        let root_str = root.path().to_string_lossy().into_owned();
        let binary_path = resolve_vx_store_binary();

        let service = ServiceBuilder::new(binary_path, endpoint)
            .caller_name("vx-store-rpc-tests")
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
        let client = StoreClient::new(virtual_handle);

        Self {
            _root: root,
            _vx_home: vx_home,
            _service: service,
            client,
        }
    }

    pub async fn put_test_manifest_node(&self, seed: &str) -> NodeHash {
        let node = CasNode::Manifest(NodeManifest {
            node_id: NodeId(format!("manifest-{seed}")),
            cache_key: Blake3Hash::from_bytes(seed.as_bytes()),
            produced_at: "2026-02-16T00:00:00Z".to_string(),
            outputs: Vec::new(),
        });
        let (bytes, hash) = node.to_bytes_and_hash();
        self.client
            .put_node(hash, bytes)
            .await
            .expect("failed to put test manifest node");
        hash
    }
}

fn free_local_endpoint() -> vx_io::net::Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral test port");
    let addr = listener
        .local_addr()
        .expect("failed to read ephemeral test port");
    drop(listener);
    vx_io::net::Endpoint::parse(&addr.to_string()).expect("failed to parse test endpoint")
}

fn resolve_vx_store_binary() -> String {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_vx-store")
        && !path.is_empty()
    {
        return path;
    }

    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/vx-store")
        .to_string_lossy()
        .into_owned()
}
