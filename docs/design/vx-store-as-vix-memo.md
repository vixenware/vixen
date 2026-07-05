# vx-store as the vix memo substrate

Thesis, from Amos: `fetch(url, cksum)` should cache by virtue of being a vix
invocation, not because it has a separate cache. Fetch is a pure deterministic
function. If the demand engine's memo persists into the content-addressed store,
then fetched crates, build outputs, and other vix results cache through the same
mechanism. The store is the cache.

This document maps what exists today and the design needed to make that true.

## Verdict

`vx-store` is a real CAS, but it is not digest-compatible with the open vix
machine today.

- `vx-store` addresses chunks and nodes by BLAKE3. The storage layout is
  `chunks/blake3/{xx}/{hash}` and `nodes/blake3/{xx}/{hash}`
  (`crates/vx-store/src/storage.rs:1-5`, `crates/vx-store/src/storage.rs:31-47`).
- `vx-services` makes that semantic, not just a directory name:
  `Blake3Hash`, `ChunkHash`, `NodeHash`, and `ContentHash` are all BLAKE3-backed
  types (`crates/vx-services/src/store.rs:77-85`,
  `crates/vx-services/src/store.rs:157-213`), and `CasNode::to_bytes_and_hash`
  computes `NodeHash(blake3(serialized_bytes))`
  (`crates/vx-services/src/store.rs:239-279`).
- vix's machine store keys are SHA-256 content hashes. `ContentHash` is
  `[u8; 32]` (`/Users/amos/oss/facet/vix/src/machine/driver.rs:561-562`), the
  driver imports `sha2::Sha256` (`/Users/amos/oss/facet/vix/src/machine/driver.rs:35-36`),
  and scalar/value/tree hash paths finalize SHA-256 digests
  (`/Users/amos/oss/facet/vix/src/machine/driver.rs:860-878`,
  `/Users/amos/oss/facet/vix/src/machine/driver.rs:1078-1096`,
  `/Users/amos/oss/facet/vix/src/machine/driver.rs:5987-5998`).

So, today, a vix store value is not a `vx-store` object by identity. A bridge can
persist vix objects inside `vx-store`, but that bridge would either add a
SHA-256 address family to the store or introduce a separate SHA-256-to-BLAKE3
index. The first option matches the thesis; the second works operationally but
weakens "the vix value is the store object by identity" into "the vix value is
indexed by a store claim."

## What vx-store is today

`vx-store` is a file-backed content-addressed storage service library. `open`
creates the directory layout and returns a `StoreService`
(`crates/vx-store/src/lib.rs:17-23`). The concrete backend is `FsStorage` inside
`CasStore`, with atomic file writes for chunks and nodes
(`crates/vx-store/src/storage.rs:18-62`, `crates/vx-store/src/storage.rs:102-142`).
There is no sqlite, mmap, LMDB, redb, sled, or database dependency in the crate
manifest (`crates/vx-store/Cargo.toml:16-31`).

The public store protocol is in `vx-services`, not `vx-store`. It has two object
classes:

- chunks: raw bytes, `blake3(raw_bytes)`, max 4 MB
  (`crates/vx-services/src/store.rs:37-44`,
  `crates/vx-services/src/store.rs:157-160`);
- nodes: version-prefixed `facet-postcard` `CasNode` bytes,
  `blake3(node_bytes)`, max 16 MB (`crates/vx-services/src/store.rs:27-31`,
  `crates/vx-services/src/store.rs:239-279`,
  `crates/vx-services/src/store.rs:657-667`).

The service API is `put/get/has/list_missing` for both chunks and nodes
(`crates/vx-services/src/store.rs:626-679`). `vx-store` validates both hash
equality and node structure on writes (`crates/vx-store/src/service.rs:15-54`,
`crates/vx-store/src/service.rs:131-176`). The `list_missing_*` calls and
`put_*` early returns are already dedup hooks: clients can avoid uploading
objects the store has, and repeated writes become no-ops
(`crates/vx-store/src/service.rs:15-28`, `crates/vx-store/src/service.rs:92-124`,
`crates/vx-store/src/service.rs:131-143`,
`crates/vx-store/src/service.rs:214-247`).

Large file storage is client-shaped. Shared helpers use FastCDC with canonical
parameters, write chunk objects, and create `BlobNode`s for multi-chunk blobs
(`crates/vx-services/src/store/chunking.rs:110-229`). Trees are Merkle
directories: sorted `DirectoryEntry`s become `DirectoryNode`s, and tree roots are
`NodeHash`es (`crates/vx-services/src/store.rs:390-450`,
`crates/vx-services/src/store/cas_tree.rs:41-143`).

