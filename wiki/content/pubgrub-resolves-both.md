+++
title = "PubGrub solves both cargo and Vixen package resolution"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["language", "resolver", "packaging", "cargo"]
+++

## Context

We need a version-constraint solver in at least two places: for Cargo dependency resolution under `vixen-rust`, and for resolving Vixen packages themselves. PubGrub is the modern algorithm (Dart's pub used it first; cargo's resolver-2.0 work is heading toward it). Implementing two different solvers — one for cargo, one for Vixen — is wasteful and ergonomically inconsistent.

## Decision

PubGrub is implemented once, as a generic constraint solver in the Rust runtime (an intrinsic), and used to resolve both:

- **Vixen packages**, by a small "core" Vixen module distributed with the runtime. The core knows the semantics of Vixen package resolution (manifests, semver ranges, the dep graph) and uses PubGrub to do the solving.
- **Cargo crates**, by `vixen-rust` (the Vixen package that knows cargo's domain model). `vixen-rust` declares Cargo's resolution semantics — workspaces, features, `[patch]`, target-specific deps, version selection rules — and uses PubGrub the same way.

## Consequences

- **One solver to maintain.** PubGrub is generic; the domain knowledge (what counts as a compatible version, what dep relationships mean) is supplied by the caller. Domain logic stays in Vixen code; algorithm stays in Rust.
- **Bootstrap is clean.** The "tiny core" Vixen package ships embedded with the runtime, so resolving Vixen packages doesn't require already having resolved Vixen packages. PubGrub is an intrinsic; the core is a known-fixed Vixen module the runtime carries. No chicken-and-egg.
- **Same resolution UX everywhere.** A user encountering "no version satisfies the constraints" from `vixen-rust` gets the same kind of error message as from Vixen package resolution. Same engine; same diagnostics.
- **Cargo's existing semantics are preserved.** `vixen-rust` codes the cargo model in Vixen, including the bits where cargo does Yet Another Domain-Specific Thing (feature unification, dev-deps vs build-deps, etc.). The output is byte-identical to what cargo would produce — see [[Lockfiles are build outputs, not input mutations]].
- **PubGrub for any future domain is free.** When `vixen-python` or `vixen-node` arrives, they declare their semver model and call into the same intrinsic. No additional solver work.

## Related

- [[Domain logic lives in Vixen, not in the runtime]]
- [[Package management for Vixen]]
- [[Rust/Vixen FFI shape]]
- [[Lockfiles are build outputs, not input mutations]]
