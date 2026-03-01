//! Build Merkle directory trees from flat file lists.
//!
//! Converts a list of (relative_path, DirEntryKind) pairs into a recursive
//! DirectoryNode tree stored in the CAS.

use std::collections::BTreeMap;

use crate::{CasNode, DirEntry, DirEntryKind, DirectoryNode, NodeHash, StoreClient};

/// A file to include in the directory tree.
pub struct MerkleTreeFile {
    /// Full relative path (e.g. "src/main.rs").
    pub path: String,
    /// The entry kind (SmallFile, LargeFile, Symlink).
    pub kind: DirEntryKind,
}

/// Split "foo/bar/baz" into ("foo/bar", "baz"). Root entries: ("", "name").
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("", path),
    }
}

/// Ensure all parent directories of `dir` exist in the map.
fn ensure_parent_dirs(dir_entries: &mut BTreeMap<String, Vec<DirEntry>>, dir: &str) {
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
pub async fn build_merkle_tree<C: roam::Caller>(
    cas: &StoreClient<C>,
    files: Vec<MerkleTreeFile>,
) -> Result<(NodeHash, u64), String> {
    let mut dir_entries: BTreeMap<String, Vec<DirEntry>> = BTreeMap::new();

    // Group files by parent directory
    for file in files {
        let (parent, name) = split_path(&file.path);
        dir_entries
            .entry(parent.to_string())
            .or_default()
            .push(DirEntry {
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

    for dir_path in &dir_list {
        let entries = dir_entries.get_mut(dir_path).unwrap();

        // Sort entries by name (raw byte comparison)
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

        // Compute total_size for this directory
        let mut total_size: u64 = 0;
        for entry in entries.iter() {
            match &entry.kind {
                DirEntryKind::SmallFile { size, .. } => total_size += size,
                DirEntryKind::LargeFile { size, .. } => total_size += size,
                DirEntryKind::Dir { total_size: ts, .. } => total_size += ts,
                DirEntryKind::Symlink { .. } => {}
            }
        }

        let dir_node = CasNode::Directory(DirectoryNode {
            entries: entries.clone(),
            total_size,
        });

        let (node_bytes, node_hash) = dir_node.to_bytes_and_hash();
        cas.put_node(node_hash, node_bytes)
            .await
            .map_err(|e| format!("failed to store DirectoryNode for '{}': {e:?}", dir_path))?;

        dir_hashes.insert(dir_path.clone(), (node_hash, total_size));

        // Add this directory as a Dir entry in the parent
        if !dir_path.is_empty() {
            let (parent, name) = split_path(dir_path);
            dir_entries
                .entry(parent.to_string())
                .or_default()
                .push(DirEntry {
                    name: name.to_string(),
                    kind: DirEntryKind::Dir {
                        hash: node_hash,
                        total_size,
                    },
                });
        }
    }

    dir_hashes
        .get("")
        .copied()
        .ok_or_else(|| "no root directory".to_string())
}
