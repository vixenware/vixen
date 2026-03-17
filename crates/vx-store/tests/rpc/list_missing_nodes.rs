use vx_store_proto::{
    Blake3Hash, CasNode, MAX_MISSING_NODES_BATCH, NodeHash, NodeId, NodeManifest,
};

use crate::harness::StoreTestEnv;

#[tokio::test(flavor = "multi_thread")]
async fn list_missing_nodes_returns_only_missing_in_input_order() {
    let env = StoreTestEnv::new().await;

    let present_a_node = CasNode::Manifest(NodeManifest {
        node_id: NodeId("present-node-a".to_string()),
        cache_key: Blake3Hash::from_bytes(b"present-node-a"),
        produced_at: "2026-02-16T00:00:00Z".to_string(),
        outputs: Vec::new(),
    });
    let (present_a_bytes, present_a) = present_a_node.to_bytes_and_hash();
    env.client
        .put_node(present_a, present_a_bytes)
        .await
        .expect("failed to seed present node A");

    let present_b_node = CasNode::Manifest(NodeManifest {
        node_id: NodeId("present-node-b".to_string()),
        cache_key: Blake3Hash::from_bytes(b"present-node-b"),
        produced_at: "2026-02-16T00:00:00Z".to_string(),
        outputs: Vec::new(),
    });
    let (present_b_bytes, present_b) = present_b_node.to_bytes_and_hash();
    env.client
        .put_node(present_b, present_b_bytes)
        .await
        .expect("failed to seed present node B");

    let missing_a = NodeHash(Blake3Hash::from_bytes(b"missing-node-a"));
    let missing_b = NodeHash(Blake3Hash::from_bytes(b"missing-node-b"));
    let query = vec![present_a, missing_a, present_b, missing_b];

    let missing = env
        .client
        .list_missing_nodes(query)
        .await
        .expect("list_missing_nodes call failed");

    assert_eq!(&missing[..], &[missing_a, missing_b]);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_missing_nodes_truncates_to_batch_cap() {
    let env = StoreTestEnv::new().await;
    let requested: Vec<_> = (0..(MAX_MISSING_NODES_BATCH + 17))
        .map(|i| {
            NodeHash(Blake3Hash::from_bytes(
                format!("missing-node-{i}").as_bytes(),
            ))
        })
        .collect();

    let missing = env
        .client
        .list_missing_nodes(requested.clone())
        .await
        .expect("list_missing_nodes call failed");

    assert_eq!(missing.len(), MAX_MISSING_NODES_BATCH);
    assert_eq!(&missing, &requested[..MAX_MISSING_NODES_BATCH]);
}
