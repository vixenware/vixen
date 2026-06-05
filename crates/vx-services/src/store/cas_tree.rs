//! Build Merkle directory trees from flat file lists.
//!
//! Converts a list of (relative_path, DirectoryEntryKind) pairs into a recursive
//! DirectoryNode tree stored in the CAS.

use std::collections::BTreeMap;

use crate::{
    BlobNode, CasNode, ChunkHash, ContentHash, DirectoryEntry, DirectoryEntryKind, DirectoryNode,
    NodeHash, StoreClient, chunking::put_nodes_if_missing,
};

/// A file to include in the directory tree.
pub struct MerkleTreeFile {
    /// Full relative path (e.g. "src/main.rs").
    pub path: String,
    /// The entry kind (SmallFile, LargeFile, Symlink).
    pub kind: DirectoryEntryKind,
}

/// Split "foo/bar/baz" into ("foo/bar", "baz"). Root entries: ("", "name").
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("", path),
    }
}

/// Ensure all parent directories of `dir` exist in the map.
fn ensure_parent_dirs(dir_entries: &mut BTreeMap<String, Vec<DirectoryEntry>>, dir: &str) {
    let mut current = dir.to_string();
    while let Some(pos) = current.rfind('/') {
        current = current[..pos].to_string();
        dir_entries.entry(current.clone()).or_default();
    }
    if !current.is_empty() {
        dir_entries.entry(String::new()).or_default();
    }
}

/// Build a Merkle directory tree from flat file entries and store all nodes
/// in the CAS. Returns the root (NodeHash, total_size).
pub async fn build_merkle_tree(
    cas: &StoreClient,
    files: Vec<MerkleTreeFile>,
) -> Result<(NodeHash, u64), String> {
    let mut dir_entries: BTreeMap<String, Vec<DirectoryEntry>> = BTreeMap::new();

    // Group files by parent directory
    for file in files {
        let (parent, name) = split_path(&file.path);
        dir_entries
            .entry(parent.to_string())
            .or_default()
            .push(DirectoryEntry {
                name: name.to_string(),
                kind: file.kind,
            });
    }

    // Ensure intermediate directories exist
    let all_dirs: Vec<String> = dir_entries.keys().cloned().collect();
    for dir in &all_dirs {
        ensure_parent_dirs(&mut dir_entries, dir);
    }

    // Process directories bottom-up (deepest first)
    let mut dir_list: Vec<String> = dir_entries.keys().cloned().collect();
    dir_list.sort_by(|a, b| {
        let a_depth = if a.is_empty() {
            0
        } else {
            a.matches('/').count() + 1
        };
        let b_depth = if b.is_empty() {
            0
        } else {
            b.matches('/').count() + 1
        };
        b_depth.cmp(&a_depth).then_with(|| a.cmp(b))
    });

    let mut dir_hashes: BTreeMap<String, (NodeHash, u64)> = BTreeMap::new();
    let mut pending_nodes = Vec::with_capacity(dir_list.len());

    for dir_path in &dir_list {
        let entries = dir_entries.get_mut(dir_path).unwrap();

        // Sort entries by name (raw byte comparison)
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

        // Compute total_size for this directory
        let mut total_size: u64 = 0;
        for entry in entries.iter() {
            match &entry.kind {
                DirectoryEntryKind::SmallFile { size, .. } => total_size += size,
                DirectoryEntryKind::LargeFile { size, .. } => total_size += size,
                DirectoryEntryKind::Dir { .. } => {
                    let child_path = if dir_path.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{dir_path}/{}", entry.name)
                    };
                    if let Some(&(_, child_total)) = dir_hashes.get(&child_path) {
                        total_size += child_total;
                    }
                }
                DirectoryEntryKind::Symlink { .. } => {}
            }
        }

        let dir_node = CasNode::Directory(DirectoryNode {
            entries: entries.clone(),
            total_size,
        });

        let (node_bytes, node_hash) = dir_node.to_bytes_and_hash();
        pending_nodes.push((node_hash, node_bytes));

        dir_hashes.insert(dir_path.clone(), (node_hash, total_size));

        // Add this directory as a Dir entry in the parent
        if !dir_path.is_empty() {
            let (parent, name) = split_path(dir_path);
            dir_entries
                .entry(parent.to_string())
                .or_default()
                .push(DirectoryEntry {
                    name: name.to_string(),
                    kind: DirectoryEntryKind::Dir { hash: node_hash },
                });
        }
    }

    put_nodes_if_missing(cas, pending_nodes)
        .await
        .map_err(|e| format!("failed to store Merkle directory nodes: {e}"))?;

    dir_hashes
        .get("")
        .copied()
        .ok_or_else(|| "no root directory".to_string())
}

