+++
title = "Package publish uses OIDC trusted publishing only"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["packaging", "auth", "security"]
+++

## Context

cargo/crates.io's model is "authenticated account holds a long-lived token, anyone with the token can publish." PyPI's modern evolution adds **trusted publishing via OIDC** — publishing is tied to a verifiable CI workflow declaration. The publishing mechanism we [[CCC authentication uses OIDC trusted publishing|already committed to for CCC auth]] is OIDC; reusing it for package publish keeps the auth surface coherent.

## Decision

Package publish to the central Vixen index is **OIDC trusted publishing only**. No long-lived API tokens. To publish, you set up your CI to mint an OIDC token, present it to the index, and the index verifies the workflow + repo + ref claims map to the package you're publishing to.

## Consequences

- **No long-lived secrets to leak.** A compromised CI token is minutes-scoped and tied to a specific workflow run. Mass-republish-attack via leaked token isn't possible.
- **Every publish has verifiable provenance.** "Published from `bearcove/vixen-rust@v2.4.1` workflow run `xyz`, verified by GitHub OIDC at time T" is recorded with the index entry. Auditable.
- **Dogfooded from day one.** During early/rapid iteration the Vixen team publishes via the same OIDC flow customers eventually will. No "internal publish path" that hides bugs in the trusted-publishing setup.
- **Higher friction on first publish.** A maintainer has to set up CI before they can publish — they can't just `vixen pkg publish` from their laptop with a token. The friction is borne once per package; subsequent publishes are workflow-triggered.
- **GitHub-and-equivalents only at v1.** OIDC verifier ships per-CI-provider. GitHub Actions on day one; GitLab CI, Buildkite, CircleCI follow as customers demand them. Self-hosted GitHub Actions runners work because GitHub mints OIDC tokens regardless of runner location.
- **Manual local publish is a deliberate non-feature.** If a maintainer wants to publish from their laptop, they push to a branch wired to a publish workflow. The workflow is the publish surface, not the maintainer's local machine.

## Related

- [[CCC authentication uses OIDC trusted publishing]]
- [[Package management for Vixen]]
- [[Store gains a retention dimension for packages]]
