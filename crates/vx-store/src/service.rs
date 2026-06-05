use crate::types::StoreService;
use moire::task::FutureExt;
use vx_services::Store;
use vx_services::{
    Blake3Hash, BlobNode, CDC_MAX_CHUNK, CacheRecordBody, CacheRecordNode, CasNode, ChunkHash,
    DirectoryEntryKind, DirectoryNode, KNOWN_CDC_PARAMS, MAX_CHUNKS_PER_BLOB,
    MAX_MISSING_CHUNKS_BATCH, MAX_MISSING_NODES_BATCH, MAX_NODE_BYTES, NodeHash,
};

impl StoreService {
    // =========================================================================
    // Chunk operations
    // =========================================================================

    async fn put_chunk(&self, hash: ChunkHash, data: Vec<u8>) -> Result<(), String> {
        if self.present_chunks.lock().contains(&hash) {
            return Ok(());
        }
        if self
            .storage
            .has_chunk(&hash)
            .named("storage.has_chunk")
            .await
            .map_err(|e| format!("storage error: {e}"))?
        {
            self.present_chunks.lock().insert(hash);
            return Ok(());
        }

        if data.len() > CDC_MAX_CHUNK as usize {
            return Err(format!(
                "chunk too large: {} bytes (max {})",
                data.len(),
                CDC_MAX_CHUNK
            ));
        }

        let actual = Blake3Hash::from_bytes(&data);
        if actual != hash.0 {
            return Err(format!(
                "hash mismatch: expected {}, got {}",
                hash.to_hex(),
                actual.to_hex()
            ));
        }

        self.storage
            .put_chunk(&hash, &data)
            .named("storage.put_chunk")
            .await
            .map_err(|e| format!("storage error: {e}"))?;
        self.present_chunks.lock().insert(hash);
        Ok(())
    }

    async fn get_chunk(&self, hash: ChunkHash) -> Option<Vec<u8>> {
        let data = self.storage.get_chunk(&hash).await.ok().flatten();
        if data.is_some() {
            self.present_chunks.lock().insert(hash);
            return data;
        }
        let upstream = self.upstream();
        let data = upstream?.get_chunk(hash).await.ok().flatten()?;
        if StoreService::put_chunk(self, hash, data.clone())
            .await
            .is_ok()
        {
            return Some(data);
        }
        None
    }

    async fn has_chunk(&self, hash: ChunkHash) -> bool {
        if self.present_chunks.lock().contains(&hash) {
            return true;
        }
        let present = self.storage.has_chunk(&hash).await.unwrap_or(false);
        if present {
            self.present_chunks.lock().insert(hash);
            return true;
        }
        let Some(upstream) = self.upstream() else {
            return false;
        };
        let present = upstream.has_chunk(hash).await.unwrap_or(false);
        if present {
            self.present_chunks.lock().insert(hash);
        }
        present
    }

    async fn list_missing_chunks(&self, hashes: Vec<ChunkHash>) -> Vec<ChunkHash> {
        let hashes = if hashes.len() > MAX_MISSING_CHUNKS_BATCH {
            tracing::warn!(
                count = hashes.len(),
                max = MAX_MISSING_CHUNKS_BATCH,
                "list_missing_chunks: batch too large, truncating"
            );
            hashes.into_iter().take(MAX_MISSING_CHUNKS_BATCH).collect()
        } else {
            hashes
        };

        let mut missing = Vec::new();
        for hash in hashes {
            if self.present_chunks.lock().contains(&hash) {
                continue;
            }
            match self.storage.has_chunk(&hash).await {
                Ok(true) => {
                    self.present_chunks.lock().insert(hash);
                }
                _ => {
                    if let Some(upstream) = self.upstream()
                        && upstream.has_chunk(hash).await.unwrap_or(false)
                    {
                        self.present_chunks.lock().insert(hash);
                        continue;
                    }
                    missing.push(hash);
                }
            }
        }
        missing
    }

    // =========================================================================
    // Node operations
    // =========================================================================

