# moire + roam v7 Migration Notes

## Context

This document captures findings from the ongoing migration of vixen from roam v6 + old moire to
roam v7 + new moire. The migration was started in a previous session.

## What Was Already Done

- `crates/vx-io/src/boot.rs` — fully migrated to roam v7
- `crates/vx-io/src/net.rs` — fully migrated to roam v7 (added `connect_session`)
- `crates/vx-io/src/spawn.rs` — partially migrated (removed `ServiceDispatcher` generic, now uses
  `DriverCaller + SessionHandle`)
- `crates/vixenfs-codegen/src/main.rs` — `vfs_service_detail()` → `vfs_service_descriptor()`
- `Cargo.toml` (workspace) — added `roam-core`, `roam-types` to workspace deps + patch section

## moire API Changes (New → Old Mapping)

### Removed entirely
- `const moire_CX: moire::moireContext = moire::moireContext::new(env!(...))` — **GONE**, delete
  these lines everywhere
- `moire::init!()` — **GONE**: moire now initializes itself via `#[ctor]` at startup, no call
  needed
- `moire::peep!(future, "name")` — **GONE**: just `future.await` directly (or name it in a
  `spawn`)

### Task spawning
| Old | New |
|-----|-----|
| `moire::spawn_tracked!("name", future)` | `moire::spawn(future)` |
| `moire::spawn_tracked!(format!("name_{}", x), future)` | `moire::spawn(future)` |

The name parameter is gone. If you need named tasks, use the `.named("...")` extension method on
the future:
```rust
moire::task::spawn(async move { ... }.named("my_task"))
```

### Mutex
| Old type | New type |
|----------|----------|
| `moire::Mutex<T>` | `moire::sync::SyncMutex<T>` (sync/parking_lot) |
| `moire::OnceCell<T>` | `moire::sync::OnceCell<T>` |

| Old macro/call | New |
|----------------|-----|
| `moire::mutex!("name", val)` | `moire::sync::SyncMutex::new("name", val)` |
| `mutex.lock_with_cx(moire_CX)` | `mutex.lock()` (sync, returns guard immediately) |
| `moire::once_cell!("name")` | `moire::sync::OnceCell::new("name")` |

Note: `moire::sync::SyncMutex` is backed by `parking_lot::Mutex` (sync, not async).
`moire::sync::Mutex` is async (backed by `tokio::sync::Mutex`, `.lock()` is async).
The old code was using the sync variant everywhere (`.lock()` without `.await`), so use `SyncMutex`.

### Watch channels
| Old | New |
|-----|-----|
| `moire::WatchSender<T>` | `moire::sync::watch::Sender<T>` |
| `moire::WatchReceiver<T>` | `moire::sync::watch::Receiver<T>` |
| `moire::watch!("name", initial_val)` | `moire::sync::watch::channel("name", initial_val)` |
| `tx.send_with_cx(val, moire_CX)` | `tx.send(val)` |
| `rx.changed_with_cx(moire_CX).await` | `rx.changed().await` |
| `rx.borrow()` | `rx.borrow()` (unchanged) |

### Oneshot channels
| Old | New |
|-----|-----|
| `moire::OneshotSender<T>` | `moire::sync::oneshot::Sender<T>` |
| `moire::OneshotReceiver<T>` | `moire::sync::oneshot::Receiver<T>` |
| `moire::oneshot!("name")` | `moire::sync::oneshot::channel("name")` |
| `moire::oneshot_channel::<T>("name", source)` | `moire::sync::oneshot::channel::<T>("name")` |
| `tx.send_with_source(val, source, moire_CX)` | `tx.send(val)` |
| `rx.recv_with_source(source, moire_CX).await` | `rx.await` (Receiver implements IntoFuture) |
| `rx.recv_with_cx(moire_CX).await` | `rx.await` |

### MPSC channels
| Old | New |
|-----|-----|
| `moire::Sender<T>` | `moire::sync::mpsc::Sender<T>` |
| `moire::channel!("name", cap)` | `moire::sync::mpsc::channel("name", cap)` |
| `tx.send_with_cx(val, moire_CX).await` | `tx.send(val).await` |

### Notify
| Old | New |
|-----|-----|
| `moire::Notify` | `moire::sync::Notify` |
| `Notify::new("name", source)` | `moire::sync::Notify::new("name")` |
| `notify.notified_with_cx(moire_CX).await` | `notify.notified().await` |
| `notify.notify_waiters()` | `notify.notify_waiters()` (unchanged) |

