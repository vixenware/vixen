# Eval2 mount modes: tracked and opaque

## Status

This document is a design note for evaluator v2 and the distributed runtime.
It is intentionally narrower than a full Vixen language spec.

The goal is to write down the parts that currently seem settled enough to guide
implementation:

- some mounts must participate in fine-grained dependency tracking
- some mounts must be treated as opaque bundles
- this split is independent from where the mount data comes from
- macOS FSKit is the cross-platform constraint that prevents per-request job
  attribution for shared visible paths

Everything else can be reconciled later.

## Problem

We want two things that pull in opposite directions:

1. Precise dependency tracking for source inputs
2. Stable shared paths for read-only bundles like toolchains and SDKs, so the
   operating system page cache can warm up and be reused across many jobs

Today, the runner gets per-job attribution by giving each action its own unique
prefix under one global VFS mount:

- [`runner_mount_base()`](/Users/amos/bearcove/vixen/crates/vx-runner/src/main.rs#L78)
- [`create_action_context()`](/Users/amos/bearcove/vixen/crates/vx-runner/src/service_vfs.rs#L635)

That works for dependency accounting, but it means the same bundle is visible at
different paths in different jobs. For path-sensitive toolchains, this hurts
performance and defeats useful filesystem caching.

The obvious alternative is to share the same visible mount path across many
jobs and ask the VFS which process requested each read or lookup. That is not a
portable foundation:

- Linux FUSE can expose caller identity
- Windows ProjFS can expose triggering process information
- macOS FSKit, as currently exposed through the public `FSVolume` callback
  surface, does not appear to provide per-file-operation caller identity

So we need a design that does not depend on per-request process identity.

## Core distinction

The important distinction is not:

- host path vs store
- discovered vs local
- proxy vs materialized

Those describe where bytes come from.

The important distinction is:

- `tracked`
- `opaque`

Those describe how a mounted input participates in dependency tracking and cache
keys.

## Tracked mounts

A tracked mount is a mount whose accesses are part of the dependency graph.

Properties:

- usually action-scoped
- usually visible under a per-action namespace
- per-access tracking is enabled
- lookups, misses, file reads, and directory enumeration matter
- overmounting matters
- invalidation is based on the observed access set, not on the whole tree hash

Typical examples:

- source trees
- generated headers
- configuration files
- small input trees where precise incremental invalidation matters

Tracked mounts are for data where we care which exact files were read, which
paths were absent, and which directories were enumerated.

## Opaque mounts

An opaque mount is a read-only mounted bundle that contributes one identity as a
whole.

Properties:

- always read-only
- can be shared across many jobs at a stable visible path
- no per-access dependency tracking
- no lookup-miss tracking
- no directory-enumeration tracking
- contributes one bundle identity to cache keys
- if any relevant part of the bundle changes, the whole bundle identity changes

Typical examples:

- MSVC
- Xcode toolchain roots
- SDK bundles
- system library bundles
- prebuilt compiler trees

Opaque does not mean ambient or host-only. An opaque mount may come from:

- a discovered host path
- the store
- a proxy to another node
- any other runtime source

What matters is that it behaves as one read-only bundle for caching purposes.

## Writable roots

Writable roots are separate from both tracked and opaque mounts.

Properties:

- action-private
- writable
- never shared
- flushed back into CAS or equivalent output storage later

They are outputs, not reusable inputs.

## Why tracked and opaque are orthogonal to transport

The source of a mount and the tracking mode are different axes.

Examples:

- a toolchain discovered on the host may be mounted as opaque
- a tree fetched from the store may be mounted as tracked
- a proxied bundle from another node may be mounted as opaque
- a proxied source tree from another node may be mounted as tracked

So the runtime model needs at least two independent pieces of metadata:

- where the mount comes from
- whether it is tracked or opaque

## Cache-key contribution

Tracked and opaque mounts contribute to cache keys differently.

### Tracked mount contribution

A tracked mount contributes the observed dependency set:

- file reads
- file attribute reads, if they matter
- lookup misses
- directory enumeration snapshots

The cache key uses the identities of the observed edges, not the identity of
the whole mounted tree.

### Opaque mount contribution

An opaque mount contributes one bundle identity.

This can be:

- a tree hash
- a discovered grant identity
- a content-addressed bundle fingerprint
- some other runtime-defined stable identity

The cache key does not care which individual files were touched inside the
bundle.

## Cross-platform consequence

On macOS, shared visible paths cannot also be the source of precise per-job VFS
attribution unless we find a different attribution mechanism above the
filesystem layer.

That means:

- tracked mounts should stay action-scoped and attributable
- opaque mounts can use stable shared visible paths

This is the main reason the split exists.

It is not primarily about trust or host-vs-store.
It is primarily about the interaction between:

- dependency precision
- shared-path performance
- platform filesystem APIs

## Relation to discovery and verification

Discovery and verification are related but separate concerns.

Discovery answers questions like:

- which host path or bundle should we use?
- what facts make it valid to use?

Tracked vs opaque answers:

- once mounted, do we track fine-grained accesses or treat it as one bundle?

Examples:

- a discovered MSVC installation may yield an opaque mount grant
- a discovered source checkout path may still be mounted as tracked

So:

- discovery decides what is available and under which verifier
- mount mode decides how it participates in dependency tracking

## Desired runtime behavior

For an action, the runtime should be able to say:

- this input is a tracked mount
- this input is an opaque mount
- this path is a writable root

And then enforce:

- tracked mounts are observable and attributable
- opaque mounts are shared, stable-path, read-only, and coarse-grained
- writable roots are private and excluded from shared reuse

## Current implementation status

The current implementation now exposes one concrete tracked/opaque difference at
the runner boundary:

- opaque mounts are mounted at stable shared visible paths
- tracked mounts stay action-scoped
- the runner returns tracked observations under tracked mount roots as part of
  `RunResult`

That is intentionally narrower than the full target model above.

What is implemented today for tracked mounts:

- successful observation of CAS-backed files via file reads and attribute reads
- directory snapshots via `read_dir`
- lookup-miss tracking rooted in CAS-backed directories
- filtering to the explicitly tracked mount roots for the action

What is not implemented yet:

- using tracked observations in `vxd` cache keys or incremental invalidation

So the current system distinguishes tracked and opaque operationally, but only
for the first slice of tracked input reporting.

## What this does not decide yet

This document does not settle:

- user-facing syntax
- how discovery is spelled
- whether `stabilize` is renamed to `lock`
- whether mount mode is inferred or explicit in user code
- exact cache storage layout
- exact RPC protocol for cross-node mounts

It only fixes the conceptual split that the implementation should preserve.

## Immediate implementation consequences

1. The runner/spec/runtime model should gain an explicit mount mode:
   - `tracked`
   - `opaque`

2. Opaque mounts should be allowed to use stable shared visible paths.

3. Tracked mounts should remain per-action and attributable.

4. Dependency recording should grow support for:
   - lookup misses
   - directory enumeration snapshots
   for tracked mounts only.

5. Cache keys should distinguish:
   - tracked dependency edge sets
   - opaque bundle identities

## Open questions

1. Should the names be `tracked` / `opaque`, or something else?
2. How should opaque bundle identity be represented in spec fixtures?
3. Can one action use both tracked and opaque views of the same underlying
   content, or should that be forbidden?
4. Should tracked mounts ever be shared across jobs on platforms that do expose
   caller identity, or is the cross-platform model intentionally stricter?
