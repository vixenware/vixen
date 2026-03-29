use vx_services::{Blake3Hash, NodeHash};

use crate::harness::StoreTestEnv;

#[tokio::test(flavor = "multi_thread")]
async fn publish_rejects_missing_manifest_node() {
    let env = StoreTestEnv::new().await;
    let cache_key = Blake3Hash::from_bytes(b"cache-key-missing-node");
    let missing_manifest = NodeHash(Blake3Hash::from_bytes(b"manifest-not-present"));

    let result = env
        .client
        .publish(cache_key, missing_manifest)
        .await
        .expect("publish call failed");

    assert!(!result.success, "publish unexpectedly succeeded");
    let error = result
        .error
        .clone()
        .expect("publish failure should include an error");
    assert!(
        error.contains("does not exist"),
        "unexpected publish error: {error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_lookup_and_unpublish_roundtrip() {
    let env = StoreTestEnv::new().await;
    let manifest_hash = env.put_test_manifest_node("publish-roundtrip").await;
    let cache_key = Blake3Hash::from_bytes(b"cache-key-roundtrip");

    let publish = env
        .client
        .publish(cache_key, manifest_hash)
        .await
        .expect("publish call failed");
    assert!(publish.success, "publish failed: {:?}", publish.error);

    let lookup = env
        .client
        .lookup(cache_key)
        .await
        .expect("lookup call failed");
    assert_eq!(lookup, Some(manifest_hash));

    let deleted = env
        .client
        .unpublish(cache_key)
        .await
        .expect("unpublish call failed");
    assert!(deleted, "expected unpublish to delete existing mapping");

    let lookup_after = env
        .client
        .lookup(cache_key)
        .await
        .expect("lookup call failed after unpublish");
    assert_eq!(lookup_after, None);

    let deleted_again = env
        .client
        .unpublish(cache_key)
        .await
        .expect("second unpublish call failed");
    assert!(
        !deleted_again,
        "second unpublish should report no mapping to delete"
    );
}
