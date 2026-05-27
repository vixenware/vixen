+++
title = "Glossary"

[extra]
type = "Doc"
updated = "2026-05-26"
+++

Short definitions. Anything that needs more than three sentences belongs in [the relevant doc](/), not here.

## CCC (Cargo Compile Cache)

The Tier 1 product. A forked cargo distributed as a rustup mirror; sends the build graph to ccc-backend in one RPC, materialises only what the user demands. See [The pitch](/pitch/#tier-1-ccc).

## ccc-backend

The Vox-powered service `cargo-vixen` speaks to. Owns OIDC verification, graph lookups, demand-queue management, telemetry. Two-layer deployment (n=many edge nodes + n=1 scheduler). Distinct from Store (the CAS sitting behind it). See [Architecture](/architecture-and-infrastructure/#ccc-backend-the-customer-facing-service).

## Demand signal

The information ccc-backend records on a cache miss — the structured inputs that let the trusted-builder fleet replay the invocation. Carried implicitly in the graph-lookup RPC, not a separate message. Unprivileged: anyone, including unauthenticated callers, can emit one. See [Cache and trust](/cache-and-trust/#how-the-cache-warms-demand-signals).

## Trust gate

The per-cache ingress policy: which signatures / attestations / producer identities a cache accepts. Local cache gates by "I wrote it." Public cache gates by "trusted builder signed it." Customer caches gate by whatever the customer configures. See [Cache and trust](/cache-and-trust/#trust-is-per-cache-not-global).

## Declared identity

A trusted shortcut that replaces hashing a toolchain's bytes with one opaque token derived from its declaration (e.g. "Xcode 26.4 manifest hash X"). Works only because we trust the declaration; tampering blast radius is local. See [Cache and trust](/cache-and-trust/#declared-identity-collapses-the-read-set-under-trust).

## Obligation

A constraint produced by a discovery probe that must be verified on later builds. Not just a fingerprint — a claim ("clang at this path has hash X") that gets re-checked concurrently with the work. Mismatch poisons the build and triggers re-dispatch. See [Build and resolution](/build-and-resolution/#discovery-produces-obligations-not-fingerprints).

## Executor class

A type identifying which executors are interchangeable for a given workload — `linux-x86_64`, `darwin-arm64`, exotic targets like `freebsd-aarch64`. Build closures are typed against an executor class; the runtime selects an actual executor at evaluation time and can fan out across all members of a class for multi-platform builds.

## Subgraph elision

The property that separates a cargo fork from a `RUSTC_WRAPPER` wrapper. Because the fork sees the whole dep graph, it can send the entire set of unit identities to ccc-backend in one RPC, get a hit/miss partition back, and materialise only the artifacts the user demands. Intermediate `.rlib`s on cache-hit subgraphs are never written to disk, never fetched. See [The pitch](/pitch/#tier-1-ccc).

## Vixen organization

The first-class Vixen-side identity that owns package scopes, billing accounts, private caches, trust settings. Customers link external orgs (GitHub, Forgejo, GitLab) to a Vixen org via OIDC; subsequent CI workflows in those external orgs authenticate as the Vixen org. One Vixen org can own one or more package scopes. See [Packages and publishing](/packages-and-publishing/#identity-vixen-orgs-link-to-external-orgs).
