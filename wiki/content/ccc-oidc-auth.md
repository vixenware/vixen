+++
title = "CCC authentication uses OIDC trusted publishing"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["ccc", "tier-1", "auth", "billing"]
+++

## Context

CCC needs to identify the customer making each cache request so we can authorise reads and bill writes against the right account. The conventional approach is a long-lived API token managed by the customer (stored as a secret, set as an env var in CI). Tokens are friction: customers have to provision them, rotate them, scope them, and clean up after leaks.

GitHub Actions ships a native alternative — OIDC — that PyPI has already proven viable for unattended publishing.

## Options considered

1. **Long-lived API tokens.** Customer creates a token at vixen.com, stores it as a GitHub Actions secret, passes it to CCC via env var. Familiar; high friction; long-lived; secret-rotation overhead.
2. **OIDC / trusted-publishing model.** Customer adds `permissions: id-token: write` to their workflow. The CCC fork requests a short-lived OIDC token from GitHub at runtime, presents it to the Vixen endpoint, we verify the issuer + audience + workflow + repo + ref claims and look up the associated billing account.

## Decision

OIDC trusted publishing. No long-lived tokens.

## Consequences

- **Zero secrets for the customer.** No token to create, store, rotate, or revoke. Adoption goes from "edit YAML, add a secret, reference it" to "edit YAML, add `permissions: id-token: write`."
- **Endpoint is baked into the cargo fork.** No URL, no env var, no config. The fork knows where to ask. Reduces adoption to the rustup mirror swap plus the OIDC permission.
- **Short-lived credentials.** OIDC tokens are minutes-scoped and verifiably tied to a specific workflow run, repo, ref, and (optionally) environment. A leaked token is useless after expiry.
- **Onboarding is a one-time web flow.** Customer visits vixen.com, links their GitHub org, we record `repo_owner=foo` (or finer-grained claims) → billing account mapping. Subsequent CI runs auto-authenticate against that mapping.
- **Provider-specific implementation.** The OIDC verifier needs per-CI-provider code: GitHub Actions today, GitLab CI / Buildkite / CircleCI / etc. later. We plan for that.
- **Self-hosted runners still work.** GitHub Actions OIDC tokens are minted by GitHub regardless of runner, so self-hosted runners get the same trust path.

## Related

- [[Tier 1 — CCC]]
- [[CCC is distributed as a rustup mirror]]
- [[Unauthenticated CCC requests miss but contribute demand signal]]
