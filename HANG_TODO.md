# Hang / Deadlock TODO

Prioritized list of suspected or confirmed hang/deadlock/stall risks.

## P0

- [x] Fix lock-order inversion between `prefixes` and `items` in `vx-vfsd` to prevent deadlocks.
  - `crates/vx-vfsd/src/service.rs` locks `prefixes -> items`
  - `crates/vx-vfsd/src/vfs.rs` has paths that lock `items -> prefixes`

- [x] Make progress reporting non-blocking in acquisition hot paths.
  - Risk: `progress.send(...).await` can stall acquisition if receiver is slow/dropped/backpressured.
  - High-risk callsite: `tokio::select!` loop in `download_and_extract_component`.
  - Candidate fixes: `try_send`, bounded timeout around `send`, or decoupled progress task.

- [x] Fail toolchain ensure paths on chunk/node write failure instead of warn-and-continue.
  - Current behavior can publish manifests that are not fully materializable.

## P1

- [x] Ensure RPC returns canonical published manifest hash (toolchain + registry).
  - Concurrent writers plus non-deterministic `created_at` can return a hash different from mapped spec key.
  - Option A: return mapping winner hash from `put_*_spec`.
  - Option B: remove/normalize non-deterministic fields from hashed manifest bytes.

- [x] Make toolchain spec publish truly create-if-absent (atomic), matching intended semantics.
  - Current `try_exists + atomic_write` pattern is race-prone.

- [x] Add closure validation before publish/finalize.
  - Verify all referenced directory/blob/chunk nodes exist before mapping spec/cache keys.

- [x] Handle `put_registry_spec` "already exists" case explicitly in result.
  - Avoid returning a non-canonical manifest hash when another writer won.

## P2

- [x] Revisit `OnceCell` failure caching policy for toolchain/registry ensure.
  - Transient first failure for a key can stay sticky for subsequent callers.

- [x] Bound growth of inflight ensure maps.
  - Inflight entries are currently never removed; can increase memory use and mutex contention over time.

- [x] Replace panic-prone `unwrap()`s in acquisition/publish paths with explicit error returns.
  - Panics under load can leave partially written CAS state and amplify inconsistency risks.

- [x] Add cycle protection for recursive DirectoryNode walks in mount/materialization paths.
  - Detect already-visited `NodeHash` during recursive traversal.

- [x] Verify and document `roam::Tx` semantics (bounded/unbounded/lossy) and backpressure behavior.
  - Needed to confirm severity of progress-channel stall risk and choose safe defaults.
  - Verified against `roam-session` source (git rev `dcea18e`):
    - `roam::channel()` uses a bounded channel (`CHANNEL_SIZE = 1024` in `roam-session/src/lib.rs`).
    - `Tx::send(...).await` awaits queue capacity / receiver readiness and can backpressure.
    - `Tx::send` returns `TxError::Closed` when receiver side is gone; no lossy drop behavior by default.
    - `try_send` exists at the runtime sender layer and is used in limited paths (e.g. `Tx` drop close), not as default `Tx::send` semantics.
