+++
title = "Unauthenticated CCC requests miss but contribute demand signal"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["ccc", "tier-1", "auth", "demand"]
+++

## Context

Once CCC adoption requires [[CCC authentication uses OIDC trusted publishing|OIDC authentication]], every request to the Vixen cache carries a verifiable customer identity — or doesn't. The question is what happens to requests that arrive without a recognised identity (anonymous probes, repos whose org isn't on file, mis-configured workflows).

Two opposing pulls:
- **Adoption funnel.** Free anonymous reads would let people try CCC without signing up.
- **Cost discipline.** Anonymous reads are infrastructure spend with no billing path. The cache is non-trivial to host.

## Options considered

1. **Anonymous reads, no writes.** Anyone can hit the cache; only authenticated customers contribute. Best funnel; uncapped cost.
2. **Rate-limited free tier with per-IP / per-anonymous-actor caps.** Compromise. Operational complexity (rate-limiter, abuse mitigation, free-vs-paid quality differentials).
3. **Hard auth — unauthenticated requests miss across the board.** No anonymous cache hits; adoption is gated.

## Decision

Option 3. Unauthenticated requests get cache misses across the board. No free tier today.

A free tier is deferred — revisit when grant or VC funding underwrites the infrastructure.

## Consequences

- **Adoption requires sign-up.** Customer has to link their GitHub org with Vixen before CCC delivers any value. The CCC binary itself can install freely (it's part of the rustup mirror toolchain) but the cache hits don't materialise until the org is on file.
- **Anonymous traffic still informs us.** A miss is a miss for the requester, but on our side we *log the missed key* as a demand signal. The unauthenticated population is unprivileged but informative — it tells us what people are trying to build, which feeds the trusted-builder work queue. See [[Demand-warmed cache]].
- **No cost-blowup vector.** The cache only serves bytes to identities we can bill (or have decided to subsidise). Storage and bandwidth scale with paying customers, not with adoption attempts.
- **Free-tier introduction is reversible.** If we later add anonymous reads (under funding), this Decision gets superseded by one that defines the free-tier shape. The OIDC infrastructure stays; we just relax the read-side gate.

## Related

- [[CCC authentication uses OIDC trusted publishing]]
- [[Demand-warmed cache]]
- [[Trust is per-cache, not global]]
