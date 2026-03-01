# Roam v7 Migration Notes (Vixen)

This document captures the concrete API changes that break v6-style code in vixen.

## 1) Service trait impls: `Trait` -> `TraitServer`

`#[roam::service]` no longer exposes the original service trait for server impls.

- Old style:
  - `impl Vfs for MyService { async fn lookup(&self, _cx: &roam::Context, ...) -> LookupResult { ... } }`
- New style:
  - `impl VfsServer for MyService { async fn lookup(&self, call: impl roam::Call<LookupResult, Infallible>, ...) { call.ok(value).await; } }`

Key changes:
- No `roam::Context` parameter.
- Methods receive a `call` object.
- Handlers respond via `call.ok(...)` / `call.err(...)`.

## 2) `roam::Context` is gone

v7 server methods do not receive a context object. Any direct use of `&roam::Context` must be removed as part of moving to `*Server` signatures.

## 3) `roam_stream` reconnection/handshake helpers removed

Symbols no longer available:
- `roam_stream::ConnectionHandle`
- `roam_stream::NoDispatcher`
- `roam_stream::HandshakeConfig`
- `roam_stream::accept`

Use v7 stack:
- `roam_core::{initiator, acceptor, Driver, BareConduit}`
- `roam_stream::StreamLink` for byte-stream links
- In vixen, prefer existing wrappers in `vx-io`:
  - `vx_io::net::connect_session(...)`
  - `vx_io::boot::boot_and_connect(...)`
  - `vx_io::spawn::ServiceBuilder`

## 4) `ServiceBuilder` API in `vx-io` changed

- Old: `ServiceBuilder::new(name, endpoint, NoDispatcher)`
- New: `ServiceBuilder::new(name, endpoint)`

- Old: `handle.boot_and_connect(None)`
- New: `handle.boot_and_connect()`

## 5) Generated protocol traits affected in vixen

The following server impl targets need migration to `*Server` form:
- `vixenfs-proto`: `VfsServer`
- `vx-runner-proto`: `RunnerServer`
- `vxd-proto`: `VxdServer`, `ProgressListenerServer`
- `vx-vfsd/src/protocol.rs`: `VfsdServer`
- `vx-store-proto`: `StoreServer`

## 6) Practical migration pattern

For each service implementation:
1. Change `impl Xxx for ...` to `impl XxxServer for ...`.
2. Replace each method signature with `(call: impl roam::Call<Ret, Err>, ...args)`.
3. Move returned value into `call.ok(ret).await`.
4. For `Result<T, E>` methods, use:
   - `Ok(v) => call.ok(v).await`
   - `Err(e) => call.err(e).await`

## 7) Current external blocker

`facet-styx` can fail depending on git revision compatibility with the current `facet` API.
This is independent from Roam migration and may need either:
- pinning a compatible styx revision, or
- upstream fixes in styx.
