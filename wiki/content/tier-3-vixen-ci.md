+++
title = "Tier 3 — Vixen CI"

[extra]
type = "Vision"
status = "living"
created = "2026-05-26"
updated = "2026-05-26"
tags = ["strategy", "vixen-ci", "tier-3", "compliance"]
+++

> The first tier where the customer writes Vixen.

Tier 2 hid the Vixen language behind a cargo-transparent surface. Tier 3 is where customers author `.vx` files — first for CI definition, eventually for whatever else they want to describe. This is the tier that makes the language a public API contract. See [[Vixen language stays internal until Tier 3]].

## What customers adopt

`.vx` files in the repository that describe CI: jobs, dependencies between them, status reporting, fly-check behaviour. Same evaluation engine as Tier 2; new authoring surface.

## What changes vs Tier 2

- **Customers write Vixen.** The language is exposed. Stability commitments start: semantic versioning, deprecation cycles, migration tooling, LSP, IDE support.
- **Customers install the `vx` CLI** (locally for fly-check, in CI for evaluation). We distribute it; we ship platform-specific instructions for it.
- **CI definition lives in the repo.** Not in `.github/workflows/*.yaml`. In `.vx` files versioned with the code they describe.
- **GitHub statuses API integration ships.** Vixen reports check status to GitHub (and equivalents for GitLab, Forgejo, etc.) by writing to the platforms' status APIs. The customer never edits a workflow YAML.

## The pitch

GitHub Actions is a glorified shell script with a YAML costume. Vixen CI is a signed, reproducible, attested distributed computation expressed in actual code. Same job, vastly more provable, vastly more programmable.

## Velocity story (engineer-to-engineer)

- **Instant fly-check on save.** The affected slice of the build/CI graph re-evaluates locally with sub-second latency. No push-and-wait.
- **No YAML.** CI logic is expressed in Vixen, the same language as the build description, with real types, real abstraction, real composition.
- **No runner agent.** No Azure agent registration, no ephemeral-VM lifecycle, no fighting a runner designed for generic CI.
- **Identical local and CI semantics, by construction.** Not a stated promise — an architectural property. "Works on my machine" stops being a meaningful failure mode.
- **Deterministic re-runs.** Every CI failure is a cache key; ship the key, anyone reproduces locally.
- **Local CI execution.** A developer can fire the same checks on their laptop that the central CI will run, against the same executor pool, with the same cache.

## Compliance story (the buyer-of-the-cheque)

- **One source of truth.** There is no separate "CI config" that can drift from "build config." Auditor reads one DAG.
- **No malicious-PR-modifies-CI vector.** In GitHub Actions, a contributor PR can edit `.github/workflows/*.yaml` and exfiltrate secrets. In Vixen CI, CI logic is part of the signed build graph; tampering with it is tampering with a signed artifact.
- **Reproducible CI failures.** Every failure is replayable from its cache key. The forensics trail is structural, not narrative.
- **Identical local & CI environment by construction.** Auditor cannot ask "what if your CI environment differs from local?" because the architecture says it can't.
- **CI environment attestation per run.** Every CI run carries the signed manifest of what executor, what toolchain, what sandbox.
- **Affected-only test execution from the build graph.** The DAG knows which tests depend on which code; rerunning only impacted tests is automatic and auditable.

## Tier 3 is not one product

The capabilities under the "you don't need GitHub Actions" umbrella aren't homogeneous. They split into at least four:

1. **Fly-check on save.** Pure dev velocity. Valued at every org size, small spend per seat.
2. **PR status reporting.** GitHub statuses API hook (and equivalents). Useful only if you use PRs. Operational cost on our side: GitHub App, webhooks, OIDC at the PR level.
3. **Build-event dashboards.** Streaming live view of executors. Valuable when builds fan out across multiple machines.
4. **Cross-team analytics.** Cost attribution, affected-tests selection, build-graph diffing across PRs. Only valuable when the org has multiple teams.

(1) is velocity at every scale. (2)–(4) are scale/compliance sells. We sell Tier 3 as the umbrella, price per-slice or bundled depending on the org.

## Pricing shape

Tier 3 is a separate tier (different SKU) but **same SKU, different price points** based on org complexity:

- A 4-person team buys Tier 3 for fly-check + maybe PR statuses. Small spend.
- A 500-person org buys for dashboards, cost attribution, audit surfaces. Large spend.
- "Vixen Enterprise" bundles Tier 1 + 2 + 3 — customers at that size always want everything.
- Standalone Tier 3 is purchasable so small teams can adopt the velocity story without committing to Tier 2's remote-execution.

## What's needed

Tier 3 is unlocked once Tier 2 has:
- A build graph rich enough to express test execution and arbitrary CI steps as nodes.
- Executor classes that can run test workloads.
- Cache + attestation infrastructure proven against production load.
- A polished local watcher / re-eval mechanism for the fly-check experience.
- The language stable enough to ship to external authors. This is the gating concern — see [[Vixen language stays internal until Tier 3]].

## Related

- [[Tier 1 — CCC]] — the on-ramp.
- [[Tier 2 — Vixen Build]] — the substrate.
- [[Vixen language stays internal until Tier 3]] — why this is the tier where the API contract starts.
- [[The build graph is also a provenance graph]] — what makes "the build is the CI" structurally true.
