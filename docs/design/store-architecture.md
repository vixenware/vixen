# The store: tiers, tenants, trust

*Banked from Amos's framing (2026-07-04): the CAS is multi-tiered,
multi-layered, untrustworthy-by-default; enterprise customers host
their own; companies pay for separate tenants. This note records the
decomposition that makes all of that one design instead of five.*

## The split content addressing gives us for free

**Bytes need no trust.** Flesh is self-verifying: request hash H,
hash what arrives, compare. A hostile tier can withhold or serve
garbage that fails verification; it cannot lie successfully.

**Claims need policy.** What can lie is the mapping layer:
"invocation identity I yields output O" (memo claims), "this fetch
observed checksum S" (observation pins), "this toolchain has
fingerprint F". That layer is the journal/spine —
"untrustworthy-by-default" is a property of CLAIMS, never of the
CAS proper, whose only obligations are integrity (free) and
availability (product tiering).

## Tiers = residency of flesh

local disk → LAN peers → tenant-regional → origin/public. Content
hash is the universal locator; misses walk tiers; the spine
(identities) travels first and decides which flesh is worth
pulling. Nix substituters at per-call granularity, with
peer-to-peer tree movement as ruled in the fleet design.

## Tenants = claim namespaces + storage isolation

A build consults an ORDERED SET OF CLAIM SOURCES UNDER POLICY:
- trust outright (own tenant's journal),
- verify-by-sampling (vendor-signed claim sets — e.g. toolchains —
  crossing tenants deliberately),
- recompute-from-pins (the audit posture).
Enterprise self-host = run a tier + own the namespace. The public
tier is the namespace with the weakest default policy. "Lockfile =
journal slice" (C5's surviving delta) is per-tenant product
surface: what a customer pins IS a slice of a claim namespace
they chose to trust.

## The attack and the stack

Cache poisoning is THE attack of this business. Defense in depth,
all layers already designed: hash-verified bytes (free, structural),
attestation-grade claims (signed, per-entity trust policy — gg's
draft assertions industrialized; Trustix adjacency adjudicated in
the prior-art doc), recompute-from-pins spot-audits.

## Rodin's facts are claims too

A widened incompatibility justified by observed read-set stability
(R2, the crown-jewel claim) is a claim-with-evidence. Learned
regions are therefore shareable WITHIN TRUST DOMAINS under the same
policy machinery — a fleet's accumulated solver knowledge is a
tenant asset exactly like its build cache. Same spine, third
customer (after memo reuse and supply-chain audit).

## Three-headed public tier (Amos musing, banked with verdict)

Idea: run the public tier as three redundant heads fed by
independent computations; publish an incident report on divergence.
VERDICT: as security, it buys only what head INDEPENDENCE buys —
same-operator heads share a fate, and consensus is garbage-in/
garbage-out against source-level or shared-toolchain attacks; the
version with teeth is organizationally independent heads (=
Trustix/rebuilders, see prior-art doc). BUT as DETERMINISM QA it is
cheap and valuable: divergence on one identity means a builder
fault or — the interesting case — a LYING IDENTITY (uncaptured
read, role gap, tier-2 modeling hole), i.e. continuous production
verification of identity⇒output, the invariant all caching rests
on. Implementation is a POLICY CONFIG over existing machinery:
three claim namespaces + equality rule + alert, sampled
recomputation not 3x-everything, agreement dedupes to zero extra
storage. CT-style "we publish divergence incidents" is a
transparency posture worth having independent of the security
story.