The current service has an in-memory positive presence cache and optional
read-through upstream store (`crates/vx-store/src/types.rs:6-42`,
`crates/vx-store/src/service.rs:56-90`, `crates/vx-store/src/service.rs:178-212`).
That is a tier hook, but not the full tier/tenant design yet. The architecture
doc defines the intended split: bytes are self-verifying, claims need policy,
tiers are content residency, and tenants are claim namespaces plus storage
isolation (`docs/design/store-architecture.md:8-41`). The present `vx-store`
crate does not implement GC, leases, tenant roots, or a claim-policy engine; the
Store trait exposes only chunk/node object operations.

`vx-services` already has a cache-record shape and a separate cache-index
protocol:

- `CacheRecordNode { namespace, input_key, body, read_witness }` is a CAS node
  (`crates/vx-services/src/store.rs:311-349`);
- `CacheIndex` maps `(namespace, input_key)` to candidate cache-record nodes
  (`crates/vx-services/src/cache_index.rs:1-40`).

That is close to the "claims are policy" layer, but the key digest is currently
`CacheInputKeyDigest(pub Blake3Hash)`, so it does not directly accept vix's
SHA-256 memo keys (`crates/vx-services/src/store.rs:311-315`).

## What the vix ValueStore and memo are today

The open machine is already structured around content identity, but only in RAM.

`ValueStore` is:

```text
entries: Vec<StoreEntry>
by_content: HashMap<(schema, content_hash), handle>
```

with `StoreEntry { schema, bytes, content_hash }`
(`/Users/amos/oss/facet/vix/src/machine/driver.rs:651-662`). Allocations compute
canonical content hashes and dedupe by `(schema, content_hash)`
(`/Users/amos/oss/facet/vix/src/machine/driver.rs:836-858`,
`/Users/amos/oss/facet/vix/src/machine/driver.rs:1078-1096`). Handles are
session-local indexes into `entries`, not persistent identity
(`/Users/amos/oss/facet/vix/src/machine/driver.rs:800-833`).

The demand memo is separate from the value store:

- `CanonMemoKey = (closure_hash, Vec<arg_content_hash>)`
  (`/Users/amos/oss/facet/vix/src/machine/driver.rs:561-562`);
- `MemoEntry { value, read_set }`
  (`/Users/amos/oss/facet/vix/src/machine/driver.rs:567-571`);
- `Driver` owns `memo`, `memo_candidates`, `journal`, `exec_cache`, fetch/exec
  backends, projection memos, and `store`
  (`/Users/amos/oss/facet/vix/src/machine/driver.rs:1259-1286`).

The INVOKE protocol is the memo boundary: `HostCall(INVOKE)` asks the driver for
a callee identity and arguments; a memo hit fills the awaited slot synchronously,
while a miss spawns work (`/Users/amos/oss/facet/vix/src/machine/driver.rs:15-22`,
`/Users/amos/oss/facet/vix/src/machine/driver.rs:1673-1723`,
`/Users/amos/oss/facet/vix/src/machine/driver.rs:1874-1912`). `memo_key` hashes
each argument by canonical content (`/Users/amos/oss/facet/vix/src/machine/driver.rs:3170-3178`).
Projection reuse is also semantic state: candidate keys are coarser, and a
candidate only hits if the recorded read set verifies against the new arguments
(`/Users/amos/oss/facet/vix/src/machine/driver.rs:3180-3259`).

Current export/import proves that vix values can cross a boundary as facet data,
but it serializes the whole current store bundle and reimports by handle order;
it is not a persistent store backend (`/Users/amos/oss/facet/vix/src/machine/driver.rs:1415-1460`).

Fetch is not yet the thesis. It lowers to `FETCH_HOST`, not an INVOKE of a pure
`fetch` function (`/Users/amos/oss/facet/vix/src/machine/lower.rs:4627-4665`).
The driver records a separate `journal` pin keyed by
`fetch:{url}:sha256:{sha}` or `fetch:{url}:observed`, then calls the configured
`FetchBackend` (`/Users/amos/oss/facet/vix/src/machine/driver.rs:3920-3955`).
The fake backend verifies SHA-256 and returns a fake tree
(`/Users/amos/oss/facet/vix/src/fetch.rs:7-15`,
`/Users/amos/oss/facet/vix/src/fetch.rs:58-75`). The existing tests prove the
journal replay behavior, not store-backed memo persistence
(`/Users/amos/oss/facet/vix/src/machine/lower.rs:6973-7015`).

