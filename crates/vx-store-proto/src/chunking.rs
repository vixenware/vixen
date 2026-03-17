//! Content-defined chunking and blob preparation helpers.
//!
//! This logic lives in the shared library layer so clients can prepare
//! blobs and store them without reimplementing FastCDC or BlobNode
//! assembly.

use crate::{
    Blake3Hash, BlobNode, BlobRef, CDC_AVG_CHUNK, CDC_MAX_CHUNK, CDC_MIN_CHUNK, CDC_PARAMS_ID,
    CasNode, ChunkEntry, ChunkHash, ContentHash, NodeHash, StoreClient,
};

/// Information about a single chunk produced by CDC.
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    /// Hash of the chunk bytes (used to address in chunks/).
    pub hash: ChunkHash,
    /// Byte offset of this chunk within the original data.
    pub offset: usize,
    /// Size of this chunk in bytes.
    pub size: u32,
}

/// Result of preparing a blob for storage.
#[derive(Debug)]
pub struct PreparedBlob {
    /// Blake3 of full file content (for integrity verification).
    pub content_hash: ContentHash,
    /// The chunks that make up this blob.
    pub chunks: Vec<ChunkInfo>,
    /// If the file is multi-chunk (> CDC_MAX_CHUNK), the serialized BlobNode
    /// ready to pass to `put_node`. None for single-chunk files.
    pub blob_node: Option<SerializedBlobNode>,
}

impl PreparedBlob {
    /// Convert to a `BlobRef` (without storing anything).
    pub fn to_blob_ref(&self) -> BlobRef {
        if let Some(blob_node) = &self.blob_node {
            BlobRef::Large {
                blob_node: blob_node.node_hash,
                content_hash: self.content_hash,
                size: self.chunks.iter().map(|c| c.size as u64).sum(),
            }
        } else {
            BlobRef::Small {
                chunk: self.chunks[0].hash,
                size: self.chunks[0].size as u64,
            }
        }
    }
}

/// A serialized BlobNode ready for storage.
#[derive(Debug)]
pub struct SerializedBlobNode {
    /// Hash of serialized_bytes (blake3), used as the node address.
    pub node_hash: NodeHash,
    /// The raw bytes to pass to `put_node`.
    pub serialized_bytes: Vec<u8>,
}

/// Chunk data using FastCDC with the canonical CDC parameters.
///
/// For data <= CDC_MAX_CHUNK, returns a single chunk (no CDC needed).
/// For larger data, runs FastCDC to produce content-defined chunks.
pub fn chunk_data(data: &[u8]) -> Vec<ChunkInfo> {
    if data.is_empty() {
        let hash = ChunkHash(Blake3Hash(*blake3::hash(data).as_bytes()));
        return vec![ChunkInfo {
            hash,
            offset: 0,
            size: 0,
        }];
    }

    if data.len() <= CDC_MAX_CHUNK as usize {
        let hash = ChunkHash(Blake3Hash(*blake3::hash(data).as_bytes()));
        return vec![ChunkInfo {
            hash,
            offset: 0,
            size: data.len() as u32,
        }];
    }

    fastcdc::v2020::FastCDC::new(data, CDC_MIN_CHUNK, CDC_AVG_CHUNK, CDC_MAX_CHUNK)
        .map(|chunk| {
            let chunk_data = &data[chunk.offset..chunk.offset + chunk.length];
            let hash = ChunkHash(Blake3Hash(*blake3::hash(chunk_data).as_bytes()));
            ChunkInfo {
                hash,
                offset: chunk.offset,
                size: chunk.length as u32,
            }
        })
        .collect()
}

