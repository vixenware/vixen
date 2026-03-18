# Eager tree hashing for inline trees

## Problem

When the evaluator produces a `TreeValue::Inline`, its identity (NodeHash) is
unknown until mount preparation time, when `snapshot_exec_inline_tree` walks the
tree, stores each entry in the CAS, and computes the merkle root.

This means the singleflight dedup key for exec mounts (`exec_mounts_singleflight_key`)
cannot use the NodeHash. Instead it recursively walks the entire inline tree,
calling `Value::key()` on every entry, hex-encoding blob hashes, formatting
strings, sorting, and joining — all to produce a dedup key that's thrown away
after the lookup.

Profiling shows this singleflight key computation dominates exec mount preparation
time. For a libpng build, mount prep takes 2-4 seconds per exec call, almost
entirely in `exec_mounts_singleflight_key`. The actual `cc` invocation takes
200ms.

## How merkle tree hashing works in vixen

A `DirectoryNode` is serialized with postcard and hashed with blake3:

```
NodeHash = blake3(postcard_serialize(DirectoryNode {
    entries: Vec<DirEntry>,  // sorted by name
    total_size: u64,
}))
```

Each `DirEntry` has a name and a `DirEntryKind`:
- `SmallFile { content_hash, size, executable }` — single-chunk blob
- `LargeFile { content_hash, blob_node, size, executable }` — multi-chunk blob
- `Dir { hash: NodeHash, total_size }` — subdirectory
- `Symlink { target }` — symlink

The key property of a merkle tree: **you only need the immediate children's
hashes to compute the parent's hash.** You never recurse into descendants.

Example: `Tree %{ "foo" => store_tree }` where `store_tree` is a
`TreeValue::Store(hash)`:

1. `store_tree` already has its `NodeHash` — no computation needed
2. Build `DirEntry { name: "foo", kind: Dir { hash, total_size } }`
3. Build `DirectoryNode { entries: [that_entry], total_size }`
4. `blake3(postcard_serialize(directory_node))` → `NodeHash`

No store access. No recursion. Just serialize the immediate children and hash.

### The `total_size` problem

Step 2 requires `total_size` for the child directory, but this is only available
by loading the child `DirectoryNode` from the store — a store access that
contradicts the "no store access" goal.

Options:
- **Remove `total_size` from `DirEntryKind::Dir`** — keep it only in
  `DirectoryNode` itself (for progress reporting) but not in parent entries.
  This is how git and nix work — directory entries don't carry recursive sizes.
  Changes the on-disk format and all hashes.
- **Carry `total_size` alongside `NodeHash` in `TreeValue::Store`** — avoids
  the store lookup but changes the value representation.

Removing `total_size` from directory entries is the cleaner long-term fix. It
makes tree identity purely structural and eliminates a class of unnecessary
store lookups. This is a separate change that can be done later.

## The FsDir snapshot boundary

`TreeValue::FsDir(path)` is a reference to a mutable directory on the host
filesystem. It cannot be hashed without reading and snapshotting its contents
first.

The snapshot boundary is at exec mount time: when a tree becomes an input to
`exec`, any `FsDir` leaves must be snapshotted into the store, producing
`Store(hash)` nodes. This already happens today in `snapshot_exec_fs_tree_cached`
(cached per-path via `OnceCell`).

The flow for a workspace source tree:

1. `Tree.from_path(@.)` → `FsDir("/workspace")` — cheap, no I/O
2. `.subtree(@src)` → `FsDir("/workspace/src")` — still cheap, narrows the path
3. Used as an exec mount → snapshot `/workspace/src` into the store → `Store(hash)`
4. That hash becomes the mount's identity

**Important:** snapshotting must not happen eagerly. The workspace could be huge,
and only the subtrees actually used as exec inputs should be snapshotted. The
demand-driven evaluation model already ensures this — `FsDir` values are lazy
references that only get snapshotted when forced at the exec boundary.

### The filesystem race window

Between the start of a build and the moment an `FsDir` is snapshotted, the
underlying files can change. A build might read a file during evaluation (to
decide which flags to use) and then snapshot a different version of that file
as an exec input.

This is the same race every build system has. Make, Bazel, Buck2, Cargo — they
all read from mutable disk without locking. Bazel's symlink forest doesn't help
either: the symlinks point at the original files, which can still change.

Vixen's model is actually stronger than Bazel's *after* the snapshot boundary:
once content is in the CAS, exec runs against genuinely immutable data, not
symlinks to mutable files. The race window is only between "start of build" and
"snapshot time."

A future file-watching daemon (like Buck2's) can detect mid-build changes and
invalidate affected computations.

## The fix: reorder snapshot before singleflight key

The current flow:

1. Compute expensive singleflight key (recursive string walk) ← **bottleneck**
2. Check if another flight is already running for this key
3. Snapshot trees into store (produces NodeHash)

The correct flow:

1. Snapshot each mount's tree (cheap if cached, produces NodeHash)
2. Singleflight key = the mount NodeHashes (O(1))

For inline trees with all-content-addressed children, the NodeHash can be
computed locally without store access (serialize + blake3). For FsDir leaves,
the snapshot is already cached per-path. Either way, the result has a hash, and
that hash is the singleflight key.

## Tree operations and semantics

### Tree literal duplicates

```vx
Tree %{ @a => x, @a => y }
```

This should be a **hard error** — duplicate keys in a tree literal are a typo.
With `Path.from_string`, duplicates can only be caught at runtime (when the tree
is constructed), not statically.

### Tree merging

`[tree_a, tree_b].merge()` intentionally combines entries from multiple trees.
Last entry wins on name conflicts. This produces a new inline tree whose hash
can be computed from its children's hashes.

### Mount layering vs. tree merging

These are distinct operations:

- **Mount layering** — multiple `(path, tree)` pairs handed to the runner. The
  filesystem/sandbox handles the overlay. No new tree is produced. Free.
- **Tree merging** — produces a new tree value with combined entries. Required
  when the merged tree is used as a value (passed around, selected from, etc.).

If you're merging trees just to mount the result at a single path, mount
layering at different sub-paths may be a better fit.

## Environment variables for debugging

- `VX_MAX_CONCURRENCY` — limits the action graph executor's semaphore
- `VX_EXEC_CONCURRENCY` — limits concurrent exec calls at the evaluator level
  (set to 1 for sequential execution and clean profiling)
