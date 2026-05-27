+++
title = "Decide how private crates get cached"

[extra]
type = "Task"
status = "todo"
priority = "medium"
tiers = ["2"]
tags = ["cache", "private", "tier-2"]
created = "2026-05-26"
+++

Public crates are clean: trusted-builder backfill fetches sources from crates.io / public git URLs directly. Customer uploads nothing. Tiny wire.

Private crates break that. The bytes aren't externally fetchable; the customer's tree is the only source. None of these is committed to yet:

- **Customer-uploaded chunks, demand-gated acceptance.** Customer's `cargo-vixen` content-addresses the read-set and uploads missing chunks to Store directly. Cache *acceptance* (the `key → output` mapping) is gated on some demand heuristic — only commit a private-cache entry if it would actually be reused. Risk: too noisy; per-commit binaries fill the cache with single-use artifacts.
- **Customer-local cache only.** Private crates never go to the shared cache. Cross-machine sharing within the org happens through normal cargo `target/` reuse + `actions/cache`. Risk: large orgs lose a real value prop (the dev-laptop-cache-sharing enterprise feature requires *some* shared private cache).
- **Tiered private cache with explicit thresholds.** Writes accepted only when a heuristic clears: hit count above N, stable semver release vs commit-sha-pinned WIP, customer-opted-in via configuration. Risk: complexity; each heuristic is a knob to tune.

What private-cache is mostly valuable for: popular internal deps, stable releases, cross-developer reuse on rebases.

What it's mostly *not* valuable for: WIP commits, feature branches, every-PR-builds-from-scratch flows.

Unblocks resolution: customer data from Tier 1. Once we see real demand-signal patterns from paying orgs, we can pick the heuristic — or learn there's no useful heuristic and private cache wants to be all-or-nothing per customer.
