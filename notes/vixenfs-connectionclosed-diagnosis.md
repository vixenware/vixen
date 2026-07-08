# VixenFS "backend not connected" — stale-appex-instance post-mortem

Status: symptom cleared on Amos's machine via the CLI repair path (kill stale
`VixenFS.appex` instances, remount). Extension pid is now fresh (35776),
replacing the stale pair (68406, 68790). This note answers *why* a stale
`VixenFS.appex` holding `vixenfs.sock` produced the observed
`connect failed: connectionClosed` retry loop, and pins down the repair verb.

## The mechanism

The socket is bound server-side by the extension, in
`crates/vx-fskit/src/macos/root_peer.rs`:

- `RootPeerState::ensure_listening()` (root_peer.rs:79-84) is guarded by a
  **per-process** `AtomicBool` (`listener_started`, root_peer.rs:29). It only
  runs once *per process*, and is only called from the real mount path —
  `loadResource`/`didFinishLoading` in `filesystem.rs:57-78` and
  `filesystem.rs:98-103` — never from `probeResource` (comment at
  `filesystem.rs:32-37` explains why: probe instances binding the socket would
  race the real mount instance).
- `run_listener()` (root_peer.rs:93-128) does `remove_endpoint` (an `unlink(2)`
  of the socket path, root_peer.rs:95) followed by
  `LocalLinkAcceptor::bind(&socket_path)` (root_peer.rs:100). This is the plain
  `bind`, **not** `bind_with_lock` — vox-stream ships both
  (`vox-stream/src/lib.rs:760` vs `:777`), and only `bind_with_lock` takes an
  exclusive `flock` on `{addr}.lock` before touching the path
  (`vox-stream/src/lib.rs:716-745`). vixenfs uses neither `try_local_lock` nor
  `bind_with_lock` — there is **no cross-process exclusivity check** on who
  owns `vixenfs.sock`. Whichever process's `run_listener()` runs *last* wins
  the path outright (unlink-then-bind is inherently a last-writer-wins race).
- `unloadResource` (`filesystem.rs:80-94`) does **not** stop the listener or
  exit the process — it only flips `FSContainerStatus` to `notReady` and
  cancels in-flight ops (`root_peer_state().cancel_inflight(...)`,
  filesystem.rs:88). So once a `VixenFS.appex` process has bound the socket,
  it keeps its listener alive for the rest of that process's lifetime even
  after FSKit tears down its volume — exactly the "orphaned but still
  listening" shape Amos observed as a second live pid.

So: FSKit/ExtensionKit is documented (`apps/mac/FSKIT-REPAIR.md`, "What We
Learned The Hard Way") to leave more than one `VixenFS` process running as a
normal side effect of its probe/load lifecycle, not as a sign the app
double-launched anything. If an **old** instance (spawned by, and still
running the compiled code of, a *previous* build/launch) is the one that most
recently called `ensure_listening()`, it — not the fresh instance backing the
currently-mounted volume — owns `vixenfs.sock`, and every new connection from
the app's `VfsBackendService` (`apps/mac/xcode/Vixen/Backend/VfsBackend.swift`)
lands on the stale process instead.

## Why that produces accept-then-close, not ECONNREFUSED

vox's connection setup runs a handshake immediately after `accept()`, before
any service/RPC dispatch — `RootPeerState::accept_link` calls
`acceptor_on(link).on_lane(...).establish_connection()`
(root_peer.rs:130-147) and only logs "session established" *after* that
resolves. Upstream vox's handshake
(`vox-core/src/handshake.rs`) has an explicit incompatible-schema rejection
path exercised by
`acceptor_rejects_incompatible_peer_message_schema_with_sorry`
(handshake.rs:701) — a "Sorry" decline sent back to the initiator at connect
time, before any Vfs call, which surfaces on the Swift side as exactly the
observed `connect failed: connectionClosed` (accept, then immediate close).
A stale appex process is running whatever vox/service schema was compiled
into *its* binary at the time it was spawned; if it predates the app's
current build (or predates a hot-relaunch of the app during dev iteration),
its embedded schema for the `Vfs` service can disagree with the current
`VfsBackend` client's, and the handshake declines every single time — a tight
~2s retry loop of connect → accept → schema check → close, never reaching a
single `Vfs` RPC. This matches the reported symptom precisely and does not
require the two `vox` crate git-pins (Rust `cb6825a` vs. Swift-side
`f4f0f90`, 920 commits apart in `facet-rs/facet`) to be the cause — that pin
gap is real drift risk worth closing separately, but the reproducible,
source-grounded mechanism for *this* incident is the stale-listener race
above, and it's sufficient on its own to explain the symptom.

## Repair verb (playbook entry)

When you see "backend not connected" **and** more than one `VixenFS` process:

1. Do not `killall`/`pkill`/`ps`-loop — FSKIT-REPAIR.md already warns these
   can hang while FSKit is wedged.
2. Identify the extension pids (`pluginkit`/`log show`, not `pgrep`, per the
   same doc) and `kill(2)` the **stale** ones directly — i.e. the ones that
   are not backing the currently-live `/Volumes/Vixen/vfs` mount. If it's
   ambiguous which is stale, killing all `VixenFS.appex` pids is safe: FSKit
   will respawn exactly one on the next access, and `didFinishLoading` /
   `loadResource` re-run `ensure_listening()` fresh in the new process.
3. Let FSKit/ExtensionKit spawn a fresh instance and let the app's
   `VfsBackendService` reconnect — this is what actually cleared the symptom
   here (new pid 35776, no code change).
4. Only escalate to killing `fskitd` itself (documented last resort in
   FSKIT-REPAIR.md) if step 2 doesn't produce a single live instance holding
   the socket.
5. Do not chase this as a schema/codegen regen problem first — check for a
   duplicate/stale `VixenFS.appex` pid before touching generated bindings or
   the vox git pins. The pin gap is worth closing as separate hygiene, but
   isn't what broke this session.

## Files referenced

- `crates/vx-fskit/src/macos/root_peer.rs:29,79-147` — per-process listener
  guard, unlink+bind race, no exit-on-unload.
- `crates/vx-fskit/src/macos/filesystem.rs:32-37,57-94` — probe vs. load vs.
  unload lifecycle; unload doesn't stop the listener.
- `apps/mac/xcode/Vixen/Backend/VfsBackend.swift` — the client that
  reconnects into whichever process currently holds the socket.
- vox (git `facet-rs/facet`, pinned `cb6825a` for Rust /
  `f4f0f90` for Swift): `vox-stream/src/lib.rs:716-780` (lock vs. non-lock
  bind), `vox-core/src/handshake.rs:701` (schema-mismatch "Sorry" decline).
- `apps/mac/FSKIT-REPAIR.md` — existing playbook; this note is additive detail
  on the "more than one VixenFS process" line item there.
