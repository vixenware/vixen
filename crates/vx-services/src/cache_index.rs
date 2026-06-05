//! Cache-index service protocol definitions.
//!
//! The index is control-plane state: it maps a pre-run input-key digest to
//! candidate cache-record CAS nodes. Cache record bodies live in `Store`.

use facet::Facet;

use crate::{CacheInputKeyDigest, NodeHash};

pub const CACHE_INDEX_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct CacheIndexKey {
    pub namespace: String,
    pub input_key: CacheInputKeyDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct CacheLookupRequest {
    pub key: CacheIndexKey,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct CacheLookupResult {
    pub records: Vec<NodeHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct CachePutRequest {
    pub key: CacheIndexKey,
    pub record: NodeHash,
}

#[vox::service]
pub trait CacheIndex {
    async fn lookup_cache_records(&self, request: CacheLookupRequest) -> CacheLookupResult;

    async fn put_cache_record(&self, request: CachePutRequest) -> Result<(), String>;
}