/// Chunk data and compute the content hash + BlobNode if needed.
pub fn prepare_blob(data: &[u8]) -> PreparedBlob {
    let content_hash = ContentHash(Blake3Hash(*blake3::hash(data).as_bytes()));
    let chunks = chunk_data(data);

    let blob_node = if chunks.len() >= 2 {
        let blob = BlobNode {
            chunks: chunks
                .iter()
                .map(|chunk| ChunkEntry {
                    hash: chunk.hash,
                    size: chunk.size,
                })
                .collect(),
            total_size: data.len() as u64,
            content_hash,
            cdc_params_id: CDC_PARAMS_ID,
        };

        let node = CasNode::Blob(blob);
        let (serialized_bytes, node_hash) = node.to_bytes_and_hash();

        Some(SerializedBlobNode {
            node_hash,
            serialized_bytes,
        })
    } else {
        None
    };

    PreparedBlob {
        content_hash,
        chunks,
        blob_node,
    }
}

/// Store a prepared blob (chunks plus optional BlobNode) into the store.
pub async fn store_prepared_blob(
    store: &StoreClient,
    prepared: &PreparedBlob,
    data: &[u8],
) -> Result<BlobRef, String> {
    for chunk in &prepared.chunks {
        let chunk_data = &data[chunk.offset..chunk.offset + chunk.size as usize];
        store
            .put_chunk(chunk.hash, chunk_data.to_vec())
            .await
            .map_err(|error| format!("{error:?}"))?;
    }

    if let Some(blob_node) = &prepared.blob_node {
        store
            .put_node(blob_node.node_hash, blob_node.serialized_bytes.clone())
            .await
            .map_err(|error| format!("{error:?}"))?;

        Ok(BlobRef::Large {
            blob_node: blob_node.node_hash,
            content_hash: prepared.content_hash,
            size: data.len() as u64,
        })
    } else {
        debug_assert_eq!(prepared.chunks.len(), 1);
        Ok(BlobRef::Small {
            chunk: prepared.chunks[0].hash,
            size: data.len() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_one_empty_chunk() {
        let chunks = chunk_data(b"");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].size, 0);
        assert_eq!(chunks[0].offset, 0);
    }

    #[test]
    fn small_input_stays_single_chunk() {
        let data = b"hello world";
        let chunks = chunk_data(data);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].size, data.len() as u32);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(
            chunks[0].hash,
            ChunkHash(Blake3Hash(*blake3::hash(data).as_bytes()))
        );
    }

    #[test]
    fn exact_max_chunk_boundary_stays_single_chunk() {
        let data = vec![0x42; CDC_MAX_CHUNK as usize];
        let chunks = chunk_data(&data);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].size, CDC_MAX_CHUNK);
    }

    #[test]
    fn just_over_max_chunk_uses_multiple_chunks() {
        let data = vec![0x42; CDC_MAX_CHUNK as usize + 1];
        let chunks = chunk_data(&data);
        assert!(chunks.len() >= 2);
        assert_eq!(
            chunks.iter().map(|chunk| chunk.size).sum::<u32>() as usize,
            data.len()
        );
    }

    #[test]
    fn chunking_is_deterministic() {
        let data = vec![0x55; 8 * 1024 * 1024];
        let first = chunk_data(&data);
        let second = chunk_data(&data);
        assert_eq!(first.len(), second.len());
        for (left, right) in first.iter().zip(second.iter()) {
            assert_eq!(left.hash, right.hash);
            assert_eq!(left.offset, right.offset);
            assert_eq!(left.size, right.size);
        }
    }

    #[test]
    fn prepare_blob_omits_blob_node_for_single_chunk_inputs() {
        let data = b"single chunk";
        let prepared = prepare_blob(data);
        assert!(prepared.blob_node.is_none());
        assert_eq!(prepared.chunks.len(), 1);
        assert_eq!(
            prepared.content_hash,
            ContentHash(Blake3Hash(*blake3::hash(data).as_bytes()))
        );
    }

    #[test]
    fn prepare_blob_emits_blob_node_for_large_inputs() {
        let data = vec![0xAB; (CDC_MAX_CHUNK as usize) + 8192];
        let prepared = prepare_blob(&data);
        assert!(prepared.blob_node.is_some());
        assert!(prepared.chunks.len() >= 2);
        assert_eq!(
            prepared.content_hash,
            ContentHash(Blake3Hash(*blake3::hash(&data).as_bytes()))
        );
    }
}
