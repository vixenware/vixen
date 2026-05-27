+++
title = "The pitch"

[extra]
type = "Doc"
updated = "2026-05-26"
+++

We ship two distinct products. **They are not tiers of a single product**, and conflating them produces broken framing fast. Read each on its own.

## CCC — Cargo Cloud Cache

A forked cargo, distributed as a rustup mirror, that dramatically speeds up Rust CI by sending the build graph to a Vixen-operated cache in one RPC and materialising only what the customer demands. Customer types `cargo build` — never sees `vx`, never authors anything new, never adopts a new build system.

[CCC →](/ccc/)

## Vixen Build

A separate product. A general-purpose build system with its own language, sandbox, distributed executor fleet, and package manager. Customer opts in by running `vx cargo build` instead of `cargo build`; their runner orchestrates while builds execute hermetically on Vixen executors. Caches *everything* the build invokes, supports any language packaged for it, fans out across OSes and architectures from one invocation. Vixen CI is its public-language surface — customers who author `.vx` files for their CI / build description.

[Vixen Build →](/vixen-build/)

## How they relate

Sequenced as **phases of the company**, not as tiers of one product. CCC ships first; its production exercise of cache + trust-gate machinery funds and informs Vixen Build's R&D. Vixen Build builds on lessons learned but is independently designed; a customer can buy Vixen Build without ever having used CCC, and vice versa.

Some infrastructure is shared internally — the content-addressed Store, the per-OS sandbox + VFS layer, the OIDC verifier, the Vixen org / billing identity model. Customers don't see the sharing; they buy a product, not a stack.

## Buyer profiles

- **CCC buyer.** An engineer with slow Rust CI. Cares about wall-clock. Adopts via a one-line workflow change. Doesn't change their build system or their runner.
- **Vixen Build buyer.** A platform team or compliance shop that wants the speed *plus* hermeticity, multi-language, multi-platform, provenance — without owning a new build system internally. Adopts via opt-in (`vx cargo build`); customer runner remains in place. Larger commitment, larger payoff.
- **Vixen CI buyer.** A subset of Vixen Build customers who additionally want `.vx`-authored CI replacing GitHub Actions. Mixed: small orgs for fly-check velocity, large orgs for the audit/compliance story.
