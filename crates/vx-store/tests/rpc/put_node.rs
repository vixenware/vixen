use vx_store_proto::{
    Blake3Hash, CAS_NODE_VERSION, CDC_MAX_CHUNK, CasNode, ChunkHash, ContentHash, NodeHash, NodeId,
    NodeManifest,
};

use crate::harness::StoreTestEnv;

#[tokio::test(flavor = "multi_thread")]
async fn put_node_roundtrip_success() {
    let env = StoreTestEnv::new().await;
    let node = CasNode::Manifest(NodeManifest {
        node_id: NodeId("store-rpc-test-node".to_string()),
        cache_key: Blake3Hash::from_bytes(b"cache-key"),
        produced_at: "2026-02-16T00:00:00Z".to_string(),
        outputs: Vec::new(),
    });
    let (bytes, hash) = node.to_bytes_and_hash();

    env.client
        .put_node(hash, bytes.clone())
        .await
        .expect("put_node call failed");

    let response = env
        .client
        .get_node(hash)
        .await
        .expect("get_node RPC failed");
    let fetched = response.expect("node not found after put_node");
    assert_eq!(fetched, bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn put_node_rejects_hash_mismatch() {
    let env = StoreTestEnv::new().await;
    let node = CasNode::Manifest(NodeManifest {
        node_id: NodeId("node-hash-mismatch".to_string()),
        cache_key: Blake3Hash::from_bytes(b"k"),
        produced_at: "2026-02-16T00:00:00Z".to_string(),
        outputs: Vec::new(),
    });
    let (bytes, _) = node.to_bytes_and_hash();
    let wrong_hash = NodeHash(Blake3Hash::from_bytes(b"wrong"));

    let err = env.client.put_node(wrong_hash, bytes).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("hash mismatch"),
        "unexpected error: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn put_node_rejects_invalid_postcard_payload() {
    let env = StoreTestEnv::new().await;
    let mut bytes = CAS_NODE_VERSION.to_le_bytes().to_vec();
    bytes.extend_from_slice(&[1, 2, 3, 4, 5]);
    let hash = NodeHash(Blake3Hash::from_bytes(&bytes));

    let err = env.client.put_node(hash, bytes).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("invalid node"),
        "unexpected error: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn put_node_rejects_invalid_blob_structure() {
    let env = StoreTestEnv::new().await;
    // BlobNode must have >=2 chunks; this payload intentionally violates that.
    let invalid_blob = vx_store_proto::BlobNode {
        chunks: vec![vx_store_proto::ChunkEntry {
            hash: ChunkHash(Blake3Hash::from_bytes(b"only-one")),
            size: 1,
        }],
        total_size: CDC_MAX_CHUNK as u64 + 1,
        content_hash: ContentHash(Blake3Hash::from_bytes(b"content")),
        cdc_params_id: 1,
    };
    let node = CasNode::Blob(invalid_blob);
    let (bytes, hash) = node.to_bytes_and_hash();

    let err = env.client.put_node(hash, bytes).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("at least 2 chunks"),
        "unexpected error for invalid blob node: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn put_node_duplicate_short_circuits_validation() {
    let env = StoreTestEnv::new().await;
    let node = CasNode::Manifest(NodeManifest {
        node_id: NodeId("duplicate-node".to_string()),
        cache_key: Blake3Hash::from_bytes(b"duplicate-cache-key"),
        produced_at: "2026-02-16T00:00:00Z".to_string(),
        outputs: Vec::new(),
    });
    let (bytes, hash) = node.to_bytes_and_hash();

    env.client
        .put_node(hash, bytes.clone())
        .await
        .expect("initial put_node call failed");

    let duplicate = b"definitely not a postcard node".to_vec();
    env.client
        .put_node(hash, duplicate)
        .await
        .expect("duplicate put_node should short-circuit");

    let fetched = env
        .client
        .get_node(hash)
        .await
        .expect("get_node RPC failed")
        .expect("node missing after duplicate put");
    assert_eq!(fetched, bytes);
}
