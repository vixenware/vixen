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
        let path = self.cache_path(namespace, key)?;
        let json = std::fs::read_to_string(path).ok()?;
        facet_json::from_str(&json).ok()
    }

    pub fn write<K: for<'a> facet::Facet<'a>, V: for<'a> facet::Facet<'a>>(
        &self,
        namespace: &str,
        key: &K,
        value: &V,
    ) {
        let Some(path) = self.cache_path(namespace, key) else {
            return;
        };
        let Ok(json) = facet_json::to_string(value) else {
            return;
        };
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

    pub fn cache_path<K: for<'a> facet::Facet<'a>>(
        &self,
        namespace: &str,
        key: &K,
    ) -> Option<PathBuf> {
        let digest = self.key_digest(key)?;
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