    async fn put_node(&self, hash: NodeHash, data: Vec<u8>) -> Result<(), String> {
        if self.present_nodes.lock().contains(&hash) {
            return Ok(());
        }
        if self
            .storage
            .has_node(&hash)
            .named("storage.has_node")
            .await
            .map_err(|e| format!("storage error: {e}"))?
        {
            self.present_nodes.lock().insert(hash);
            return Ok(());
        }

        if data.len() > MAX_NODE_BYTES {
            return Err(format!(
                "node too large: {} bytes (max {})",
                data.len(),
                MAX_NODE_BYTES
            ));
        }

        let actual = Blake3Hash::from_bytes(&data);
        if actual != hash.0 {
            return Err(format!(
                "hash mismatch: expected {}, got {}",
                hash.to_hex(),
                actual.to_hex()
            ));
        }

        let node = CasNode::from_bytes(&data).map_err(|e| format!("invalid node: {e}"))?;
        match &node {
            CasNode::Blob(blob) => validate_blob_node(blob)?,
            CasNode::Directory(dir) => validate_directory_node(dir)?,
            CasNode::CacheRecord(record) => validate_cache_record_node(record)?,
        }

        self.storage
            .put_node(&hash, &data)
            .await
            .map_err(|e| format!("storage error: {e}"))?;
        self.present_nodes.lock().insert(hash);
        Ok(())
    }

    async fn get_node(&self, hash: NodeHash) -> Option<Vec<u8>> {
        let data = self.storage.get_node(&hash).await.ok().flatten();
        if data.is_some() {
            self.present_nodes.lock().insert(hash);
            return data;
        }
        let upstream = self.upstream();
        let data = upstream?.get_node(hash).await.ok().flatten()?;
        if StoreService::put_node(self, hash, data.clone())
            .await
            .is_ok()
        {
            return Some(data);
        }
        None
    }

    async fn has_node(&self, hash: NodeHash) -> bool {
        if self.present_nodes.lock().contains(&hash) {
            return true;
        }
        let present = self.storage.has_node(&hash).await.unwrap_or(false);
        if present {
            self.present_nodes.lock().insert(hash);
            return true;
        }
        let Some(upstream) = self.upstream() else {
            return false;
        };
        let present = upstream.has_node(hash).await.unwrap_or(false);
        if present {
            self.present_nodes.lock().insert(hash);
        }
        present
    }

    async fn list_missing_nodes(&self, hashes: Vec<NodeHash>) -> Vec<NodeHash> {
        let hashes = if hashes.len() > MAX_MISSING_NODES_BATCH {
            tracing::warn!(
                count = hashes.len(),
                max = MAX_MISSING_NODES_BATCH,
                "list_missing_nodes: batch too large, truncating"
            );
            hashes.into_iter().take(MAX_MISSING_NODES_BATCH).collect()
        } else {
            hashes
        };

        let mut missing = Vec::new();
        for hash in hashes {
            if self.present_nodes.lock().contains(&hash) {
                continue;
            }
            match self.storage.has_node(&hash).await {
                Ok(true) => {
                    self.present_nodes.lock().insert(hash);
                }
                _ => {
                    if let Some(upstream) = self.upstream()
                        && upstream.has_node(hash).await.unwrap_or(false)
                    {
                        self.present_nodes.lock().insert(hash);
                        continue;
                    }
                    missing.push(hash);
                }
            }
        }
        missing
    }
}

impl Store for StoreService {
    async fn put_chunk(&self, hash: ChunkHash, data: Vec<u8>) -> Result<(), String> {
        StoreService::put_chunk(self, hash, data).await
    }

    async fn get_chunk(&self, hash: ChunkHash) -> Option<Vec<u8>> {
        StoreService::get_chunk(self, hash).await
    }

    async fn has_chunk(&self, hash: ChunkHash) -> bool {
        StoreService::has_chunk(self, hash).await
    }

    async fn list_missing_chunks(&self, hashes: Vec<ChunkHash>) -> Vec<ChunkHash> {
        StoreService::list_missing_chunks(self, hashes).await
    }

    async fn put_node(&self, hash: NodeHash, data: Vec<u8>) -> Result<(), String> {
        StoreService::put_node(self, hash, data).await
    }

    async fn get_node(&self, hash: NodeHash) -> Option<Vec<u8>> {
        StoreService::get_node(self, hash).await
    }

    async fn has_node(&self, hash: NodeHash) -> bool {
        StoreService::has_node(self, hash).await
    }

    async fn list_missing_nodes(&self, hashes: Vec<NodeHash>) -> Vec<NodeHash> {
        StoreService::list_missing_nodes(self, hashes).await
    }
}

// =============================================================================
// Validation helpers
// =============================================================================

