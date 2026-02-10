# VixenFS TODO

## Concurrency Bugs (fix now)

- [x] **Volume.items data race** — `items: [UInt64: Item]` is unprotected mutable state on a
  non-actor class, accessed from multiple async FSKit callbacks concurrently. Protect with
  `OSAllocatedUnfairLock` (can't make Volume an actor — it subclasses FSVolume).

- [x] **Item mutable state race** — `Item` has `private(set) var cachedAttributes` mutated via
  `updateAttributes()`, but Item instances are shared between Volume's cache and FSKit. Concurrent
  reads of `cachedAttributes` (from `attributes()`) and writes (from `lookupItem`) race.
  Protect with a lock.

## Correctness (address soon)

- [x] **setAttributes is a silent no-op** — Returns current attributes without doing anything.
  Callers think `chmod` succeeded. Should either call the RPC or return ENOTSUP.

- [ ] **enumerateDirectory cookie semantics** — `nextCookie` is a local 1-based counter but the
  server's cursor is opaque. If FSKit re-enumerates with a non-zero cookie, the local counter
  restarts at 1 while the server gets the cookie as cursor. Needs investigation into FSKit's
  pagination contract.

- [ ] **Stale cached attributes** — `attributes()` returns cached values with no revalidation.
  If a file changes server-side, cached attrs are permanently stale until reclaim+re-lookup.

## Code Cleanup

- [x] **Remove all preload code** — Preloading was a bad idea. Remove `FileContentCache`,
  `preloadEnabled`, the `?preload=1` URL flag extraction, and simplify the read path to
  always do direct per-read RPCs.

- [x] **Remove vfsCall, use roam built-in timeouts** — Roam now has call-level timeouts
  (default 30s). Removed the manual task-group timeout wrapper entirely.

- [x] **Unused Logger extensions** — `Logger.d()`, `Logger.e()`, `Logger.posixError()` are
  defined in VixenFS.swift but never called.

- [x] **Hardcoded volumeStatistics** — Returns fictional block counts. Should at minimum
  document this or query the backend.

- [ ] **probeResource generates random containerUUID** — Each probe creates a new UUID. If FSKit
  probes the same resource twice it gets different container IDs.

- [ ] **ReadTelemetry is a global singleton** — If two volumes exist, stats interleave and
  reset() wipes the other volume's data. Should be per-volume.

- [ ] **lookupItem makes two sequential RPCs** — Could be one if the protocol returned full
  attributes in lookup. Requires protocol change (roam codegen).

- [ ] **Server-side dead code in VfsClient.swift** — ~300 lines of VfsHandler/dispatcher/
  serializers ship in the extension. Needs client-only codegen mode. (codegen issue, not
  hand-editable)