Exec is also split today. The open exec model owns the two-tier semantics and
verification (`/Users/amos/oss/facet/vix/src/exec.rs:1-32`), while the runtime
answers "what happened when I ran this" and moves bytes. `ExecCache` is in-memory
tier-1 plus tier-2 candidates (`/Users/amos/oss/facet/vix/src/exec.rs:575-678`).
The open/proprietary division says this is the intended line: open vix owns
language, identity, memo keying, read-set verification, and protocol; Vixen owns
warm caches, fleets, multi-tenant substrate, and the production daemon
(`docs/design/open-proprietary-division.md:7-16`,
`docs/design/open-proprietary-division.md:27-55`).

## The design: one persistent memo substrate

The open side should define a persistence trait in vix terms. The proprietary
side should implement it with `vx-store` plus a claim/index service.

Open vix should not depend on `vx-services`. The boundary should speak only vix
semantic identities:

```rust
pub trait PersistentValueStore {
    fn get_value(&self, schema: &str, hash: [u8; 32]) -> Result<Option<StoredValue>, String>;
    fn put_value(&self, value: &StoredValue) -> Result<(), String>;

    fn lookup_memo(&self, key: &PersistentMemoKey) -> Result<Option<PersistentMemoEntry>, String>;
    fn put_memo(&self, key: &PersistentMemoKey, entry: &PersistentMemoEntry)
        -> Result<(), String>;

    fn lookup_projection_candidates(
        &self,
        key: &PersistentProjectionCandidateKey,
    ) -> Result<Vec<PersistentMemoKey>, String>;
    fn put_projection_candidate(
        &self,
        candidate_key: &PersistentProjectionCandidateKey,
        memo_key: &PersistentMemoKey,
    ) -> Result<(), String>;
}
```

The real API needs names and serialization types, but this is the shape:

- values are addressed by `(schema, SHA-256 content_hash)`, because that is what
  `ValueStore` already dedupes on;
- memo claims are addressed by the canonical memo key;
- projection candidates are persisted separately because lookup is not just
  exact key lookup;
- read sets remain open semantic data and are verified by open vix before a
  projection or tier-2 hit is accepted.

On a value allocation, vix still computes the SHA-256 content hash. The backing
store is consulted before allocating a fresh RAM handle. If present, the value is
imported into the current process-local `ValueStore` and gets a local handle. If
absent, vix allocates locally and publishes the durable value object. The local
handle remains ephemeral; the durable identity is `(schema, hash)`.

On an INVOKE, vix computes the same `CanonMemoKey` it already computes. Lookup
order becomes:

1. process-local `memo`;
2. persistent exact memo claim;
3. process-local projection candidates;
4. persistent projection candidates, each verified by open vix read-set logic;
5. spawn and compute.

When an invocation completes, vix writes the result value object, then writes the
memo claim. The claim should point at a value identity, not at the current
session's handle. Loading a claim imports the value by identity and remaps it to
a session-local handle.

That is the "memoization = caching" mechanism. There is no registry cache in the
fetch backend. There is a pure invocation whose result is a vix Tree value; the
result enters `ValueStore`; the memo claim maps `(fetch closure, url, checksum)`
to that Tree value; future demand hits the memo and never calls the backend.

## Making fetch fit the thesis

The current `FETCH_HOST` primitive and `journal` are a useful proof of checksum
pinning, but they are a separate cache path. To make the thesis true:

- Lower `fetch(url, sha256)` as a vix invocation of a known pure intrinsic, or
  route primitive fetch through the same persistent memo path with an explicit
  intrinsic closure identity.
- The result is a Tree value. The tree's bytes live in the store, and the memo
  claim maps the fetch invocation key to that Tree.
- For declared checksums, the memo key includes URL and checksum. For observed
  checksums, the first run creates an observation claim that freezes the observed
  checksum; subsequent runs should use the pinned checksum as part of the stable
  key, not a bespoke fetch cache.
- The backend still exists, but only for acquisition: HTTP, sparse index, crate
  archive download, checksum verification, unpacking into a tree. It does not
  own caching.

`rodin-survey` already has reusable sparse-index mechanics: crates.io index path
calculation, `curl` with a user-agent, Cargo sparse-cache decoding, committed
fixture fallback, and facet-json parsing of index rows
(`crates/rodin-survey/src/lib.rs:11-15`,
`crates/rodin-survey/src/lib.rs:108-130`,
`crates/rodin-survey/src/lib.rs:142-214`). That code is a good source for the
real sparse backend. Its current `target/rodin-survey-cache` and Cargo cache
fallback are research scaffolding, not the production caching model.

## Build outputs and exec claims

Build outputs are the same story plus the tier-2 verification rule.

