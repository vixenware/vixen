+++
title = "Tier 2 — Vixen Build"

[extra]
type = "Vision"
status = "living"
created = "2026-05-26"
updated = "2026-05-26"
tags = ["strategy", "vixen-build", "tier-2"]
+++

> Cache *everything*. Customer still calls `cargo build`.

Tier 1 caches what a cargo fork can see from the runner. Tier 2 caches everything else — and the customer never knows the difference at the source level.

## What Tier 2 actually is

The customer's CI keeps invoking `cargo build`. The [[CCC is a cargo fork, not a rustc wrapper|cargo fork]] from Tier 1 is still the entry point. What changes is *where the build runs*: a Tier 2 cargo invocation is shipped to Vixen's executor fleet, evaluated against a sandbox + VFS + cache substrate, and the artifacts come back to the customer's runner as if cargo had just done it locally — faster.

The customer's runner is now an orchestration host, not a build host. They still own their CI workflow. They still pay for their runner (smaller, because the build window collapsed). Every non-Rust-build step (artifact uploads, deploys, custom scripts) runs untouched on the runner.

## What the customer does NOT do

- They do not install a `vx` CLI. We do not distribute one for this tier.
- They do not enable Mac OS FSKit or Windows projected file system on their machines. The VFS lives on Vixen's executors.
- They do not author `.vx` files. The Vixen language is not exposed at Tier 2 — see [[Vixen language stays internal until Tier 3]].
- They do not switch build systems. Cargo is still the build system from their perspective.
- They do not own the sandbox configuration. That's our problem on our executors.

The promise: "cargo, transparently — but with cache-everything."

## Why CCC alone is structurally limited

[[Tier 1 — CCC]] sees the build through cargo's eyes *on the customer's runner*. That gets us subgraph elision and rustc-invocation caching (see [[Subgraph elision]]). But it stops there. CCC does not see — and cannot constrain — what `build.rs` reads from `/usr/include`, what the linker opens, what proc-macros poke at, what `pkg-config` scrapes, what `cmake` decides about your local OpenSSL.

Caching *everything* requires two pieces a cargo fork on a customer-controlled runner structurally cannot provide:

- **A virtual filesystem on the executor.** Vixen sees exactly which bytes the build reads, interposes the cache at the byte-read level. No process can quietly depend on something we didn't hash.
- **A sandbox on the executor.** The build *cannot* read bytes outside the read-set we declared. The sandbox is what makes the cache key correct.

The customer's runner can't host either of these (or rather, we don't want to ship the operational complexity to them). The executor fleet can.

## The trust ask

Bigger than Tier 1, but along a different axis than I'd first framed.

At Tier 1, the customer trusted us with cache *correctness* (we won't return wrong bytes). At Tier 2, they additionally trust us with *execution* — their build is happening on infrastructure we control, against environment assumptions we declare. If our environment differs in a meaningful way from what their `build.rs` expects, their build breaks.

The ask: *trust that the way Vixen builds your project produces the same artifacts cargo would on your runner*.

## How we de-risk the ask

- **Cargo-transparent surface.** The customer's `cargo build` is the same invocation. If our backend misbehaves, the customer falls back to the previous tier (Tier 1, or even pure cargo) by flipping their rustup source URL. No code changes.
- **A/B verifiable.** Customer can run the same build on Tier 2 and on plain cargo, compare outputs byte-for-byte. The compatibility promise is *testable* by the customer themselves.
- **Tier 1 is the on-ramp.** Customers arrive via the rustup mirror, get the Tier 1 win, learn that the trust gate and the OIDC pipeline behave. Tier 2 extends the same trust to a wider surface — same payment relationship, same identity, same fallback.

## Why the language can stay internal

The customer never writes `.vx` files. They don't see the language. They don't import our stdlib. The Vixen language is the *internal substrate* for Vixen's implementation of Rust build semantics, and nothing else.

This means we can ship Tier 2 commercially before the language is "done." Multiple revisions, refactors, breakages — all invisible to the customer because they have no callers in the language. Real-world workloads validate the design while we still have the freedom to change it. See [[Vixen language stays internal until Tier 3]].

## What that unlocks

Cache-everything plus the sandbox plus the graph gives us things no other build system delivers. Several of them are customer-visible (they consume the *output* of these capabilities, not the language behind them):

- Cross-language hermetic builds — your cargo workspace plus its `cc-rs` or `cxx` C/C++ pieces all live in one cache.
- Multi-platform fan-out — you call `cargo build` once, we build for linux × macOS × windows × arm/x86 from one invocation.
- SLSA-grade provenance attestation by graph traversal — see [[The build graph is also a provenance graph]].
- Time-travel build replay; bisect-as-a-service against the cache.
- Sub-file incremental invalidation via the sandbox read-set.
- License/origin policy enforcement at build time.

Full catalogue: [[Enterprise feature candidates]].

## Carryover from Tier 1

Everything CCC builds — wire protocol, cache infrastructure, trust gates, demand-warming pipeline, OIDC verifier, rustup mirror — transfers untouched. Tier 1 is the customer-funded prototype of Tier 2's substrate.

## Architectural decisions still open

- [[Cache key + sandbox: one design doc?]] — the prerequisite that "cache everything" is built on.
- [[Streams as first Tier-2 primitive?]] — which primitive to drive first.
- [[Process model]] — processes as opaque inputs, but their outputs as streams.

## Related

- [[Tier 1 — CCC]] — the on-ramp.
- [[Tier 3 — Vixen CI]] — where the language finally gets exposed.
- [[Vixen language stays internal until Tier 3]] — why Tier 2 can ship without language stabilisation.
- [[Trust is per-cache, not global]] — extends naturally to Tier 2.
- [[Declared identity collapses the read-set under trust]] — how the sandbox closure becomes tractable.
