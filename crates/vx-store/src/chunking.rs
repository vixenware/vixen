//! Content-Defined Chunking using FastCDC.
//!
//! CDC parameters are protocol constants defined in `vx-store-proto`.
//! The `fastcdc` crate version is pinned in workspace Cargo.toml to prevent
//! boundary behavior changes across versions.
//!
//! All chunking, assembly, and Merkle tree construction logic lives here
//! (client-side), not in the store. The store is a dumb validated KV store.

use vx_store_proto::{
    Blake3Hash, BlobNode, CDC_AVG_CHUNK, CDC_MAX_CHUNK, CDC_MIN_CHUNK, CDC_PARAMS_ID, CasNode,
    ChunkEntry, ChunkHash, ContentHash, NodeHash,
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
        // Empty data = single empty chunk
        let hash = ChunkHash(Blake3Hash(*blake3::hash(data).as_bytes()));
        return vec![ChunkInfo {
            hash,
            offset: 0,
            size: 0,
        }];
    }

    if data.len() <= CDC_MAX_CHUNK as usize {
        // Single chunk — no CDC needed
        let hash = ChunkHash(Blake3Hash(*blake3::hash(data).as_bytes()));
        return vec![ChunkInfo {
            hash,
            offset: 0,
            size: data.len() as u32,
        }];
    }

    // Multi-chunk — run FastCDC
    let chunker = fastcdc::v2020::FastCDC::new(data, CDC_MIN_CHUNK, CDC_AVG_CHUNK, CDC_MAX_CHUNK);

    chunker
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
///
/// For single-chunk files (≤ CDC_MAX_CHUNK), `blob_node` is None.
/// For multi-chunk files, `blob_node` contains the serialized BlobNode
/// ready to pass to `put_node`.
pub fn prepare_blob(data: &[u8]) -> PreparedBlob {
    let content_hash = ContentHash(Blake3Hash(*blake3::hash(data).as_bytes()));
    let chunks = chunk_data(data);

    let blob_node = if chunks.len() >= 2 {
        // Multi-chunk file — create a BlobNode
        let blob = BlobNode {
            chunks: chunks
                .iter()
                .map(|c| ChunkEntry {
                    hash: c.hash,
                    size: c.size,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_input() {
        let chunks = chunk_data(b"");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].size, 0);
        assert_eq!(chunks[0].offset, 0);
    }

    #[test]
    fn test_small_input() {
        let data = b"hello world";
        let chunks = chunk_data(data);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].size, data.len() as u32);
        assert_eq!(chunks[0].offset, 0);

        // Hash should match blake3 of the data
        let expected = ChunkHash(Blake3Hash(*blake3::hash(data).as_bytes()));
        assert_eq!(chunks[0].hash, expected);
    }

    #[test]
    fn test_exact_max_chunk_boundary() {
        // Data exactly at CDC_MAX_CHUNK should be a single chunk
        let data = vec![0x42u8; CDC_MAX_CHUNK as usize];
        let chunks = chunk_data(&data);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].size, CDC_MAX_CHUNK);
    }

    #[test]
    fn test_just_over_max_chunk() {
        // Data just over CDC_MAX_CHUNK should produce multiple chunks
        let data = vec![0x42u8; CDC_MAX_CHUNK as usize + 1];
        let chunks = chunk_data(&data);
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got {}",
            chunks.len()
        );

        // Total size should equal input size
        let total: u32 = chunks.iter().map(|c| c.size).sum();
        assert_eq!(total as usize, data.len());
    }

    #[test]
    fn test_multi_chunk_coverage() {
        // 10MB of pseudo-random data
        let mut data = vec![0u8; 10 * 1024 * 1024];
        // Fill with deterministic pseudo-random bytes
        let mut state: u64 = 0xdeadbeef;
        for byte in data.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 33) as u8;
        }

        let chunks = chunk_data(&data);

        // Should have multiple chunks
        assert!(chunks.len() >= 2, "10MB should produce multiple chunks");

        // Chunks should cover the entire input
        let total: usize = chunks.iter().map(|c| c.size as usize).sum();
        assert_eq!(total, data.len());

        // Chunks should be contiguous
        let mut offset = 0;
        for chunk in &chunks {
            assert_eq!(chunk.offset, offset);
            offset += chunk.size as usize;
        }

        // Each chunk should be within bounds
        for chunk in &chunks {
            assert!(
                chunk.size <= CDC_MAX_CHUNK,
                "chunk too large: {}",
                chunk.size
            );
        }
    }

    #[test]
    fn test_determinism() {
        // Same input always produces same chunks
        let data = vec![0x55u8; 8 * 1024 * 1024];
        let chunks1 = chunk_data(&data);
        let chunks2 = chunk_data(&data);

        assert_eq!(chunks1.len(), chunks2.len());
        for (a, b) in chunks1.iter().zip(chunks2.iter()) {
            assert_eq!(a.hash, b.hash);
            assert_eq!(a.offset, b.offset);
            assert_eq!(a.size, b.size);
        }
    }

    #[test]
    fn test_reassembly_verification() {
        // Chunk, then verify reassembly matches original
        let mut data = vec![0u8; 6 * 1024 * 1024];
        let mut state: u64 = 0xcafebabe;
        for byte in data.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 33) as u8;
        }

        let chunks = chunk_data(&data);
        let mut reassembled = Vec::with_capacity(data.len());
        for chunk in &chunks {
            reassembled.extend_from_slice(&data[chunk.offset..chunk.offset + chunk.size as usize]);
        }
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_prepare_blob_small() {
        let data = b"small file contents";
        let prepared = prepare_blob(data);

        assert!(
            prepared.blob_node.is_none(),
            "single-chunk file should not have BlobNode"
        );
        assert_eq!(prepared.chunks.len(), 1);
        assert_eq!(
            prepared.content_hash,
            ContentHash(Blake3Hash(*blake3::hash(data).as_bytes()))
        );
    }

    #[test]
    fn test_prepare_blob_large() {
        // 10MB of data
        let data = vec![0xAB; 10 * 1024 * 1024];
        let prepared = prepare_blob(&data);

        assert!(
            prepared.blob_node.is_some(),
            "multi-chunk file should have BlobNode"
        );
        assert!(prepared.chunks.len() >= 2);
        assert_eq!(
            prepared.content_hash,
            ContentHash(Blake3Hash(*blake3::hash(&data).as_bytes()))
        );

        let blob_node = prepared.blob_node.unwrap();
        // Node hash should be blake3 of serialized bytes
        let expected_hash = NodeHash(Blake3Hash(
            *blake3::hash(&blob_node.serialized_bytes).as_bytes(),
        ));
        assert_eq!(blob_node.node_hash, expected_hash);

        // The serialized bytes should deserialize back to a valid CasNode::Blob
        let decoded = CasNode::from_bytes(&blob_node.serialized_bytes).expect("should deserialize");
        match decoded {
            CasNode::Blob(blob) => {
                assert_eq!(blob.chunks.len(), prepared.chunks.len());
                assert_eq!(blob.total_size, data.len() as u64);
                assert_eq!(blob.content_hash, prepared.content_hash);
                assert_eq!(blob.cdc_params_id, CDC_PARAMS_ID);
            }
            other => panic!("expected CasNode::Blob, got {other:?}"),
        }
    }

    #[test]
    fn test_prepare_blob_empty() {
        let prepared = prepare_blob(b"");
        assert!(prepared.blob_node.is_none());
        assert_eq!(prepared.chunks.len(), 1);
        assert_eq!(prepared.chunks[0].size, 0);
    }

    #[test]
    fn test_chunk_hash_correctness() {
        // Verify each chunk's hash matches blake3 of its actual bytes
        let mut data = vec![0u8; 8 * 1024 * 1024];
        let mut state: u64 = 0x12345678;
        for byte in data.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 33) as u8;
        }

        let chunks = chunk_data(&data);
        for chunk in &chunks {
            let chunk_bytes = &data[chunk.offset..chunk.offset + chunk.size as usize];
            let expected = ChunkHash(Blake3Hash(*blake3::hash(chunk_bytes).as_bytes()));
            assert_eq!(
                chunk.hash, expected,
                "chunk hash mismatch at offset {}",
                chunk.offset
            );
        }
    }
}