### Process
| Old | New |
|-----|-----|
| `moire::Child` | `moire::process::Child` |
| `child.wait_with_cx(moire_CX).await` | `child.wait().await` |
| `child.output_with_cx(moire_CX).await` | `child.output().await` |
| `cmd.spawn_with_cx(moire_CX)` | `cmd.spawn()` |

### Net (just added back)
`moire::net::connect(fut, display, protocol)` and `moire::net::accept(fut, display, protocol)` —
these are wrappers that instrument the futures. Already used correctly in `vx-io/src/net.rs`.

## roam v7 API Changes

### Removed from roam_stream
These items are **gone** from roam_stream v7:
- `roam_stream::accept` — the old accept() function
- `roam_stream::HandshakeConfig` — gone
- `roam_stream::NoDispatcher` — gone
- `roam_stream::ServiceDispatcher` trait — gone
- `roam_stream::ConnectionHandle` — moved to `roam_core::ConnectionHandle`

roam_stream v7 is now **only** a byte-stream framing layer: `StreamLink`, `LocalLink`,
`LocalLinkAcceptor`. Nothing else.

### What `#[roam::service]` generates in v7

For a service `trait Foo { async fn bar(&self, ...) -> T; }`, the macro generates:
- `FooServer` trait — implement this on your handler struct
- `FooClient` struct — wraps a `DriverCaller`, has `.bar(...)` methods
- `FooDispatcher` struct — wraps an `impl FooServer`, implements `Handler<R>` for routing requests

Example from the roam test suite:
```rust
#[roam::service]
trait Adder {
    async fn add(&self, a: i32, b: i32) -> i32;
}

impl AdderServer for MyAdder {
    async fn add(&self, call: impl roam::Call<i32, core::convert::Infallible>, a: i32, b: i32) {
        call.ok(a + b).await;
    }
}

// Server side:
let dispatcher = AdderDispatcher::new(MyAdder);
let mut server_driver = Driver::new(server_handle, dispatcher, Parity::Even);

// Client side:
let mut client_driver = Driver::new(client_handle, (), Parity::Odd);
let caller = client_driver.caller();
let client = AdderClient::new(caller);
let response = client.add(3, 5).await?;
```

### New server-side accept loop pattern

Old (v6):
```rust
let (_handle, mut incoming, driver) = accept(stream, HandshakeConfig { ... }, dispatcher).await?;
let driver_handle = moire::spawn_tracked!("...", async move { driver.run().await });
while let Some(conn) = moire::peep!(incoming.recv(), "...").await {
    let dispatcher = FooDispatcher::new(service.clone());
    conn.accept(vec![], Some(Box::new(dispatcher))).await?;
}
```

New (v7):
```rust
use roam_core::{BareConduit, Driver, session::acceptor};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, MessageFamily, Parity};

let (r, w) = tokio::io::split(stream);
let link = StreamLink::new(r, w);
let conduit = BareConduit::<MessageFamily, _>::new(link);

// Virtual connection acceptor (registered at session build time, called synchronously):
struct MyVirtualAcceptor { boot_service: BootService<MyService> }
impl ConnectionAcceptor for MyVirtualAcceptor {
    fn accept(&self, conn_id: ConnectionId, peer_settings: &ConnectionSettings, metadata: &[...])
        -> Result<AcceptedConnection, Metadata<'static>>
    {
        let Some(service) = self.boot_service.gate().service() else {
            return Err(vec![]); // reject
        };
        let dispatcher = FooDispatcher::new(service.clone());
        Ok(AcceptedConnection {
            settings: ConnectionSettings { parity: Parity::Even, max_concurrent_requests: 64 },
            metadata: vec![],
            setup: Box::new(|handle| {
                let mut driver = Driver::new(handle, dispatcher, Parity::Even);
                moire::spawn(async move { driver.run().await });
            }),
        })
    }
}

let (mut session, root_handle, _session_handle) = acceptor(conduit)
    .on_connection(MyVirtualAcceptor { boot_service: boot_service.clone() })
    .establish()
    .await
    .map_err(|e| eyre::eyre!("session establishment failed: {e}"))?;

// Root connection: BootService handles boot/shutdown RPCs
let boot_dispatcher = BootDispatcher::new(boot_service.clone());
let mut root_driver = Driver::new(root_handle, boot_dispatcher, Parity::Even);

moire::spawn(async move { session.run().await });
moire::spawn(async move { root_driver.run().await });
```

Key points:
- The virtual connection acceptor is registered at `acceptor(...).on_connection(...)` build time
- `ConnectionAcceptor::accept()` is called **synchronously** from the session run loop when
  `ConnectionOpen` arrives — no async, no blocking
