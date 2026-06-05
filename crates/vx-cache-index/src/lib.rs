//! Temporary local cache-index substrate.
//!
//! This crate owns the compatibility file layout used while the runtime cache is
//! moving toward the daemon/control-plane index plus `vx-store` record bodies.
//! It is deliberately generic over Facet key/value types: callers still own the
//! pre-run input-key law, but the evaluator no longer owns the disk-index
//! mechanics.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct LocalCacheIndex {
    root: PathBuf,
}

impl LocalCacheIndex {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn read<K: for<'a> facet::Facet<'a>, V: for<'a> facet::Facet<'a>>(
        &self,
        namespace: &str,
        key: &K,
    ) -> Option<V> {
        let digest = self.key_digest(key)?;
        self.read_digest(namespace, &digest)
    }

    pub fn write<K: for<'a> facet::Facet<'a>, V: for<'a> facet::Facet<'a>>(
        &self,
        namespace: &str,
        key: &K,
        value: &V,
    ) {
        let Some(digest) = self.key_digest(key) else {
            return;
        };
        self.write_digest(namespace, &digest, value);
    }

    pub fn read_digest<V: for<'a> facet::Facet<'a>>(
        &self,
        namespace: &str,
        digest: &str,
    ) -> Option<V> {
        let json = self.read_record(namespace, digest)?;
        facet_json::from_str(&json).ok()
    }

    pub fn write_digest<V: for<'a> facet::Facet<'a>>(
        &self,
        namespace: &str,
        digest: &str,
        value: &V,
    ) {
        let Some(path) = self.cache_path_for_digest(namespace, digest) else {
            return;
        };
        let Ok(json) = facet_json::to_string(value) else {
            return;
        };
        write_json(path, &json);
    }

    pub fn read_record(&self, namespace: &str, digest: &str) -> Option<String> {
        let path = self.cache_path_for_digest(namespace, digest)?;
        std::fs::read_to_string(path).ok()
    }

    pub fn write_record(&self, namespace: &str, digest: &str, record: &str) {
        let Some(path) = self.cache_path_for_digest(namespace, digest) else {
            return;
        };
        write_json(path, record);
    }

    pub fn cache_path<K: for<'a> facet::Facet<'a>>(
        &self,
        namespace: &str,
        key: &K,
    ) -> Option<PathBuf> {
        let digest = self.key_digest(key)?;
        self.cache_path_for_digest(namespace, &digest)
    }

    pub fn cache_path_for_digest(&self, namespace: &str, digest: &str) -> Option<PathBuf> {
        if namespace.is_empty() || !valid_digest_hex(digest) {
            return None;
        }
        Some(
            self.root
                .join(namespace)
                .join(&digest[..2])
                .join(format!("{digest}.json")),
        )
    }

    pub fn key_digest<K: for<'a> facet::Facet<'a>>(&self, key: &K) -> Option<String> {
        let key_json = facet_json::to_string(key).ok()?;
        Some(blake3::hash(key_json.as_bytes()).to_hex().to_string())
    }
}

fn write_json(path: PathBuf, json: &str) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut tmp = path.clone();
    tmp.set_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&tmp, json.as_bytes()).is_err() {
        return;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(tmp);
    }
}

fn valid_digest_hex(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::LocalCacheIndex;

    #[derive(facet::Facet, Debug, PartialEq, Eq)]
    struct CacheValue {
        value: String,
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "vx-cache-index-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn typed_key_and_digest_address_the_same_record() {
        let root = temp_root("typed-key");
        let index = LocalCacheIndex::new(&root);
        let key = CacheValue {
            value: "key".to_string(),
        };
        let digest = index.key_digest(&key).expect("key digest");

        assert_eq!(
            index.cache_path("ns", &key),
            index.cache_path_for_digest("ns", &digest)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn digest_addressed_records_round_trip_as_typed_values() {
        let root = temp_root("digest-record");
        let index = LocalCacheIndex::new(&root);
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        index.write_record("records", digest, "{\"value\":\"cached\"}");

        assert_eq!(
            index.read_digest::<CacheValue>("records", digest),
            Some(CacheValue {
                value: "cached".to_string()
            })
        );
        assert_eq!(
            index.read_record("records", digest),
            Some("{\"value\":\"cached\"}".to_string())
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
