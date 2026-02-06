# vixenfs-proto

VFS protocol crate for Vixen's macOS FSKit integration.

## What is this?

This crate defines the RPC protocol used between:

- the macOS FSKit extension hosted by `Vixen.app`
- a backend VFS server implementation (for Vixen, this is wired through `vx-rhea`)

The protocol is generic: if you want to implement your own backend, implement the `Vfs` trait from this crate.

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
vixenfs-proto = { path = "../vixenfs-proto" }
roam = { workspace = true }
roam-stream = { workspace = true }
tokio = { version = "1", features = ["full"] }
```

Then implement `Vfs` and serve it over a `roam_stream` transport.

## Notes

- Filesystem type is `vixenfs`
- URL scheme used by the mount helper is `vixen://`
- Root item id is always `1`

## Source of truth

This crate is maintained in the Vixen monorepo:

- https://github.com/bearcove/vixen