- The `AcceptedConnection::setup` callback receives the `ConnectionHandle` and must spawn its
  own `Driver`
- The root connection (boot protocol) gets its own `Driver` with `BootDispatcher`

### `BootDispatcher` — where is it now?

In the current (partially migrated) `vx-io/src/boot.rs`, `BootDispatcher` is NOT defined. It was
generated by `#[roam::service]` on the `Boot` trait as a `ServiceDispatcher` in v6. In v7, the
`#[roam::service]` macro generates `BootDispatcher` as a `Handler<R>` implementor.

So `use vx_io::boot::BootDispatcher` should still work once the roam macro regenerates — the name
`BootDispatcher` is now generated by `#[roam::service]` as a dispatcher that implements
`Handler<R>` (not `ServiceDispatcher`).

### Type alias changes in daemon files

Files like `vxd/src/main.rs` have:
```rust
pub type UpstreamCaller = roam_stream::ConnectionHandle;
```
This needs to become:
```rust
pub type UpstreamCaller = roam_core::DriverCaller;
```
(Same for `CasCaller`, `VfsdCaller` in vx-runner, `StoreCaller` in vx-vfsd, etc.)

### `boot_and_connect` signature changed

Old: `boot_and_connect(&root_handle, None)` or `boot_and_connect(&root_handle, Some(Box::new(dispatcher)))`
New: `boot_and_connect(&caller, &session_handle)` — no dispatcher argument

The old API allowed passing a callback dispatcher for virtual connection acceptance. In v7, the
dispatcher is registered in the session `.on_connection()`. BUT: `vx_io::boot::boot_and_connect`
is the CLIENT-SIDE helper (it calls `boot()` RPC and opens a virtual connection). For virtual
connection callbacks from the server, you use `ConnectionAcceptor`.

So for the client-side `vx`:
- Old: `handle.boot_and_connect(Some(Box::new(ProgressListenerDispatcher::new(...))))`
- New: The progress listener callback needs to be registered differently...

Actually wait — the progress listener was a DISPATCHER for callbacks FROM the daemon TO the vx
client. In v7, the client's virtual connection driver also needs a handler for inbound calls.

So on the client side (vx):
```rust
// Client opens virtual connection with a handler for inbound calls (progress callbacks)
let progress_listener = ProgressListenerImpl::new(mode.clone());
let progress_dispatcher = ProgressListenerDispatcher::new(progress_listener);
let virtual_handle = session_handle.open_connection(settings, vec![]).await?;
let mut driver = Driver::new(virtual_handle, progress_dispatcher, Parity::Odd);
let virtual_caller = driver.caller();
moire::spawn(async move { driver.run().await });
```

This means `boot_and_connect` in `vx_io::boot` needs to either:
1. Accept an optional handler (as it did before, but typed differently)
2. Or be split: `boot()` on its own, then the caller sets up the virtual connection with its handler

Looking at the migrated `boot_and_connect`:
```rust
pub async fn boot_and_connect(caller: &DriverCaller, session_handle: &SessionHandle) -> Result<DriverCaller>
```
It currently uses `Driver::new(virtual_handle, (), Parity::Odd)` — the no-op handler. This is
fine for services that don't need callbacks, but vx needs the progress listener dispatcher.

**Suggested fix**: make `boot_and_connect` generic:
```rust
pub async fn boot_and_connect<H: Handler<DriverReplySink>>(
    caller: &DriverCaller,
    session_handle: &SessionHandle,
    handler: H,
) -> Result<DriverCaller>
```

## Files Still Needing Migration

### moire-only changes (no roam changes needed):
- `crates/vx-io/src/lib.rs` — `moire::peep!(...)`, `moire::fs::*`
- `crates/vx-io/src/boot.rs` — `moire::WatchReceiver/Sender`, `watch!`, `once_cell!`, `moireContext`
- `crates/vx-io/src/spawn.rs` — `moire::Child`, `moireContext`, `wait_with_cx`
- `crates/vx-fskit/src/macos/probe.rs` — `output_with_cx`, `spawn_with_cx`, `wait_with_cx`
- `crates/vx-fskit/src/macos/process.rs` — `moireContext`, `output_with_cx`
- `crates/vx-fskit/src/macos/setup_impl.rs` — `moireContext`, `output_with_cx`, `moire::fs`
- `crates/vx-fskit/src/macos/teardown_impl.rs` — `moireContext`, `spawn_with_cx`, `wait_with_cx`
- `crates/vx-vfs/src/types.rs` — `moire::WatchReceiver<bool>`
- `crates/vx-vfs/src/macos.rs` — `moire::WatchSender`, `moire::channel!`, `moire::watch!`,
  `Sender`, `spawn_tracked!`, `send_with_cx`, `recv_with_cx`, `moire::fs`, `moireContext`

