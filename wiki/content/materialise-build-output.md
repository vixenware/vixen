+++
title = "Design build-output materialisation"

[extra]
type = "Task"
status = "todo"
priority = "medium"
tiers = ["2", "3"]
tags = ["runtime", "vfs", "filesystem", "vx-cli"]
created = "2026-05-26"
+++

When `@vixen/cargo::main` returns a `Tree` (or `Blob`) containing the build artifacts, `vx`-the-CLI has to land them on the customer's filesystem at the conventional cargo locations — `target/release/foo`, `target/release/build/...`, etc. — so subsequent CI steps (packaging, deploying, anything that expects cargo's output layout) see the artifacts where cargo would have put them.

Three shapes on the table, none committed to:

- **Copy bytes into `target/`.** Walk the returned Tree, write each file's bytes to disk at the conventional path. Simple. Wasteful — every artifact lives once in the content-addressed store and again as a copy on disk.
- **Hard-link.** Doesn't compose with content-defined chunking. A file's bytes are split into multiple chunks in the store; there's no single inode to hard-link from. Symlinks have the same problem if we tried that route. Dead end.
- **`target/` as a Vixen VFS mount.** The customer's `target/` directory IS a virtual filesystem the runtime backs against the store. "Materialising" is exposing paths through the VFS; no byte copy. Cleanest from a content-addressed perspective. Cost: requires a VFS mount to be live on the customer's machine after the build, with whatever operational consequence that has (process must stay running, mount must survive reboots or be remountable, etc.).

## What needs deciding

- Which shape (copy / VFS-mount / something else) for v1.
- If VFS-mount: lifecycle of the mount. Does it live for the duration of one `vx cargo build`, persist between builds, follow the workspace, get remounted on `vx` startup?
- If copy: deduplication strategy. Reuse store-side chunks to avoid re-reading from the store on incremental rebuilds?
- Cross-platform reality. macOS FSKit and Windows ProjFS mounts have different lifecycle and permissions stories than Linux FUSE. Does the materialisation strategy differ per OS?
- Cleanup story. When does materialised output get garbage-collected (or unmounted)?

## What unblocks resolution

A first deployment — even copy-based — gets us shipping. The cleaner VFS-mount story can come later if copy turns out to be a bottleneck or the operational shape proves out.

## Related

- [Epic: vx cargo build, end-to-end](/vx-cargo-build-trace/)
- [Architecture and infrastructure](/architecture-and-infrastructure/) — the VFS stack we'd build on.
