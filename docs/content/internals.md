+++
title = "Internals"
insert_anchor_links = "heading"
+++

This page collects design ideas and directions for vixen's internals. Much of
this is aspirational — things are moving fast and details change weekly.

## The store

vx-store is content-addressed storage built on blake3. Everything is a blob:
source files, compiler outputs, toolchain binaries.

Large blobs are split using content-defined chunking (CDC), so similar files
share most of their chunks. Trees of files are represented as merkle trees —
a tree manifest lists entries with their blob hashes, and the tree itself is
just another blob.

This means deduplication is automatic, integrity is verifiable, and syncing
between a local store and a remote one is a matter of exchanging missing chunks.

## The runner

vx-runner executes build actions inside a sandbox, with inputs served from a
virtual filesystem backed by the store. On macOS this is FSKit; on Linux it's
FUSE.

The compiler only sees what's been explicitly placed in the VFS for that action.
It can't reach the host filesystem. Outputs are written to a designated area in
the VFS and flushed back to the store when the action completes.

This is how hermeticity works: the sandbox prevents the compiler from observing
anything outside its inputs, so the cache key is actually meaningful.

## Debugging with pipes

When a build is stuck — a daemon isn't responding, an action is hanging — you
can use Unix pipes to introspect the running system. The daemons expose
diagnostic channels that let you see what queries are in-flight, what RPCs are
pending, and where things are blocked.

## A language for rules

We've started designing a rule language (built on [styx](https://styx-lang.dev))
for describing build actions declaratively. The idea is that instead of
hard-coding how to invoke rustc or zig, you write rules that describe inputs,
outputs, and the command to run. This is early and likely to change
significantly.