### Both moire + roam changes needed:
- `crates/vxd/src/main.rs` — `moire::Child`, `Mutex`, `mutex!`, `init!`, `spawn_tracked!`,
  `peep!`, `roam_stream::{HandshakeConfig, NoDispatcher, accept}`, `roam_stream::ConnectionHandle`,
  `UpstreamCaller` type alias, `boot_and_connect(None)` calls, `NoDispatcher` in `ServiceBuilder`
- `crates/vxd/src/executor.rs` — `moire::Mutex`, `mutex!`, `spawn_tracked!`
- `crates/vxd/src/service.rs` — `moire::Mutex`, `spawn_tracked!`, `roam_stream::moire::prelude::*`
- `crates/vx-runner/src/main.rs` — `moire::OnceCell`, `mutex!`, `once_cell!`, `init!`,
  `spawn_tracked!`, `peep!`, `roam_stream::{HandshakeConfig, NoDispatcher, accept}`,
  `roam_stream::ConnectionHandle`
- `crates/vx-runner/src/service.rs` — `roam_stream::moire::prelude::*`, `peep!`, `roam::Context`
- `crates/vx-runner/src/sandbox.rs` — `roam_stream::moire::prelude::*`, `mutex!`, `spawn_tracked!`
- `crates/vx-runner/src/service_vfs.rs` — `moire::process::Command`
- `crates/vx-store/src/main.rs` — `moireContext`, `mutex!`, `init!`, `spawn_tracked!`, `peep!`,
  `roam_stream::{HandshakeConfig, accept}`, `recv_with_cx`
- `crates/vx-store/src/toolchain.rs` — check for moire usage
- `crates/vx-store/src/registry.rs` — check for moire usage
- `crates/vx-store/src/service.rs` — `roam_stream::moire::prelude::*`
- `crates/vx-vfsd/src/main.rs` — `moire::init!`, `spawn_tracked!`, `moire::fs`, `peep!`,
  `roam_stream::{HandshakeConfig, NoDispatcher, accept}`, old `reconnecting_client_named` call,
  old `boot_and_connect(&root_handle, None)` signature
- `crates/vx-vfsd/src/vfs.rs` — `moire::Mutex`, `roam_stream::moire::prelude::*`
- `crates/vx-http/src/lib.rs` — `moireContext`, `Mutex`, `Notify`, `lock_with_cx`,
  `notified_with_cx`, `moire::Source::caller()`
- `crates/vx/src/main.rs` — `moire::init!`, `roam_stream::ConnectionHandle`,
  `roam_stream::moire::prelude::*`, `NoDispatcher` in `ServiceBuilder`,
  `boot_and_connect(Some(Box::new(dispatcher)))`, `roam::Context` in cmd_kill
- `crates/vx/src/tui.rs` — `moire::Mutex`, `OneshotReceiver`, `OneshotSender`, `mutex!`,
  `oneshot!`, `spawn_tracked!`

### Special cases

**`vx-vfsd/src/main.rs`** `connect_to_store` function still uses the old
`vx_io::net::reconnecting_client_named` which was removed from vx-io. Needs to use
`ServiceBuilder` instead.

**`roam_stream::moire::prelude::*`** — several files use this import. It's unclear whether
roam_stream still exports a `moire` re-export in v7. Likely not. These files probably got the
moire re-export through roam_stream v6. Remove these imports and add direct moire/tokio imports.

**`roam::Context` in service impls** — in v7 the handler methods no longer take `&roam::Context`.
Any remaining `impl Foo for Bar { async fn method(&self, _cx: &roam::Context, ...) }` need to
become `impl FooServer for Bar { async fn method(&self, call: impl roam::Call<...>, ...) }`.

**`moire::Source::caller()`** — referenced in vx-http and vx-reaper. This was used to create
named entities with call-site tracking. In the new API, `Notify::new` only takes a name (no
source). Drop the `Source::caller()` arg.

## Daemon Accept Loop — Concrete Template

For each daemon (vxd, vx-runner, vx-store, vx-vfsd), the accept loop becomes:

