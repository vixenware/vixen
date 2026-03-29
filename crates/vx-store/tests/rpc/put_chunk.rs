use vx_services::{Blake3Hash, CDC_MAX_CHUNK, ChunkHash};

use crate::harness::StoreTestEnv;

#[tokio::test(flavor = "multi_thread")]
async fn put_chunk_roundtrip_success() {
    let env = StoreTestEnv::new().await;
    let data = b"hello from put_chunk_roundtrip_success".to_vec();
    let hash = ChunkHash(Blake3Hash::from_bytes(&data));

    env.client
        .put_chunk(hash, data.clone())
        .await
        .expect("put_chunk call failed");

    let response = env
        .client
        .get_chunk(hash)
        .await
        .expect("get_chunk RPC failed");
    let fetched = response.as_ref().expect("chunk not found after put_chunk");
    assert_eq!(fetched, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn put_chunk_rejects_hash_mismatch() {
    let env = StoreTestEnv::new().await;
    let data = b"chunk bytes".to_vec();
    let wrong_hash = ChunkHash(Blake3Hash::from_bytes(b"something else"));

    let err = env.client.put_chunk(wrong_hash, data).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("hash mismatch"),
        "unexpected error: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn put_chunk_rejects_oversized_payload() {
    let env = StoreTestEnv::new().await;
    let data = vec![0xAB; CDC_MAX_CHUNK as usize + 1];
    let hash = ChunkHash(Blake3Hash::from_bytes(&data));

    let err = env.client.put_chunk(hash, data).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("chunk too large"),
        "unexpected error: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn put_chunk_duplicate_short_circuits_validation() {
    let env = StoreTestEnv::new().await;
    let original = b"stored chunk bytes".to_vec();
    let hash = ChunkHash(Blake3Hash::from_bytes(&original));

    env.client
        .put_chunk(hash, original.clone())
        .await
        .expect("initial put_chunk call failed");

    let duplicate = b"different bytes, same claimed hash".to_vec();
    env.client
        .put_chunk(hash, duplicate)
        .await
        .expect("duplicate put_chunk should short-circuit");

    let fetched = env
        .client
        .get_chunk(hash)
        .await
        .expect("get_chunk RPC failed")
        .expect("chunk missing after duplicate put");
    assert_eq!(fetched, original);
}