For tier 1, the persistent key is the existing exact identity: closure or
primitive identity, normalized plan, capability fingerprint, and coarse mounted
tree identities. The output Tree is a store value. If the exact key hits, vix can
import the Tree value and return it without running.

For tier 2, persistence must not bypass verification. A candidate record can be
durable, but it carries the observed read set. On lookup, open vix reruns the
existing `verify()` logic against the current world before accepting the output
(`/Users/amos/oss/facet/vix/src/exec.rs:21-25`,
`/Users/amos/oss/facet/vix/src/exec.rs:491-573`,
`/Users/amos/oss/facet/vix/src/exec.rs:650-660`). Persistence changes residency;
it must not change the proof obligation.

The Vixen substrate already has the right product-side shape: CAS-backed VFS
serving sits in front of `vx-store`, and platform VFS bridges materialize bytes
on read (`docs/design/vix-substrate-vfs-sandbox.md:21-79`). Fleet execution also
already treats trees as movable content and records where tree fingerprints live
(`/Users/amos/oss/facet/vix-wire/src/lib.rs:694-760`,
`/Users/amos/oss/facet/vix-wire/src/lib.rs:928-960`). The persistent memo store
would turn those tree movements into ordinary object residency under the same
store.

## How vx-store should back the trait

The product implementation needs two layers:

1. CAS bytes/nodes: actual value bytes, tree chunks, directory nodes, archives,
   and build outputs.
2. Claim/index layer: memo key to result value, projection candidates, exec
   tier-2 candidates, fetch observation pins, provenance, tenant policy.

The store architecture already says bytes are self-verifying but claims need
policy (`docs/design/store-architecture.md:8-20`). Therefore fetched bytes and
trusted build outputs may share the same physical CAS if their hashes verify,
but their claims should live in separate namespaces with separate policy:

- `vix.value.v1`: immutable vix value objects by `(schema, sha256)`;
- `vix.memo.v1`: exact invocation memo claims;
- `vix.memo.projection.v1`: candidate lists plus projection read sets;
- `vix.fetch.observation.v1`: observed checksum pins;
- `vix.exec.tier1.v1`: exact exec output claims;
- `vix.exec.tier2.v1`: exec candidate claims with read witnesses;
- tenant-qualified versions of the above for hosted/self-hosted policy.

If `vx-store` grows algorithm-typed addresses, `vix.value.v1` can be a real
SHA-256-addressed object family. If it remains BLAKE3-only, then the claim layer
must map vix SHA-256 identities to BLAKE3 `NodeHash`es. That is acceptable as a
transition, but it is not the thesis in its strongest form.

GC must be claim-rooted. The current store has no GC surface. A future collector
needs to trace from tenant roots, memo/index claims, leases for live runs,
published artifact roots, and policy-retained provenance. It must not infer
liveness from bare object presence, because CAS objects are claimless bytes.

## Hard questions for Amos

1. Digest compatibility: should `vx-store` grow algorithm-typed object addresses
   so vix SHA-256 values are first-class store objects, or should vix move to
   BLAKE3 for its semantic content hashes? The current state is incompatible.
   I recommend preserving vix's SHA-256 semantic identity and making the store
   multi-algorithm, because fetch checksums, Cargo crate checksums, and the
   newly landed vix incremental SHA-256 hashing all point that way.
2. Claim trust: fetched bytes are checksum-verified and can share CAS flesh, but
   memo claims and build-output claims require tenant/provenance policy. Should
   public fetch claims be accepted only as "bytes available, recompute or verify
   claim locally", while own-tenant claims are trusted outright?
3. Persistence scope: exact pure memo entries can persist directly. Projection
   and exec tier-2 entries can persist only as candidates with read witnesses
   that open vix verifies on every use. Live waiters, pending runs, trace clocks,
   AST parse memo tables, positive presence caches, and backend connection state
   should stay ephemeral.
4. Fetch shape: should `fetch` be reified as a real intrinsic vix function with
   a stable closure hash, or should primitive host calls get a general
   "memoized intrinsic" identity? The former is cleaner for the thesis; the
   latter may fit the current lowering with fewer moving parts.
5. Tree identity: vix Trees currently have SHA-256 semantic hashes; `vx-store`
   Merkle directories are BLAKE3 `NodeHash`es. If build outputs are to dedupe
   across vix values and VFS trees by identity, this needs the same answer as
   question 1, not an ad hoc tree adapter.
6. GC and leases: what are the first durable roots? Likely tenant memo indexes,
   published artifact roots, local worktree roots, and active-run leases. The
   current `vx-store` crate has no GC, so this is product architecture work, not
   a vix machine change.
