# Rodin warm facts need derivations

Status: design pass only. No solver code changes in this note.

## Problem

Rodin's warm solve path can import learned no-goods, but not the proof graphs
that justified them. That violates the replay-before-trust model for cached
facts: a consumer of a cached fact is supposed to replay the trace before
accepting it (`docs/design/march-solver-recovered.md:101-105`,
`docs/design/march-solver-recovered.md:396-404`). It also cuts against the
Rodin ruling that every exclusion carries its derivation for proof, explanation,
and widening certificate (`docs/design/rodin.md:76-82`).

The practical symptom is in the current warm path: `Problem::warm_facts` is only
`Vec<LearnedNoGood>` (`crates/rodin-core/src/lib.rs:99-105`), the round-3 warm
measurement clones `cold.learned` into that field (`crates/rodin-core/src/lib.rs:1999-2008`),
and `install_warm_fact` overwrites the imported fact's original `DerivationId`
with a freshly appended empty `Resolve` derivation (`crates/rodin-core/src/lib.rs:940-953`).

## Current Shapes

`crates/rodin-facts` is deliberately the fact layer: plain data, append-only
stores, interval algebra, and replayable derivation structure, with search and
propagation left to Rodin proper (`crates/rodin-facts/src/lib.rs:1-5`).

Rodin's problem input currently carries roots, decrees, feature exclusions,
warm facts, and a learned-kind label; the constructor initializes `warm_facts`
to an empty vector (`crates/rodin-core/src/lib.rs:99-118`).

`SolveResult` carries both the learned facts and the `DerivationStore` that can
replay them (`crates/rodin-core/src/lib.rs:127-140`). `SolveError::NoSolution`
also returns learned facts and the derivation store (`crates/rodin-core/src/lib.rs:301-311`).
The current warm-fact transfer path uses only `cold.learned`, not
`cold.derivations` (`crates/rodin-core/src/lib.rs:1999-2021`).

The current learned fact is `LearnedNoGood { region, derivation, support,
point_versions, widened_versions, kind }` (`crates/rodin-core/src/lib.rs:280-287`).
Its `region` is a `Region` whose terms are package-version sets plus feature
polarities (`crates/rodin-facts/src/region.rs:13-17`). The region can test
assignments, test containment, subtract another region into a finite cover,
and test overlap (`crates/rodin-facts/src/region.rs:65-148`).

The exclusion store persists `Exclusion { region, derivation }` and assigns a
local `ExclusionId` on append (`crates/rodin-facts/src/exclusion.rs:3-27`).
It can find a stored exclusion that contains another region, and enumerate
exclusions overlapping a region (`crates/rodin-facts/src/exclusion.rs:45-58`).

The clause store persists Horn-ish `Clause { antecedents, consequent }` over
`Atom::InGraph`, `Atom::Ver`, and `Atom::Feat`; appending a clause also indexes
it by mentioned package and feature (`crates/rodin-facts/src/clause.rs:5-44`,
`crates/rodin-facts/src/clause.rs:62-78`).

The derivation store's local identity is `DerivationId(u32)`, and a derivation
is only `premises: Vec<Premise>` plus `rule: RuleKind` (`crates/rodin-facts/src/derivation.rs:5-37`).
The store owns a vector of derivations plus registries for known clauses,
exclusions, and pins (`crates/rodin-facts/src/derivation.rs:39-45`). Replay
starts from one local derivation id, follows premises, recursively follows
exclusion premises through the exclusion registry, checks pins and clauses by
membership, detects cycles, and only allows hypotheses below a conflict rule
(`crates/rodin-facts/src/derivation.rs:102-177`).

Solver construction seeds the derivation store with every compiled clause id
from the current index (`crates/rodin-core/src/lib.rs:919-938`). Warm fact
installation appends a new empty `Resolve` derivation, overwrites the imported
fact's derivation id with that local id, appends an exclusion for the fact's
region, registers that exclusion-to-derivation mapping, and pushes the modified
fact into `self.learned` (`crates/rodin-core/src/lib.rs:940-953`).

During propagation, learned facts are cloned, their vector index becomes the
local `ExclusionId`, and an applicable learned region either produces a conflict
premise or subtracts the excluded version set from the one remaining unsatisfied
package domain (`crates/rodin-core/src/lib.rs:1173-1222`).

The current learn path builds a conflict derivation from deduplicated conflict
premises plus the current hypothesis when present, then immediately replay-checks
that derivation under `debug_assert` (`crates/rodin-core/src/lib.rs:1461-1475`).
It derives widening support only from clause premises, ignoring exclusion, pin,
and hypothesis premises for support extraction (`crates/rodin-core/src/lib.rs:1818-1825`).
It shrinks the base region to packages mentioned by the support edges, falling
back to the hypothesis package only if the support-derived region is empty
(`crates/rodin-core/src/lib.rs:1477-1490`).

Widening is currently driven by the support edge set: `widen_region` compares
the selected edge state against every version of the parent package and inserts
the parent-version set whose edge state is identical (`crates/rodin-core/src/lib.rs:795-849`).
`support_packages` maps each support edge to its parent and dependency packages
(`crates/rodin-core/src/lib.rs:852-858`).

