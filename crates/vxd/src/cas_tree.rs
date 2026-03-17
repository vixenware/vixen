//! Thin wrappers around `vx_store_proto::cas_tree`.

#[allow(dead_code)]
pub(crate) type TreeFile = vx_store_proto::cas_tree::MerkleTreeFile;

#[allow(dead_code)]
pub(crate) async fn build_merkle_tree(
    store: &vx_store_proto::StoreClient,
    files: Vec<TreeFile>,
) -> Result<(vx_store_proto::NodeHash, u64), String> {
    vx_store_proto::cas_tree::build_merkle_tree(store, files).await
}