fn validate_blob_node(blob: &BlobNode) -> Result<(), String> {
    if blob.chunks.len() < 2 {
        return Err(format!(
            "BlobNode must have at least 2 chunks, got {}",
            blob.chunks.len()
        ));
    }

    if blob.chunks.len() > MAX_CHUNKS_PER_BLOB {
        return Err(format!(
            "BlobNode has {} chunks (max {})",
            blob.chunks.len(),
            MAX_CHUNKS_PER_BLOB
        ));
    }

    let mut sum: u64 = 0;
    for (i, chunk) in blob.chunks.iter().enumerate() {
        if chunk.size == 0 || chunk.size > CDC_MAX_CHUNK {
            return Err(format!(
                "chunk[{i}] size {} is out of range 1..={}",
                chunk.size, CDC_MAX_CHUNK
            ));
        }
        sum += chunk.size as u64;
    }

    if sum != blob.total_size {
        return Err(format!(
            "BlobNode total_size mismatch: sum of chunks is {sum}, declared {}",
            blob.total_size
        ));
    }

    if blob.total_size <= CDC_MAX_CHUNK as u64 {
        return Err(format!(
            "BlobNode total_size {} is <= CDC_MAX_CHUNK ({}), should be SmallFile",
            blob.total_size, CDC_MAX_CHUNK
        ));
    }

    if !KNOWN_CDC_PARAMS
        .iter()
        .any(|(id, _, _, _)| *id == blob.cdc_params_id)
    {
        return Err(format!("unknown cdc_params_id: {}", blob.cdc_params_id));
    }

    Ok(())
}

fn validate_directory_node(dir: &DirectoryNode) -> Result<(), String> {
    let entries = &dir.entries;

    for (i, entry) in entries.iter().enumerate() {
        if entry.name.is_empty() {
            return Err(format!("entry[{i}] has empty name"));
        }

        if entry.name == "." || entry.name == ".." {
            return Err(format!("entry[{i}] has forbidden name \"{}\"", entry.name));
        }

        if entry.name.contains('/') {
            return Err(format!("entry[{i}] name \"{}\" contains '/'", entry.name));
        }

        match &entry.kind {
            DirectoryEntryKind::SmallFile { size, .. } => {
                if *size > CDC_MAX_CHUNK as u64 {
                    return Err(format!(
                        "entry[{i}] \"{}\" is SmallFile but size {} > CDC_MAX_CHUNK ({})",
                        entry.name, size, CDC_MAX_CHUNK
                    ));
                }
            }
            DirectoryEntryKind::LargeFile { size, .. } => {
                if *size <= CDC_MAX_CHUNK as u64 {
                    return Err(format!(
                        "entry[{i}] \"{}\" is LargeFile but size {} <= CDC_MAX_CHUNK ({})",
                        entry.name, size, CDC_MAX_CHUNK
                    ));
                }
            }
            DirectoryEntryKind::Dir { .. } | DirectoryEntryKind::Symlink { .. } => {}
        }

        if i > 0 {
            let prev = &entries[i - 1].name;
            match prev.as_bytes().cmp(entry.name.as_bytes()) {
                std::cmp::Ordering::Greater => {
                    return Err(format!(
                        "entries not sorted: \"{}\" comes after \"{}\"",
                        entry.name, prev
                    ));
                }
                std::cmp::Ordering::Equal => {
                    return Err(format!("duplicate entry name: \"{}\"", entry.name));
                }
                std::cmp::Ordering::Less => {}
            }
        }
    }

    Ok(())
}

fn validate_cache_record_node(record: &CacheRecordNode) -> Result<(), String> {
    if record.namespace.is_empty() {
        return Err("CacheRecord namespace must not be empty".to_string());
    }
    if let CacheRecordBody::Opaque { format, .. } = &record.body
        && format.is_empty()
    {
        return Err("CacheRecord opaque body format must not be empty".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use vx_services::{
        Blake3Hash, CacheInputKeyDigest, CacheRecordBody, CacheRecordNode, CacheRecordValue,
        CasNode, Store,
    };

    #[tokio::test]
    async fn cache_record_nodes_round_trip_through_store() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("store")).expect("utf8 store path");
        let store = crate::open(root).await.expect("open store");
        let node = CasNode::CacheRecord(CacheRecordNode {
            namespace: "function-output-v1".to_string(),
            input_key: CacheInputKeyDigest(Blake3Hash::from_bytes(b"key")),
            body: CacheRecordBody::Value(CacheRecordValue::String("cached".to_string())),
            read_witness: None,
        });
        let (bytes, hash) = node.to_bytes_and_hash();

        Store::put_node(&store, hash, bytes.clone())
            .await
            .expect("cache record enters store");
        let fetched = Store::get_node(&store, hash)
            .await
            .expect("cache record is fetched by node hash");

        assert_eq!(fetched, bytes);
    }
}
