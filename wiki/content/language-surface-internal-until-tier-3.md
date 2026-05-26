+++
title = "Vixen language stays internal until Tier 3"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["strategy", "language", "tier-2", "tier-3", "api-surface"]
+++

## Context

Vixen has a build language. The runtime evaluates `.vx` files into a build graph, ships closures to executors, etc. The question is when that language becomes a *public API surface* — when customers write `.vx` files and we owe them stability.

Three viable answers: expose at Tier 2 (customers author hermetic builds), expose at Tier 3 (CI tier), never expose (purely internal forever).

## Options considered

1. **Expose at Tier 2.** Customers write `.vx` build files for hermetic, multi-language, cache-everything builds. The language is part of the product. Stability commitments start as soon as Tier 2 ships.
2. **Expose at Tier 3.** Customers stay on cargo at Tier 2; Tier 2 is "cargo build transparently routed through Vixen's sandbox + cache + executor fleet" — no `.vx` files visible to customers. Customers only author `.vx` when they buy Tier 3 (CI replacement, fly-check, etc.).
3. **Never expose.** Vixen remains a hosted service forever; customers never write Vixen code. Loses the long tail of customisation and the "your CI is in your repo" Tier 3 value prop.

## Decision

Option 2. The Vixen language surface is **internal** through Tier 1 and Tier 2. It becomes a **public API contract** at Tier 3.

## Consequences

- **Tier 2 ships commercially without "finishing" the language.** The language, stdlib, and Rust-build-semantics implementation can iterate freely — multiple incompatible revisions, refactors driven by real-world workload observation, breakage that only Vixen-internal code is exposed to.
- **No `vx` CLI distribution at Tier 1 or Tier 2.** No FSKit-on-macOS instructions, no Windows projected-FS instructions, no `.vx` authoring tutorials in the marketing surface. The customer touches cargo and rustup; everything else is opaque service infrastructure.
- **Tier 3 carries a step-up in commitment.** Once customers author `.vx`, deprecation cycles, semantic versioning, migration tooling, LSP stability, and IDE support all become real product concerns. Tier 3 pricing reflects this.
- **Real-world validation comes first.** Two years of Tier 2 customers using Vixen to build their cargo workspaces means the language and stdlib have seen real-world build patterns before any external developer writes against them. The Tier 3 launch surface is informed by data, not by speculation.
- **Internal documentation can be aggressive.** The internal-only language can have experimental features, undocumented escape hatches, and "this is wrong but it works for now" implementations. They get cleaned up before Tier 3.
- **No "we'll keep it backwards compatible" anti-pattern.** Backwards compatibility is the discipline of the externally-exposed API. The internal surface gets refactored when it should be refactored. See the project's general stance against backwards compatibility absent explicit need.

## Related

- [[Tier 1 — CCC]]
- [[Tier 2 — Vixen Build]]
- [[Tier 3 — Vixen CI]]
- [[Three-tier product strategy]]
