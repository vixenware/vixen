# Fission FSKit extension: stuck-audit (where OUR extension wedges the mount)

Threat model, corrected scope. The wedge is not "the app touches a wedged
mount." The wedge is **our FSKit extension is the thing that wedges the mount**
— by failing to reply to an FSKit protocol callback the way FSKit requires.
Every FSKit operation is a callback that hands us a `reply`/completion block; the
kernel VFS layer that dispatched it is blocked until we call that block exactly
once. If any code path returns without replying, throws/panics past the reply,
or awaits something unbounded before replying, the originating `read(2)`/
`stat(2)`/`getdirentries(2)` never returns — the calling process enters
uninterruptible (`D`) sleep, and `SIGKILL` cannot reclaim it because the signal
is deferred until the syscall returns, which is never. That is the wedge, and
its *source* is our code.

The extension is Rust (`crates/vx-fskit/src/macos/`) behind thin Swift appex
shims (`apps/mac/xcode/VixenFS/`, 78 lines total). The FSKit protocol
conformances are `define_class!` Objective-C classes in Rust:
`RustUnaryFileSystem` (`filesystem.rs`) and `RustVolume` (`volume.rs` +
`volume/helpers.rs`), with `RustItem` (`item.rs`) and the connection registry
`RootPeerState` (`root_peer.rs`). Backend content is served by a **Vox RPC peer**
(the app's `VfsBackend`, or `vx-vfsd`) that *connects to us*; we call it via
`VfsClient` for every non-synthetic lookup/read/write.

This audit reads from code on `main`. It is: the entry-point × reply-guarantee
matrix, then findings ranked by wedge likelihood, then fix shapes.

> The app-side caller concern (the app core issuing FS ops against a wedged
> mount, isolation architecture, zombie recovery protocol) is **separate future
> work**, not this audit; an earlier draft explored it and is preserved in git
> history at commit `c3db95c3` (`vixen-app-wedge-resilience.md`). This document
> is strictly about our extension's own reply guarantees.

---

## 1. Entry-point × reply-guarantee matrix

Every FSKit callback we implement, whether its reply is reached on **all** paths
(success / error / RPC-failure / panic), and the mechanism by which it can fail
to reply. "Sync-safe" = replies unconditionally with no backend RPC and no
panic source. "WEDGE" = contains an unbounded `block_on_vfs` (see F1) whose
non-return skips the reply.

### `FSUnaryFileSystemOperations` — `filesystem.rs`

| Callback | Replies on all paths? | Notes |
|---|---|---|
| `probeResource` | ✅ sync | `.expect("CONTAINER_UUID valid")` on a constant — safe. |
| `loadResource` | ✅ sync | Starts listener (non-blocking spawn), builds `RustVolume`, replies. Panic risk if `RustVolume::new` unwraps fail (F4). |
| `unloadResource` | ✅ sync | Sets `notReady`, replies. Does **not** drain in-flight ops (F5). |
| `didFinishLoading` | ✅ (void, no reply block) | Starts listener idempotently. |

### `FSVolumeOperations` — `volume.rs`

| Callback | Replies on all paths? | Mechanism / risk |
|---|---|---|
| `supportedVolumeCapabilities` / `volumeStatistics` | ✅ sync (return value) | constants |
| `mountWithOptions` | ✅ sync | null-error reply |
| `unmountWithReplyHandler` | ✅ sync | no in-flight cancellation (F5) |
| `synchronizeWithFlags` | ✅ sync | null-error reply |
| `getAttributes` | ⚠️ **WEDGE** (backend items) | `attributes_for_item` → `VirtualKind::None` → `refresh_backend_item` → **`block_on_vfs(get_attributes)`** (helpers.rs:279). Virtual items (root/meta/status) are sync-safe. |
| `setAttributes` | ⚠️ **WEDGE** | `block_on_vfs(set_attributes)` then `refresh_backend_item` (a second `block_on_vfs`) (volume.rs:212, 221) |
| `lookupItemNamed` | ⚠️ **WEDGE** + panic | backend path `block_on_vfs(lookup)` (helpers.rs:369); `.expect("directory should be cached")` (321), `.expect(".vixenfs dir should be cached")` (335) — F4 |
| `reclaimItem` | ✅ sync | takes `items`/`routed` locks (F3 poison) |
| `readSymbolicLink` | ✅ sync | ENOTSUP |
| `createItemNamed` | ⚠️ **WEDGE** | `block_on_vfs(create)` + `block_on_vfs(get_attributes)` (volume.rs:386, 400) |
| `createSymbolicLink` / `createLinkToItem` | ✅ sync | ENOTSUP |
| `removeItem` | ⚠️ **WEDGE** | `block_on_vfs(delete)` (volume.rs:544) |
| `renameItem` | ⚠️ **WEDGE** | `block_on_vfs(rename)` (volume.rs:646) |
| `enumerateDirectory` | ⚠️ **WEDGE** + panic | backend path `block_on_vfs(read_dir)` (helpers.rs:527); `.expect(".vixenfs dir should be cached")` (473) — F4. Root/`.vixenfs` enumeration is sync-safe. |
| `activateWithOptions` | ✅ sync | `get_cached_item(ROOT)` → replies Some/None both |
| `deactivateWithOptions` | ✅ sync | no in-flight cancellation (F5) |

### `FSVolumePathConfOperations` — all ✅ sync scalar returns (no reply block).

### `FSVolumeReadWriteOperations` — `volume.rs`

| Callback | Replies on all paths? | Mechanism / risk |
|---|---|---|
| `readFromFile` | ⚠️ **WEDGE** (backend files) | `read_item` → backend path **`block_on_vfs(read)`** (helpers.rs:645). **Hottest path** — every file byte read. Virtual `status.json` read is sync-safe. |
| `writeContents` | ⚠️ **WEDGE** | `block_on_vfs(write)` (volume.rs:855) |

### `FSVolumeXattrOperations` — all ✅ sync (ENOATTR / ENOTSUP / empty list).

### `FSVolumeOpenCloseOperations` — `openItem`/`closeItem`/`isOpenCloseInhibited` all ✅ sync (status-snapshot bookkeeping under the per-item lock).

**Matrix summary: 9 WEDGE-class callbacks** — `getAttributes`, `setAttributes`,
`lookup`, `create`, `remove`, `rename`, `enumerate`, `read`, `write` — every one
via an unbounded `block_on_vfs`. Every other callback replies synchronously and
unconditionally. Crucially, **branch coverage is clean**: I traced every
`match`/`if let` arm and early return in all 9 WEDGE handlers and every sync
handler — each calls `reply.call(...)` exactly once on every branch. There is no
"forgot to reply on the error arm" bug. The *only* two ways a reply is skipped
are (a) `block_on_vfs` never returning, and (b) a panic before the reply. The
findings below are those two mechanisms.

---

## 2. Findings, ranked by wedge likelihood

### F1 — CRITICAL: `block_on_vfs` has no deadline; an alive-but-silent provider wedges any of the 9 hot callbacks permanently

`support.rs:56-63`:

```rust
pub(super) fn block_on_vfs<F, T>(future: F) -> Result<T, Retained<NSError>>
where F: Future<Output = Result<T, i32>> {
    runtime().handle().block_on(async move { future.await.map_err(posix_error) })
}
```

The `future` is a `VfsClient` RPC (`client.read(...).await`, `.lookup(...)`,
etc.). **There is no timeout** — not here, and not in Vox (`VfsClient` methods
are plain `async fn` with no per-call deadline surface; grep of `vx-fskit` for
`timeout|deadline|Duration` is empty). If the provider is *alive but does not
reply* — a deadlocked or infinite-looping handler in `vx-vfsd`/`VfsBackend`, a
handler blocked on a slow/hung CAS or network fetch, or Vox channel backpressure
that never drains — then `future.await` never completes, `block_on` never
returns, and the FSKit callback **never calls `reply`**. The originating
`read(2)`/`stat(2)` hangs forever; the process wedges in `D` state.

Stuck scenario (concrete): a guest build issues `read()` on a large file whose
bytes require a CAS chunk the provider is fetching from a stalled upstream; the
provider's `read` handler awaits that fetch indefinitely; our `readFromFile` is
parked in `block_on_vfs`; the guest's `read(2)` never returns; `kill -9` on the
guest does nothing. This is the wedge, on the single most frequent callback.

**What is already handled:** a *clean* provider disconnect is fine — the Vox
await errors out (connection closed), maps to `EIO`, and the reply fires. The
registry even watches for it (`root_peer.rs:344-347` `client.caller.closed()` →
`record_closed`), and stale generations map to `ESTALE`/`ENOTCONN`
(`helpers.rs:195-217`). The gap is strictly the **alive-but-silent** provider,
which no current mechanism bounds.

**Fix shape:** wrap the awaited RPC in a bounded `tokio::time::timeout` inside
`block_on_vfs`; on elapse, return `Err(EIO)` (or `ETIMEDOUT`) so the callback
*always* completes. A flat wall-clock bound converts an infinite wedge into a
bounded error even if it occasionally error-replies a legitimately slow read
(the calibration tension the substrate doc §4 names). Better: a deadline that
resets on RPC-level progress (bytes streamed / heartbeat) so honest slow
transfers survive while a truly silent provider still trips. Best: fold this
into F2 (the timeout lives in the spawned task).

### F2 — HIGH: synchronous `block_on` on the FSKit-provided thread → dispatch-thread exhaustion; one hung provider stalls *all* prefixes

`block_on_vfs` blocks the **calling FSKit thread** for the entire RPC
(`support.rs:60`). FSKit dispatches volume operations on a bounded thread/queue
supply; each in-flight slow-or-hung WEDGE-class op parks one of those threads.
Once they are all parked (F1 makes "parked forever" reachable), **even
backend-free callbacks queue behind them** — root `getAttributes`, `.vixenfs`
enumeration, `status.json` read: precisely the synthetic paths that are
otherwise sync-safe and that health probes rely on to distinguish "volume alive"
from "volume wedged." So a single hung provider does not just wedge its own
prefix's reads; it can freeze liveness observability for the whole mount, across
every prefix, because they share the FSKit dispatch pool this design blocks.

Stuck scenario: provider A (prefix `app`) hangs on one file; enough concurrent
readers hit it to saturate FSKit's dispatch threads; a read of prefix `build`
(served by a healthy provider B) now also hangs, and so does `cat
/Volumes/Vixen/vfs/.vixenfs/status.json`, so the mount looks globally dead even
though only A is sick.

**Fix shape:** stop blocking the FSKit thread — adopt FSKit's asynchronous reply
idiom. Copy the reply block to the heap (`RcBlock`, made `'static + Send`),
`runtime().spawn(...)` the RPC, and call `reply.call(...)` from the task's
completion, with F1's `timeout` *inside* the task. The FSKit thread returns
immediately; concurrency is bounded by the tokio runtime (already multi-threaded,
`support.rs:46-54`), not by FSKit's thread supply; and a hung provider parks a
cheap task, not a scarce FSKit thread. This one change subsumes F1 (timeout in
the task) and makes F5 natural (the task holds an abort handle).

### F3 — HIGH: poison-propagating `.expect()` on every shared lock turns one panic into a whole-volume wedge

The connection registry is a single `Arc<Mutex<RootPeerSnapshot>>` locked on
every routing call — `client_for_generation`, `client_for_prefix`,
`live_prefixes`, `live_generations`, `generation`, `status_json_bytes`
(`root_peer.rs:195-291`), each via `.expect("root peer snapshot mutex
poisoned")`. Same pattern for `items`/`routed` (`helpers.rs:45-99`) and the
per-item `state` mutex (`item.rs`, ~14 sites). The locks are correctly **not**
held across any `block_on_vfs` (I verified: every RPC-path helper locks, reads,
and drops before/after the RPC, never around it), so they do not themselves
wedge — good.

The hazard is poison propagation. If **any** thread panics while holding one of
these locks, the `Mutex` is poisoned and **every subsequent `.expect()` panics**
— so one panic in one callback cascades into all future callbacks on that lock
failing to reply. And these are Objective-C methods (`define_class!`): a panic
unwinding into FSKit's ObjC frame is undefined behavior (in practice `abort`),
which drops every in-flight reply at once. A live panic source sits right under
the registry lock: `status_json_bytes` does
`facet_json::to_string(&payload).expect("serialization should not fail")`
(`root_peer.rs:283-284`) *while holding the snapshot lock* — a serialization
failure there poisons the registry for the entire mount lifetime.

Stuck scenario: `status_json_bytes` hits an unexpected serialization error under
the lock → panic → registry poisoned → the next `lookup`/`read` on any prefix
calls `client_for_generation` → `.expect(poisoned)` → panic → abort → all
in-flight VFS ops on the mount wedge until fskitd notices the extension died.

**Fix shape:** (a) wrap every FSKit entry point in a `catch_unwind` reply-guard
that, on a caught panic, calls `reply` with `EIO` — so a bug can never skip the
reply and never unwind across the ObjC boundary; (b) replace poison-propagating
`.expect()` with `lock().unwrap_or_else(PoisonError::into_inner)` so one panic
does not cascade; (c) remove the panic source under the lock (serialize
*outside* the critical section, or handle the error).

### F4 — MEDIUM: invariant `.expect()`/`.unwrap()` inside handlers = skipped reply + ObjC-boundary abort

Independently of poison, several handlers panic on "can't happen" cache
invariants: `lookup_item` `.expect("directory should be cached")`
(`helpers.rs:321`) and `.expect(".vixenfs dir should be cached")` (335);
`enumerate_directory` the same at `helpers.rs:473`; `RustVolume::new`
`Retained::retain(...).unwrap()` (`helpers.rs:26`); `root_peer_state`
`.expect("not initialized")` (`root_peer.rs:368`). Each is reachable under a
cache/reclaim race (e.g. `reclaimItem` removed an item that a concurrent
`lookup` still expects, or a future refactor). In an ObjC callback a panic
skips the reply and unwinds into FSKit → abort → all in-flight replies lost.

**Fix shape:** convert each to a graceful error reply (`ENOENT`/`EIO`) instead of
a panic, backed by the F3a `catch_unwind` reply-guard as defense-in-depth. The
`.vixenfs`/root items are seeded at volume construction (`helpers.rs:16-19`) and
protected from reclaim (`helpers.rs:60-62`), so today the invariant holds — this
is hardening against races and future edits, hence MEDIUM.

### F5 — MEDIUM: in-flight WEDGE ops are neither cancelled nor error-replied on teardown or provider loss

`unmount`, `deactivate`, and `unloadResource` reply immediately
(`volume.rs:97-103, 727-735`; `filesystem.rs:80-93`) without draining or
cancelling threads currently parked in `block_on_vfs`. The tokio runtime is a
`static OnceLock` never shut down (`support.rs:29-54`), so a parked `block_on`
survives teardown. If a provider is *hung* (F1) when FSKit tears the volume
down, those threads stay parked forever, and the volume can neither cleanly
deactivate nor be safely reused at the same generation. (A provider *clean*
disconnect mid-op is fine — the specific await errors out.)

**Fix shape:** a per-generation cancellation token inside the extension. On
provider disconnect (`record_closed`, already the hook) or on
`deactivate`/`unmount`, fire the token; every spawned RPC task (F2) selects
against it and, when it fires, error-replies its callback (`ESTALE`/`EIO`)
promptly. This is the "watchdog that error-replies stragglers" — it guarantees
teardown drains, not abandons, in-flight callbacks.

### F6 — observation (positive): the reply-skip surface is exactly two mechanisms

Stated so fixes stay focused: there is no missing-`reply` branch anywhere. The
listener/accept path correctly detects clean provider death and routes it to
error replies (`ENOTCONN`/`ESTALE`). The probe-vs-load listener split
(`filesystem.rs:32-37`, `root_peer.rs:66-82`) is a deliberate correctness fix,
not a wedge risk. The synthetic paths (root, `.vixenfs`, `status.json`) never
touch the backend and are individually reply-safe. Close F1/F2 (never-return)
and F3/F4 (panic-before-reply) and the extension's reply guarantee is total.

---

## 3. Fix priority

1. **F2 + F1 together — defer-reply with an in-task timeout.** The single
   highest-value change: heap-copy the reply block, spawn the RPC on the tokio
   runtime, `timeout` it, and reply from the task. This closes the never-return
   wedge (timeout → error reply), the dispatch-thread exhaustion (no `block_on`
   on the FSKit thread), and gives F5 its cancellation seam. Every one of the 9
   WEDGE callbacks routes through `block_on_vfs`, so the change is localized to
   that one function plus the reply-block plumbing at each call site.
2. **F3 + F4 — panic isolation.** `catch_unwind` reply-guard at every entry
   point, non-poisoning lock acquisition, and graceful error replies for the
   invariant `.expect()`s. Closes the panic-before-reply surface and stops the
   ObjC-boundary abort.
3. **F5 — per-generation cancellation** on top of F2's task model, so teardown
   and provider loss drain in-flight callbacks instead of abandoning them.

Together, F1–F5 make the extension's contract with FSKit total: **every callback
replies exactly once, within a bounded time, on every path — success, backend
error, RPC timeout, provider death, and internal panic.** That is the property
that means *our* extension can no longer be the thing that wedges the mount.
