+++
title = "Internals"
insert_anchor_links = "heading"
+++

This page summarizes the current internal architecture of vixen and highlights
areas that are still evolving.

## The store

`vx-store` is content-addressed storage built on BLAKE3. Everything is stored as
blobs: source files, compiler outputs, and toolchain payloads.

Large blobs are split with content-defined chunking (CDC), so similar files
share most chunks. File trees are represented as Merkle trees: a manifest lists
entries and blob hashes, and the manifest itself is also content-addressed.

This provides automatic deduplication, verifiable integrity, and efficient sync
between local and remote stores.

## The runner

`vx-runner` executes build actions in a sandbox, with inputs served from a
virtual filesystem backed by the store.

- macOS uses FSKit
- Linux uses FUSE
- Windows uses ProjFS

The compiler only sees declared inputs. Outputs are written inside the mounted
build view and committed back to the store when the action finishes.

This is the basis of hermeticity in vixen: if an action cannot observe
undeclared host state, cache keys remain meaningful.

## Debugging with moire

All vixen daemons are instrumented with
[moire](https://github.com/bearcove/moire), a low-overhead diagnostics system.
Locks, async tasks, threads, and RPC state are tracked.

When a build appears stuck, `vx debug` requests diagnostics from running daemons
and aggregates the collected state into a local dashboard.

## Rule language direction

vixen is exploring a rule language (built on
[styx](https://styx-lang.dev)) for declarative build action definitions.
The intent is to describe inputs, outputs, and commands in rules rather than
hard-coding tool invocations.

This area is still under active design and may change substantially.
