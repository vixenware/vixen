//! Content-defined chunking and blob preparation helpers.
//!
//! This logic lives in the shared library layer so clients can prepare
//! blobs and store them without reimplementing FastCDC or BlobNode
//! assembly.

use std::collections::HashSet;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::{
    Blake3Hash, BlobNode, BlobRef, CDC_AVG_CHUNK, CDC_MAX_CHUNK, CDC_MIN_CHUNK, CDC_PARAMS_ID,
    CasNode, ChunkEntry, ChunkHash, ContentHash, MAX_MISSING_CHUNKS_BATCH, MAX_MISSING_NODES_BATCH,
    NodeHash, StoreClient,
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
#[derive(Debug, Clone)]
pub struct SerializedBlobNode {
    /// Hash of serialized_bytes (blake3), used as the node address.
    pub node_hash: NodeHash,
    /// The raw bytes to pass to `put_node`.
    pub serialized_bytes: Vec<u8>,
}

/// Bytes for a chunk that is ready to store.
#[derive(Debug, Clone)]
pub struct ReadyChunk {
    /// Hash of the chunk bytes (used to address in chunks/).
    pub hash: ChunkHash,
    /// Raw chunk bytes.
    pub data: Vec<u8>,
    /// Byte offset of this chunk within the original data.
    pub offset: usize,
}

/// Final result of streaming blob ingestion.
#[derive(Debug)]
pub struct StreamedBlob {
    /// Blake3 of full file content (for integrity verification).
    pub content_hash: ContentHash,
    /// Final blob reference shape.
    pub blob_ref: BlobRef,
    /// Total input size in bytes.
    pub size: u64,
    /// Chunk metadata in file order.
    pub chunks: Vec<ChunkInfo>,
    /// Serialized BlobNode for multi-chunk blobs.
    pub blob_node: Option<SerializedBlobNode>,
}

impl StreamedBlob {
    /// Convert the streamed result into a PreparedBlob-compatible shape.
    pub fn to_prepared_blob(&self) -> PreparedBlob {
        PreparedBlob {
            content_hash: self.content_hash,
            chunks: self.chunks.clone(),
            blob_node: self.blob_node.clone(),
        }
    }
}

#[derive(Debug)]
pub struct FinishedStreamingBlob {
    pub streamed: StreamedBlob,
    pub final_chunk: ReadyChunk,
}

/// Incremental blob builder that preserves the exact `prepare_blob` semantics:
/// - one chunk for total size <= CDC_MAX_CHUNK
/// - FastCDC only once total size exceeds CDC_MAX_CHUNK
#[derive(Debug)]
pub struct StreamingBlobBuilder {
    pending: Vec<u8>,
    offset: usize,
    chunks: Vec<ChunkInfo>,
    content_hasher: blake3::Hasher,
    total_size: u64,
}

impl StreamingBlobBuilder {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            offset: 0,
            chunks: Vec::new(),
            content_hasher: blake3::Hasher::new(),
            total_size: 0,
        }
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<ReadyChunk> {
        if bytes.is_empty() {
            return Vec::new();
        }

        self.content_hasher.update(bytes);
        self.total_size += bytes.len() as u64;
        self.pending.extend_from_slice(bytes);

        let mut ready = Vec::new();
        while self.pending.len() > CDC_MAX_CHUNK as usize {
            let mut chunks = fastcdc::v2020::FastCDC::new(
                &self.pending,
                CDC_MIN_CHUNK,
                CDC_AVG_CHUNK,
                CDC_MAX_CHUNK,
            );
            let Some(chunk) = chunks.next() else {
                break;
            };

            let chunk_len = chunk.length;
            let chunk_data = self.pending.drain(..chunk_len).collect::<Vec<_>>();
            let hash = ChunkHash(Blake3Hash(*blake3::hash(&chunk_data).as_bytes()));
            let offset = self.offset;
            self.offset += chunk_len;

            self.chunks.push(ChunkInfo {
                hash,
                offset,
                size: chunk_len as u32,
            });
            ready.push(ReadyChunk {
                hash,
                data: chunk_data,
                offset,
            });
        }

        ready
    }

    pub fn finish(mut self) -> FinishedStreamingBlob {
        let final_chunk = ReadyChunk {
            hash: ChunkHash(Blake3Hash(*blake3::hash(&self.pending).as_bytes())),
            data: self.pending,
            offset: self.offset,
        };

        self.chunks.push(ChunkInfo {
            hash: final_chunk.hash,
            offset: final_chunk.offset,
            size: final_chunk.data.len() as u32,
        });

        let content_hash = ContentHash(Blake3Hash(*self.content_hasher.finalize().as_bytes()));

        let blob_node = if self.chunks.len() >= 2 {
            let blob = BlobNode {
                chunks: self
                    .chunks
                    .iter()
                    .map(|chunk| ChunkEntry {
                        hash: chunk.hash,
                        size: chunk.size,
                    })
                    .collect(),
                total_size: self.total_size,
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

        let blob_ref = if let Some(blob_node) = &blob_node {
            BlobRef::Large {
                blob_node: blob_node.node_hash,
                content_hash,
                size: self.total_size,
            }
        } else {
            BlobRef::Small {
                chunk: self.chunks[0].hash,
                size: self.total_size,
            }
        };

        FinishedStreamingBlob {
            streamed: StreamedBlob {
                content_hash,
                blob_ref,
                size: self.total_size,
                chunks: self.chunks,
                blob_node,
            },
            final_chunk,
        }
    }
}

impl Default for StreamingBlobBuilder {
    fn default() -> Self {
        Self::new()
    }
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
    let mut builder = StreamingBlobBuilder::new();
    let _ = builder.push_bytes(data);
    builder.finish().streamed.to_prepared_blob()
}

/// Store a prepared blob (chunks plus optional BlobNode) into the store.
pub async fn store_prepared_blob(
    store: &StoreClient,
    prepared: &PreparedBlob,
    data: &[u8],
) -> Result<BlobRef, String> {
    let mut seen_hashes = HashSet::with_capacity(prepared.chunks.len());
    let mut unique_hashes = Vec::with_capacity(prepared.chunks.len());
    for chunk in &prepared.chunks {
        if seen_hashes.insert(chunk.hash) {
            unique_hashes.push(chunk.hash);
        }
    }

    let missing_hashes = list_missing_chunks(store, unique_hashes).await?;
    let missing_hashes: HashSet<_> = missing_hashes.into_iter().collect();

    let mut uploaded_hashes = HashSet::with_capacity(missing_hashes.len());
    for chunk in &prepared.chunks {
        if !missing_hashes.contains(&chunk.hash) || !uploaded_hashes.insert(chunk.hash) {
            continue;
        }
        let chunk_data = &data[chunk.offset..chunk.offset + chunk.size as usize];
        store
            .put_chunk(chunk.hash, chunk_data.to_vec())
            .await
            .map_err(|error| format!("{error:?}"))?;
    }

    if let Some(blob_node) = &prepared.blob_node {
        put_node_if_missing(
            store,
            blob_node.node_hash,
            blob_node.serialized_bytes.clone(),
        )
        .await?;

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

/// Store a blob from an AsyncRead source using the shared streaming CDC pipeline.
///
/// This preserves the exact same chunking semantics as `prepare_blob()`:
/// a single chunk for total size <= CDC_MAX_CHUNK, otherwise canonical FastCDC.
pub async fn store_blob_stream<R: AsyncRead + Unpin + ?Sized>(
    store: &StoreClient,
    reader: &mut R,
) -> Result<StreamedBlob, String> {
    let mut builder = StreamingBlobBuilder::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut uploaded_hashes = HashSet::new();

    loop {
        let read = reader
            .read(&mut buf)
            .await
            .map_err(|error| format!("{error}"))?;
        if read == 0 {
            break;
        }

        for chunk in builder.push_bytes(&buf[..read]) {
            if uploaded_hashes.insert(chunk.hash) {
                store
                    .put_chunk(chunk.hash, chunk.data)
                    .await
                    .map_err(|error| format!("{error:?}"))?;
            }
        }
    }

    let finished = builder.finish();
    let streamed = finished.streamed;
    let final_chunk = finished.final_chunk;

    if uploaded_hashes.insert(final_chunk.hash) {
        store
            .put_chunk(final_chunk.hash, final_chunk.data)
            .await
            .map_err(|error| format!("{error:?}"))?;
    }

    if let Some(blob_node) = &streamed.blob_node {
        put_node_if_missing(
            store,
            blob_node.node_hash,
            blob_node.serialized_bytes.clone(),
        )
        .await?;
    }

    Ok(streamed)
}

pub async fn list_missing_chunks(
    store: &StoreClient,
    hashes: Vec<ChunkHash>,
) -> Result<Vec<ChunkHash>, String> {
    let mut missing = Vec::new();
    for batch in hashes.chunks(MAX_MISSING_CHUNKS_BATCH) {
        let batch_missing = store
            .list_missing_chunks(batch.to_vec())
            .await
            .map_err(|error| format!("{error:?}"))?;
        missing.extend(batch_missing);
    }
    Ok(missing)
}

pub async fn put_chunk_if_missing(
    store: &StoreClient,
    hash: ChunkHash,
    data: Vec<u8>,
) -> Result<(), String> {
    if store
        .has_chunk(hash)
        .await
        .map_err(|error| format!("{error:?}"))?
    {
        return Ok(());
    }

    store
        .put_chunk(hash, data)
        .await
        .map_err(|error| format!("{error:?}"))
}

pub async fn put_node_if_missing(
    store: &StoreClient,
    hash: NodeHash,
    data: Vec<u8>,
) -> Result<(), String> {
    let missing = list_missing_nodes(store, vec![hash]).await?;
    if missing.is_empty() {
        return Ok(());
    }

    store
        .put_node(hash, data)
        .await
        .map_err(|error| format!("{error:?}"))
}

pub async fn list_missing_nodes(
    store: &StoreClient,
    hashes: Vec<NodeHash>,
) -> Result<Vec<NodeHash>, String> {
    let mut missing = Vec::new();
    for batch in hashes.chunks(MAX_MISSING_NODES_BATCH) {
        let batch_missing = store
            .list_missing_nodes(batch.to_vec())
            .await
            .map_err(|error| format!("{error:?}"))?;
        missing.extend(batch_missing);
    }
    Ok(missing)
}

pub async fn put_nodes_if_missing(
    store: &StoreClient,
    nodes: Vec<(NodeHash, Vec<u8>)>,
) -> Result<(), String> {
    let missing = list_missing_nodes(store, nodes.iter().map(|(hash, _)| *hash).collect()).await?;
    let missing: HashSet<_> = missing.into_iter().collect();

    for (hash, data) in nodes {
        if !missing.contains(&hash) {
            continue;
        }
        store
            .put_node(hash, data)
            .await
            .map_err(|error| format!("{error:?}"))?;
    }

    Ok(())
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

    #[test]
    fn streaming_builder_matches_prepare_blob_for_small_inputs() {
        let data = b"hello streamed world";
        let prepared = prepare_blob(data);

        let mut builder = StreamingBlobBuilder::new();
        let ready = builder.push_bytes(&data[..5]);
        assert!(ready.is_empty());
        let ready = builder.push_bytes(&data[5..]);
        assert!(ready.is_empty());

        let finished = builder.finish();
        let streamed = finished.streamed;
        assert_eq!(streamed.content_hash, prepared.content_hash);
        assert_eq!(streamed.chunks.len(), prepared.chunks.len());
        assert_eq!(streamed.blob_node.is_none(), prepared.blob_node.is_none());
        for (left, right) in streamed.chunks.iter().zip(prepared.chunks.iter()) {
            assert_eq!(left.hash, right.hash);
            assert_eq!(left.offset, right.offset);
            assert_eq!(left.size, right.size);
        }
    }

    #[test]
    fn streaming_builder_matches_prepare_blob_for_large_inputs() {
        let data = vec![0xCD; (CDC_MAX_CHUNK as usize) + (CDC_MIN_CHUNK as usize) + 12345];
        let prepared = prepare_blob(&data);

        let mut builder = StreamingBlobBuilder::new();
        for chunk in data.chunks(97 * 1024) {
            let _ = builder.push_bytes(chunk);
        }

        let finished = builder.finish();
        let streamed = finished.streamed;
        assert_eq!(streamed.content_hash, prepared.content_hash);
        assert_eq!(streamed.chunks.len(), prepared.chunks.len());
        assert_eq!(streamed.blob_node.is_some(), prepared.blob_node.is_some());
        for (left, right) in streamed.chunks.iter().zip(prepared.chunks.iter()) {
            assert_eq!(left.hash, right.hash);
            assert_eq!(left.offset, right.offset);
            assert_eq!(left.size, right.size);
        }

        let left_blob = streamed.blob_node.expect("streamed blob node");
        let right_blob = prepared.blob_node.expect("prepared blob node");
        assert_eq!(left_blob.node_hash, right_blob.node_hash);
        assert_eq!(left_blob.serialized_bytes, right_blob.serialized_bytes);
    }
}