The learned fact currently points at the conflict derivation created before
widening, while the widened region and support counts are stored directly on the
fact (`crates/rodin-core/src/lib.rs:1492-1517`). The fact layer already has a
`RuleKind::Widen { certificate: WidenCert }`, but the current learn path does
not build a separate widen derivation node (`crates/rodin-facts/src/derivation.rs:11-31`,
`crates/rodin-core/src/lib.rs:1461-1517`).

The round-3 trace and evidence show the post-463a395b minimized shape: the
downgrade-cascade learned regions mention only the support packages, not the old
hostage packages (`docs/design/rodin-c-evidence.md:833-856`). The aggregate
evidence reports 13 learned no-goods, 13 point versions, 98 widened versions,
and a 7.538462x widening factor (`docs/design/rodin-c-evidence.md:820-831`).
The added test asserts the first downgrade-cascade fact's region packages equal
the support packages and replays that learned derivation (`crates/rodin-core/src/lib.rs:2949-2984`);
another test replays every learned no-good derivation for the pressure scenarios
(`crates/rodin-core/src/lib.rs:2988-3004`).

The record surface currently serializes fact id, kind, region, support edges,
widened counts, and summary; it does not serialize derivation graph material
(`crates/rodin-core/src/lib.rs:205-214`, `crates/rodin-core/src/lib.rs:2409-2430`).

## Design Constraints

Facts are monotone and keepable while hypotheses are retractable
(`docs/design/rodin.md:18-22`). Manifest-derived facts are scoped to the pinned
world, with no cross-user sharing for now (`docs/design/rodin.md:44-47`).
Widened range facts are verified, never interpolated (`docs/design/rodin.md:39-43`),
and a fact's validity range is its read-set stability range
(`docs/design/rodin.md:111-118`).

At the Vixen store layer, bytes are self-verifying, but claims need policy
(`docs/design/store-architecture.md:8-20`). Rodin's widened incompatibilities
are explicitly claims-with-evidence, shareable within trust domains under the
same policy machinery (`docs/design/store-architecture.md:51-58`).

The import design therefore needs two distinct properties:

1. It must preserve enough evidence to replay a warm fact without trusting the
   producer's local ids.
2. It must support policy-controlled sharing where the fact summary can move
   cheaply and the larger proof graph can be fetched only when policy requires
   replay.

## Proposed Import Shape

Replace the effective meaning of `Problem::warm_facts: Vec<LearnedNoGood>` with
a proof-bearing bundle. The Rust names below are sketches, not an implementation
request:

```rust
pub struct WarmFactBundle {
    pub scope: FactScope,
    pub facts: Vec<WarmLearnedNoGood>,
    pub proofs: WarmProofSource,
}

pub struct WarmLearnedNoGood {
    pub fact: LearnedNoGood,
    pub proof_root: ProofNodeId,
    pub proof_digest: ProofDigest,
}

pub enum WarmProofSource {
    Inline(ProofGraph),
    Lazy(ProofFetchRef),
}
```

`WarmLearnedNoGood::fact` is the operational pruning summary: region, support,
kind, and widening counts. `proof_root` is the proof identity for that summary.
`proof_digest` is the content-addressed identity that lets the consumer dedupe,
cache, and spot-audit the graph. `WarmProofSource::Inline` is the cheap
process-local path for counterfactual re-solves. `WarmProofSource::Lazy` is the
tenant/cache path where facts can be advertised first and proof graph material
is fetched only when the policy says replay is needed.

The proof graph should not expose raw `DerivationId`, `ClauseId`, or
`ExclusionId` as durable identity. Those are local vector indexes in the current
stores (`crates/rodin-facts/src/derivation.rs:5-6`,
`crates/rodin-facts/src/clause.rs:18-19`,
`crates/rodin-facts/src/exclusion.rs:3-4`). Instead, make the durable graph a
content-addressed DAG over stable premise witnesses:

- `ClauseWitness`: package/source identity, selected parent version, edge key,
  consequent kind, and normalized atom shape, enough to rebind to a local
  `ClauseId` in the current compiled index.
- `PinWitness`: root/decree/index-row/snapshot evidence, enough to rebind or
  reject under the current `FactScope`.
- `ExclusionWitness`: imported fact digest plus proof root, so recursive
  exclusion premises replay against their own imported graph rather than a raw
  producer-side `ExclusionId`.
- `HypothesisWitness`: the branch assumption package/version for explanation
  and replay shape, not a trusted global fact.
- `WidenWitness`: support edge set, selected edge state, widened parent-version
  coverage, and the pinned read-set/hash evidence that made the edge state
  stable over that coverage.

This can still lower into today's local `DerivationStore`: import allocates
fresh local derivation ids, fresh local exclusion ids, and registers local
clause/pin/exclusion premises after rebinding. But the durable proof identity is
the normalized graph digest, not any local id assigned during one solve.

