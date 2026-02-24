+++
title = "Specification"
insert_anchor_links = "heading"
+++

vixen does not yet publish a frozen, versioned specification. This page captures
what is currently specified, what is intentionally out of scope, and what is
still under design.

## Current guarantees

The current implementation commits to the following behavior:

- Build orchestration is handled by `vxd`; storage and execution are separate daemons
- Build artifacts are stored in content-addressed storage
- Build actions run in a sandboxed virtual filesystem
- Unsupported project shapes fail explicitly instead of silently degrading

## Current scope

vixen currently targets a restricted Rust subset. The implementation is
intentionally narrow relative to the long-term goal, and support is expected to
expand over time as core guarantees harden.

## Not yet specified

These areas are active work and not yet stable:

- Rule language syntax and semantics
- Full remote/distributed execution behavior
- Cross-project cache sharing and policy controls
- Compatibility commitments across future releases

## Source of truth today

Until a formal spec is published, use these references:

- [README](https://github.com/bearcove/vixen)
- [Internals](/internals/)
- [Daemon topologies](/daemons/)

This page will be replaced by a versioned specification once interfaces and
behavioral guarantees stabilize.
