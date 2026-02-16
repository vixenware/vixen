use vx_store_proto::{Blake3Hash, ChunkHash, MAX_MISSING_CHUNKS_BATCH};

use crate::harness::StoreTestEnv;

#[tokio::test(flavor = "multi_thread")]
async fn list_missing_chunks_returns_only_missing_in_input_order() {
    let env = StoreTestEnv::new().await;

    let present_a_data = b"present-a".to_vec();
    let present_a = ChunkHash(Blake3Hash::from_bytes(&present_a_data));
    env.client
        .put_chunk(present_a, present_a_data)
        .await
        .expect("failed to seed present chunk A");

    let present_b_data = b"present-b".to_vec();
    let present_b = ChunkHash(Blake3Hash::from_bytes(&present_b_data));
    env.client
        .put_chunk(present_b, present_b_data)
        .await
        .expect("failed to seed present chunk B");

    let missing_a = ChunkHash(Blake3Hash::from_bytes(b"missing-a"));
    let missing_b = ChunkHash(Blake3Hash::from_bytes(b"missing-b"));
    let query = vec![present_a, missing_a, present_b, missing_b];

    let missing = env
        .client
        .list_missing_chunks(query)
        .await
        .expect("list_missing_chunks call failed");

    assert_eq!(missing, vec![missing_a, missing_b]);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_missing_chunks_truncates_to_batch_cap() {
    let env = StoreTestEnv::new().await;
    let requested: Vec<_> = (0..(MAX_MISSING_CHUNKS_BATCH + 17))
        .map(|i| ChunkHash(Blake3Hash::from_bytes(format!("missing-{i}").as_bytes())))
        .collect();

    let missing = env
        .client
        .list_missing_chunks(requested.clone())
        .await
        .expect("list_missing_chunks call failed");

    assert_eq!(missing.len(), MAX_MISSING_CHUNKS_BATCH);
    assert_eq!(missing, requested[..MAX_MISSING_CHUNKS_BATCH].to_vec());
}
