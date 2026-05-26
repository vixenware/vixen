+++
title = "Three-tier product strategy"

[extra]
type = "Vision"
status = "living"
created = "2026-05-26"
updated = "2026-05-26"
tags = ["strategy", "product", "tiers"]
+++

Vixen ships as three product tiers. Each is a viable standalone offering. Each primes demand for the next. The first two run *through cargo*; the third is where the customer finally writes Vixen.

## Tier 1 — CCC (Cargo Compile Cache)

A [[CCC is a cargo fork, not a rustc wrapper|cargo fork]] distributed as a [[CCC is distributed as a rustup mirror|rustup mirror]]. Intercepts the build at the cargo level and consults the Vixen cache; skips entire subgraphs when downstream artifacts are cached. Auth is [[CCC authentication uses OIDC trusted publishing|OIDC]]; [[Unauthenticated CCC requests miss but contribute demand signal|unauthenticated requests miss]] but contribute demand signal. See [[Tier 1 — CCC]].

The bet: most "slow Rust CI" pain is build time, and a correctness-first remote cache collapses the build window without asking customers to change their build system, their runner choice, or their workflow.

## Tier 2 — Vixen Build

**Customer still calls `cargo build`.** The Vixen sandbox + VFS + cache + executor fleet runs the build hermetically on Vixen-controlled infrastructure; the customer's runner becomes a thin orchestration host. The customer never sees the `vx` CLI, never installs FSKit on macOS or projected FS on Windows, never authors `.vx` files. See [[Tier 2 — Vixen Build]].

The bet: cache-everything plus sandbox plus executor fleet, sold as a cargo-transparent service, lets us own the entire build epistemology without asking customers to adopt a new build system. And because the Vixen language has no external callers at this tier, we can keep iterating on it under real-world load with no stability burden — see [[Vixen language stays internal until Tier 3]].

## Tier 3 — Vixen CI

**Customers write Vixen.** `.vx` files in the repo describe CI: jobs, status reporting, fly-check behaviour. Same evaluation engine as Tier 2; new authoring surface. GitHub Actions, the Azure runner agent, and the workflow YAML layer all become redundant. See [[Tier 3 — Vixen CI]].

The bet: once the build is signed, reproducible, distributed, and attested, there is nothing CI does that the build doesn't already do better — and CI itself becomes a single source of truth versioned with the code it describes. This is also the first tier where the Vixen language graduates to a public API contract.

## Sequencing

Tier 1 funds Tier 2's R&D and exercises the cache + trust-gate machinery in production against real customer load. Tier 2 absorbs everything Tier 1 learned and adds the language (internal), sandbox, and remote-exec primitives — and ships commercially without ever exposing the language to customers. Tier 3 is unlocked once Tier 2's build graph and language are mature enough to expose externally; the language stabilisation is the gating concern.

Tier 2 work does not wait for Tier 1 to ship. The architectural decisions converge regardless of release order.

## Why three tiers, not one

Each tier asks more of the customer (trust, lock-in, surface area) and delivers more in return. The tier ladder lets a customer enter at Tier 1 with zero commitment and move up as they trust the platform. It also lets the same architecture sell to three different buyer profiles:

- **Tier 1 buyer:** an engineer fed up with slow Rust CI. Cares about speed.
- **Tier 2 buyer:** a platform team that wants the speed *and* the cross-language / multi-platform / provenance properties — without owning a new build system. Cares about hermeticity, attestation, fleet utilisation. Still uses cargo.
- **Tier 3 buyer:** mixed audience. At small org size, individual devs paying for fly-check velocity. At large org size, compliance / SRE / platform-engineering buying CI-as-build-graph for the audit story.

The technical core is the same. The framing isn't.