```rust
// In the connection handler task:
let (r, w) = tokio::io::split(stream);
let link = roam_stream::StreamLink::new(r, w);
let conduit = roam_core::BareConduit::<roam_types::MessageFamily, _>::new(link);

let (mut session, root_handle, _session_handle) =
    roam_core::session::acceptor(conduit)
        .on_connection(VirtualConnectionAcceptor { boot_service: boot_service.clone() })
        .establish()
        .await
        .map_err(|e| eyre::eyre!("session failed: {e}"))?;

let boot_disp = BootDispatcher::new(boot_service.clone());
let mut root_driver = roam_core::Driver::new(root_handle, boot_disp, roam_types::Parity::Even);

moire::spawn(async move { session.run().await });
moire::spawn(async move { root_driver.run().await });
```

Where `VirtualConnectionAcceptor` is a local struct that implements `ConnectionAcceptor`:
```rust
struct VirtualConnectionAcceptor { boot_service: BootService<FooService> }

impl roam_core::session::ConnectionAcceptor for VirtualConnectionAcceptor {
    fn accept(
        &self,
        _conn_id: roam_types::ConnectionId,
        _peer_settings: &roam_types::ConnectionSettings,
        _metadata: &[roam_types::MetadataEntry],
    ) -> Result<roam_core::session::AcceptedConnection, roam_types::Metadata<'static>> {
        let Some(service) = self.boot_service.gate().service() else {
            return Err(vec![]); // reject: boot not complete
        };
        let dispatcher = FooDispatcher::new(service.clone());
        Ok(roam_core::session::AcceptedConnection {
            settings: roam_types::ConnectionSettings {
                parity: roam_types::Parity::Even,
                max_concurrent_requests: 64,
            },
            metadata: vec![],
            setup: Box::new(move |handle| {
                let mut driver = roam_core::Driver::new(handle, dispatcher, roam_types::Parity::Even);
                moire::spawn(async move { driver.run().await });
            }),
        })
    }
}
```

## Cargo.toml Changes Still Needed

For each daemon crate that needs the new roam API, add to its `Cargo.toml`:
```toml
roam-core = { workspace = true }
roam-types = { workspace = true }
```

And remove any references to old roam_stream high-level types (keeping roam-stream for StreamLink).

## `ServiceBuilder` and `NoDispatcher`

Old usage in vxd, vx-runner, vx:
```rust
ServiceBuilder::new("vx-store", store_endpoint.clone(), NoDispatcher)
```

New: `ServiceBuilder` no longer takes a dispatcher (removed generic). Just:
```rust
ServiceBuilder::new("vx-store", store_endpoint.clone())
```

The new `ServiceHandle` returned has `.caller` (DriverCaller) and `.session_handle` (SessionHandle).
`boot_and_connect` on it now takes no dispatcher.

## vx-specific: Progress Listener

The old code in `vx/src/main.rs`:
```rust
let progress_listener = ProgressListenerImpl::new(self.mode.clone());
let dispatcher = vxd_proto::ProgressListenerDispatcher::new(progress_listener);
let virtual_handle = handle.boot_and_connect(Some(Box::new(dispatcher))).await?;
```

In v7, `boot_and_connect` doesn't take a dispatcher. The progress listener needs to be the
handler of the virtual connection's Driver. So `boot_and_connect` needs to be made generic
over `H: Handler<DriverReplySink>`, or the caller needs to manually do what `boot_and_connect`
does and set up its own driver with the dispatcher.

Simplest fix: add a `boot_and_connect_with_handler<H>` variant (or make `boot_and_connect` generic).

## `roam_stream::moire::prelude::*`

In v6, `roam_stream` re-exported moire primitives. This import is seen in:
- `vxd/src/main.rs`
- `vxd/src/executor.rs`
- `vxd/src/service.rs`
- `vx-runner/src/main.rs`
- `vx-runner/src/sandbox.rs`
- `vx-runner/src/service.rs`
- `vx-vfsd/src/main.rs`
- `vx-vfsd/src/vfs.rs`
- `vx/src/main.rs`
- `vx/src/tui.rs`

All `use roam_stream::moire::prelude::*;` lines should be **removed**. Any moire types they
provided should be imported from `moire` directly.

## roam::Context Still Present

`vx-runner/src/service.rs` still has the old-style impl:
```rust
impl Runner for RunnerService {
    async fn version(&self, _cx: &roam::Context) -> ServiceVersion { ... }
    async fn compile_rust(&self, _cx: &roam::Context, ...) { ... }
    // etc.
}
```
This needs to be updated to v7 style with `call: impl roam::Call<ReturnType, ErrorType>`.
