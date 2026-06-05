use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vx_services::{
    CacheIndex, CacheIndexKey, CacheLookupRequest, CacheLookupResult, CachePutRequest, NodeHash,
};

#[derive(Clone, Default)]
pub struct CacheIndexService {
    inner: Arc<Mutex<HashMap<CacheIndexKey, Vec<NodeHash>>>>,
}

impl CacheIndexService {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CacheIndex for CacheIndexService {
    async fn lookup_cache_records(&self, request: CacheLookupRequest) -> CacheLookupResult {
        let limit = if request.limit == 0 {
            usize::MAX
        } else {
            request.limit as usize
        };
        let records = self
            .inner
            .lock()
            .unwrap()
            .get(&request.key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .collect();
        CacheLookupResult { records }
    }

    async fn put_cache_record(&self, request: CachePutRequest) -> Result<(), String> {
        if request.key.namespace.is_empty() {
            return Err("cache index namespace must not be empty".to_string());
        }
        let mut index = self.inner.lock().unwrap();
        let records = index.entry(request.key).or_default();
        if !records.contains(&request.record) {
            records.push(request.record);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use vx_services::{
        Blake3Hash, CacheIndex, CacheIndexKey, CacheInputKeyDigest, CacheLookupRequest,
        CachePutRequest, NodeHash,
    };

    use super::CacheIndexService;

    fn hash(seed: u8) -> Blake3Hash {
        Blake3Hash(*blake3::hash(&[seed]).as_bytes())
    }

    #[tokio::test]
    async fn cache_index_returns_records_for_the_pre_run_key() {
        let service = CacheIndexService::new();
        let key = CacheIndexKey {
            namespace: "exec-value-v1".to_string(),
            input_key: CacheInputKeyDigest(hash(1)),
        };
        let record = NodeHash(hash(2));

        service
            .put_cache_record(CachePutRequest {
                key: key.clone(),
                record,
            })
            .await
            .expect("record enters index");
        service
            .put_cache_record(CachePutRequest {
                key: key.clone(),
                record,
            })
            .await
            .expect("duplicate insert is idempotent");

        let lookup = service
            .lookup_cache_records(CacheLookupRequest { key, limit: 8 })
            .await;

        assert_eq!(lookup.records, vec![record]);
    }

    #[tokio::test]
    async fn cache_index_rejects_empty_namespaces() {
        let service = CacheIndexService::new();
        let result = service
            .put_cache_record(CachePutRequest {
                key: CacheIndexKey {
                    namespace: String::new(),
                    input_key: CacheInputKeyDigest(hash(1)),
                },
                record: NodeHash(hash(2)),
            })
            .await;

        assert_eq!(
            result,
            Err("cache index namespace must not be empty".to_string())
        );
    }
}