## Import Algorithm

For each warm fact:

1. Scope-gate the fact before fetching large proof material. If the fact's
   `FactScope` does not match the current pinned world or trust policy, skip it.
2. Fetch the proof graph when needed. Inline graphs need no I/O. Lazy refs fetch
   by content digest from the claim namespace/CAS policy layer.
3. Verify the graph digest before interpreting it. Bad bytes are a content
   integrity failure; bad claims are a policy/replay failure.
4. Rebind stable premise witnesses to the current solve:
   - clauses are found by normalized clause witness, not producer `ClauseId`;
   - pins are checked against current roots, decrees, index hashes, and scope;
   - nested exclusion premises recursively import or verify the referenced fact;
   - hypotheses are admitted only under conflict rules, matching today's replay
     rule.
5. Replay the conflict proof from the imported root. For v0.1 this should be
   full replay on every imported fact used by a process-local warm solve.
6. Verify the widening witness separately, or normalize it as an explicit
   `RuleKind::Widen` node above the conflict proof. The verifier reruns the
   support-edge stability check over the pinned index rows and confirms the
   imported region does not exceed the certified coverage.
7. Install only after replay succeeds: allocate local ids, append the exclusion,
   register the exclusion-to-derivation mapping, and then push the fact into
   `self.learned`.

This changes warm import from "trust fact summary, invent derivation" to "verify
proof graph, then mint local ids for this solve."

## Full Replay Vs Spot Checks

Recommendation for the next solver change: full replay for all process-local
warm facts. The round-3 counterfactual latency story depends on reusing cold
facts inside the same pinned world, and those proof graphs are already resident
on `SolveResult`.

Spot checks belong later, at the store policy layer. The policy matrix can be:

- Own process / same solve family: inline proof graph, full replay before first
  use.
- Own tenant namespace: lazy graph fetch, full replay on first use per digest,
  then reuse the verified digest locally.
- Vendor-signed or public namespace: policy can require full replay, sampled
  replay, or recompute-from-pins before accepting the claim.

The important boundary is that spot-checking is a claim-namespace policy, not a
solver shortcut. The solver should receive either a verified imported fact or no
fact.

## Failure Policy

Default v0.1 behavior should be fail-closed per fact and continue cold:

- Proof unavailable: skip the fact for this solve.
- Scope mismatch: skip the fact; do not mark the producer bad.
- Digest mismatch or malformed graph: reject that proof digest and report a
  claim-integrity event.
- Replay failure: reject that fact digest, do not install the exclusion, and
  continue solving without it.
- Widen witness failure: either reject the whole fact or shrink to the
  replay-proven point region. This is an open decision because shrinking keeps
  useful knowledge but creates a new derived claim.

Namespace poisoning should be policy-controlled. A single bad fact digest should
not automatically poison a tenant namespace; repeated replay failures from the
same source should be enough for quarantine policy to trigger. Public/shared
namespaces should probably quarantine the exact claim digest first, then the
source only after multiple independent failures.

## Open Decisions For Amos

OPEN 1: First implementation shape. Should process-local counterfactual solves
carry `Inline(Arc<ProofGraph>)` directly on the problem, or should even the
local path exercise the lazy proof-fetch interface from the beginning?

OPEN 2: Durable premise identity. Should the durable graph be built from stable
witnesses as proposed above, or should v0.1 initially remap current local ids
under a documented "same `CompiledIndex` instance only" boundary?

OPEN 3: Widening node. Should imported proofs require an explicit
`RuleKind::Widen` node now, using the existing fact-layer type, or should the
warm verifier treat widening as a side certificate attached to the learned fact
until the learn path is refactored?

OPEN 4: Verification failure fallback. On a valid conflict proof but invalid
widen certificate, should import drop the fact entirely, or shrink it to the
point region proven by the conflict derivation?

OPEN 5: Policy surface. Should `Problem` carry unverified warm claims and let
`solve` verify them, or should a caller-facing `WarmFactVerifier` produce a
separate `VerifiedWarmFacts` value so the solver never sees unverified claims?

OPEN 6: Record/explain surface. Should `RecordFact` gain proof-root and
verification-status fields now, or should proof material remain outside the
round-3 JSON record until the trace/explain vocabulary is designed?

## Minimal Next Implementation Cut

After confirmation, the smallest faithful code path would be:

1. Add a proof-bearing warm-fact bundle that can carry the cold
   `DerivationStore` beside `Vec<LearnedNoGood>` for process-local re-solves.
2. Change the round-3 warm scenario to pass the cold proof store as well as
   `cold.learned`.
3. Change `install_warm_fact` to import and replay the original derivation
   graph before registering the local exclusion.
4. Preserve the current behavior for callers that provide no proof bundle:
   either reject those warm facts by default, or accept them only through an
   explicit "trusted/no-replay" policy gate if Amos wants that escape hatch.

That cut fixes the parked question without introducing persistent remote fact
sharing yet. The later persistent shape can replace the inline proof bundle with
content-addressed lazy proof fetches under the store claim policy.
