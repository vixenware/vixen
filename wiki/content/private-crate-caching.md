+++
title = "Private crate caching: when is it worth it?"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["ccc", "tier-2", "cache", "private"]
+++

The common-case CCC flow is clean for public crates: the trusted-builder backfill fetches sources from crates.io, builds them on official infrastructure, pushes the output into the cache. No customer upload, tiny wire. See [[CCC service]].

Private crates break that flow. The bytes are not externally fetchable; the customer's tree is the only source. Three shapes are on the table, none committed to yet.

## Options

### 1. Customer-uploaded chunks, demand-gated acceptance

Customer's `cargo-vixen` content-addresses the read-set and uploads missing chunks to Store directly (Store accepts because hash-verified content can't poison a CAS). Cache *acceptance* (the `key → output` mapping) is gated on some demand heuristic — only commit a private-cache entry if it would actually be reused.

The risk: too noisy. Accepting every customer's per-commit binaries fills the cache with single-use artifacts. Storage and operational cost without payoff.

### 2. Customer-local cache only

Private crates never go to the shared cache. Customer's `cargo-vixen` keeps a local on-disk cache of its own private artifacts; cross-machine sharing within the org happens through normal cargo `target/` reuse + GitHub Actions `actions/cache`. No Vixen-side caching of private bytes.

The risk: large orgs lose a real value prop. The whole "dev-laptop cache sharing against the org's remote cache" enterprise feature (#9 in [[Enterprise feature candidates]]) requires *some* private-cache mechanism.

### 3. Tiered private cache with explicit thresholds

Private cache writes are accepted, but only when a heuristic clears:
- Hit count on the derived key crosses a threshold (the same private artifact requested N times → cache it next time it's built).
- Crate metadata signals stability (semver release vs a commit-sha-pinned WIP).
- Customer explicitly opts a dep into caching via configuration (`vixen.toml` or equivalent on the customer side — though [[Vixen language stays internal until Tier 3|we don't extend Cargo.toml's schema]] for this).

The risk: complexity. Each heuristic is a knob. Tuning is a per-customer ops problem.

## What it pays off for

The private-cache feature is most valuable for:
- **Popular internal deps.** A vendored proprietary library that every internal product depends on. Built once, reused everywhere.
- **Stable releases.** Internal deps versioned with semver, released periodically. Low churn → high reuse.
- **Cross-developer reuse.** A developer rebases onto main; if the org's CI built the same merge-base earlier, the developer's local build can hit a private-cache entry.

It is mostly *not* valuable for:
- WIP commits, feature branches, every-PR-builds-from-scratch flows.
- Single-developer, single-repo, low-build-frequency projects.

## What unblocks resolution

Customer data from Tier 1. Once we see real demand-signal patterns from real orgs, we can pick the heuristic — or learn that there's no useful heuristic and private cache wants to be either everything-or-nothing per customer.

## Related

- [[CCC service]]
- [[Demand-warmed cache]]
- [[Tier 2 pricing shape]] — where private cache lives as a Team-tier feature.
- [[Enterprise feature candidates]] — feature #9 (dev-laptop cache sharing) depends on this.