/// Read one regular file out of a CAS-backed Merkle directory tree.
///
/// Returns `Ok(None)` when the path is absent or names a non-file entry. Missing or
/// malformed CAS nodes/chunks are integrity errors and return `Err`.
pub async fn read_tree_file(
    cas: &StoreClient,
    root: NodeHash,
    path: &str,
) -> Result<Option<Vec<u8>>, String> {
    let components = normalize_tree_path(path)?;
    if components.is_empty() {
        return Ok(None);
    }

    let mut dir = fetch_directory(cas, root).await?;
    for (index, component) in components.iter().enumerate() {
        let Some(entry) = dir
            .entries
            .iter()
            .find(|entry| entry.name.as_str() == component.as_str())
        else {
            return Ok(None);
        };
        let is_leaf = index + 1 == components.len();
        match (&entry.kind, is_leaf) {
            (
                DirectoryEntryKind::SmallFile {
                    content_hash, size, ..
                },
                true,
            ) => return read_small_file(cas, *content_hash, *size).await.map(Some),
            (
                DirectoryEntryKind::LargeFile {
                    content_hash,
                    blob_node,
                    size,
                    ..
                },
                true,
            ) => {
                return read_large_file(cas, *blob_node, *content_hash, *size)
                    .await
                    .map(Some);
            }
            (DirectoryEntryKind::Dir { hash }, false) => {
                dir = fetch_directory(cas, *hash).await?;
            }
            _ => return Ok(None),
        }
    }

    Ok(None)
}

fn normalize_tree_path(path: &str) -> Result<Vec<String>, String> {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" => {}
            "." | ".." => {
                return Err(format!(
                    "tree path `{path}` contains illegal component `{component}`"
                ));
            }
            _ => components.push(component.to_string()),
        }
    }
    Ok(components)
}

async fn fetch_directory(cas: &StoreClient, hash: NodeHash) -> Result<DirectoryNode, String> {
    let data = cas
        .get_node(hash)
        .await
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| format!("missing CAS directory node {hash}"))?;
    match CasNode::from_bytes(&data).map_err(|error| format!("invalid CAS node {hash}: {error}"))? {
        CasNode::Directory(directory) => Ok(directory),
        CasNode::Blob(_) => Err(format!("CAS node {hash} is a blob, expected directory")),
        CasNode::CacheRecord(_) => Err(format!(
            "CAS node {hash} is a cache record, expected directory"
        )),
    }
}

async fn fetch_blob(cas: &StoreClient, hash: NodeHash) -> Result<BlobNode, String> {
    let data = cas
        .get_node(hash)
        .await
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| format!("missing CAS blob node {hash}"))?;
    match CasNode::from_bytes(&data).map_err(|error| format!("invalid CAS node {hash}: {error}"))? {
        CasNode::Blob(blob) => Ok(blob),
        CasNode::Directory(_) => Err(format!("CAS node {hash} is a directory, expected blob")),
        CasNode::CacheRecord(_) => Err(format!("CAS node {hash} is a cache record, expected blob")),
    }
}

async fn read_small_file(
    cas: &StoreClient,
    content_hash: ContentHash,
    size: u64,
) -> Result<Vec<u8>, String> {
    let data = cas
        .get_chunk(ChunkHash(content_hash.0))
        .await
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| format!("missing CAS chunk {}", content_hash.0))?;
    verify_file_bytes(&data, content_hash, size)?;
    Ok(data)
}

async fn read_large_file(
    cas: &StoreClient,
    blob_node: NodeHash,
    content_hash: ContentHash,
    size: u64,
) -> Result<Vec<u8>, String> {
    let blob = fetch_blob(cas, blob_node).await?;
    let mut data = Vec::with_capacity(blob.total_size as usize);
    for chunk in blob.chunks {
        let bytes = cas
            .get_chunk(chunk.hash)
            .await
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| format!("missing CAS chunk {}", chunk.hash))?;
        if bytes.len() != chunk.size as usize {
            return Err(format!(
                "CAS chunk {} has {} bytes, expected {}",
                chunk.hash,
                bytes.len(),
                chunk.size
            ));
        }
        data.extend_from_slice(&bytes);
    }
    verify_file_bytes(&data, content_hash, size)?;
    Ok(data)
}

fn verify_file_bytes(data: &[u8], content_hash: ContentHash, size: u64) -> Result<(), String> {
    if data.len() as u64 != size {
        return Err(format!(
            "CAS file has {} bytes, expected {size}",
            data.len()
        ));
    }
    let actual = crate::Blake3Hash::from_bytes(data);
    if actual != content_hash.0 {
        return Err(format!(
            "CAS file content hash mismatch: expected {}, got {}",
            content_hash.0, actual
        ));
    }
    Ok(())
}
