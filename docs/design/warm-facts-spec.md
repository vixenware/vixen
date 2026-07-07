# Warm facts spec — proof-bearing, premise-keyed, replay-gated

Status: design pass only. No `rodin.vix` changes, no Rust. This document
satisfies the hard gate recorded in `facet-cc NEXT.md`: **before any
warm / persistent / cross-solve fact work (the restart-with-retained-facts
line), facts must grow the adjudicated shape.** The corpus audit
(`corpus-match-audit-2026-07-07.md:99-116`, `:195-203`) found the landed vix
learning is an in-session prototype — `LearnedNoGood { region, active }` —
below that bar. The three `CONTRADICTS-ADJUDICATION` findings this spec closes:

1. **Declared-structure widening treated as R2** — `rodin.vix:904-971` widens by
   `clause_same_effect`, no observed read-set (`corpus-match-audit:105`).
2. **No premise-keyed support / absence keys** — `LearnedNoGood` is only
   `region, active` (`corpus-match-audit:106`).
3. **No warm-fact / proof semantics** — `learn()` installs a naked region; no
   derivation, proof digest, or verification status (`corpus-match-audit:110`).

The one production target is the **vix** implementation
(`CLAUDE.md`, `rodin-in-vix.md`); cargo is the oracle. The Rust type sketches in
`rodin-warm-facts-derivations.md` are the normative *shapes*; this spec ports
them to the vix fact layer and names the delta against the landed prototype.

Every design element below cites the corpus adjudication it inherits. Nothing
here is novel invention — it is the paid-for adjudications assembled into one
buildable shape, plus a labeled stage plan and the explicit R2 boundary.

---

## 0. Terminology map (match the corpus exactly)

| Term | Origin | Meaning here |
| --- | --- | --- |
| `LearnedNoGood { region, active }` | prototype `rodin.vix:150-154` | Stage-0 operational pruning summary. The degenerate warm fact. |
| Premise-keyed support | `adjudication.md:35-44`, `:66-75` | The set of premises a fact watched = the fact's read-set. Recorded through **one choke point**; under-recording support is THE soundness bug. |
| Absence premise / absence key `(P, ¬Z)` | `adjudication.md:42-44` | First-class subscribable premise: "no version of P in range Y." A publish into Y probes the absence index. |
| `RuleKind::Widen { certificate: WidenCert }` | `rodin-facts` fact layer, cited `rodin-warm-facts-derivations.md:93-95`, RULING 3 `:314-317` | A widened fact's proof must prove the **widened** claim, as a real proof node — not the narrower point. |
| `WidenCert` | prior-art `:448-452` | In-fact certificate; today a hash of declared `EdgeSignature` equality. Stage-preserved. |
| `WidenWitness` | `rodin-warm-facts-derivations.md:185-186` | The durable-graph node: support edge set + selected edge state + widened coverage + pinned read-set/hash evidence. |
| `WidenEvidence::ObservedReadSet {read_set_digest, snapshot_id}` | prior-art `:465-484` | The novel R2 mechanism. **OUT OF SCOPE here** (§5). |
| `WarmFactVerifier` → `VerifiedWarmFacts` | `rodin-warm-facts-derivations.md` RULING 5 `:318-320` | Verifier lives OUTSIDE the solver; `solve()` accepts only the verified type. Unverified facts are a compile error, not a runtime check. |
| `WarmFactBundle` / `WarmProofSource` (`Inline` / `Lazy`) | `rodin-warm-facts-derivations.md:140-156`, RULING 1 `:308-310` | Proof-bearing import bundle. `Inline` ships now; `Lazy` stays a declared, unimplemented variant. |
| `FactScope` | `rodin.md:47-50` (item 9), `rodin-warm-facts-derivations.md:141` | Validity scope: the pinned index-file hashes consulted. No cross-user sharing for now. |
| `ProofDigest` / `ProofNodeId` | `rodin-warm-facts-derivations.md:146-171` | Content-addressed proof identity. In vix, free from the demand machine. |
| `ClauseWitness`, `PinWitness`, `ExclusionWitness`, `HypothesisWitness` | `rodin-warm-facts-derivations.md:172-190` | Durable, stable premise witnesses — never raw local `DerivationId`/`ClauseId`/`ExclusionId`. |

---

## 1. The fact shape

The gate is a shape gate. The stage-0 prototype (`rodin.vix:150-154`) carries a
`Region` and an `active` flag and nothing else. The adjudicated shape adds four
things: **premise-keyed support** (including absence premises), a **proof
digest**, a **widening witness where applicable**, and a **validity scope**.

Expressed in the vix fact layer (names are sketches, following the Rust sketches
in `rodin-warm-facts-derivations.md:140-190`; the durable identity is the
content-addressed graph, never a local vector index — `:166-171`):

```
struct WarmFact {
    fact:         LearnedNoGood,       // region + active — the stage-0 pruning summary
    support:      [Premise],           // premise-keyed support = this fact's read-set
    proof_root:   ProofNodeId,         // identity of the derivation that justifies `region`
    proof_digest: ProofDigest,         // content-addressed identity (free from the demand machine)
    widen:        Option<WidenWitness>,// present iff `region` was widened past its point
    scope:        FactScope,           // validity scope: the pinned index-file hashes consulted
}
```

### 1.1 Premise-keyed support — the read-set (`adjudication.md:35-44`, `:66-75`)

`support` is the fact's read-set: every premise the refuting derivation
consumed. The adjudication's ruling build sequence puts this **first** (step 1,
`:66-70`): *premise-keyed support (instrumented traces, absence keys, inverted
index premise→facts)*. The classic soundness bug is **under-recorded support**
(`:38-42`); the defense is that all reads go through **one choke point** and
replay re-reads through that same traced interface — otherwise the fact and its
proof silently agree and are both wrong (`:38-42`).

```
enum Premise {
    Clause(ClauseWitness),         // parent version + edge key + consequent shape (rodin-warm-facts-derivations.md:172-174)
    Pin(PinWitness),               // root/decree/index-row/snapshot evidence (:175-176)
    Exclusion(ExclusionWitness),   // nested imported-fact digest + proof root (:177-179)
    Hypothesis(HypothesisWitness), // branch assumption pkg/version (:180-181)
    Absence(AbsenceWitness),       // "no version of P in range Y" — first-class (adjudication.md:42-44)
}
```

`ClauseWitness` follows `rodin-warm-facts-derivations.md:172-174`:
package/source identity, selected parent version, edge key, consequent kind,
normalized atom shape — enough to rebind to a local `ClauseId` in the current
compiled index without trusting the producer's index. Optional deps are
**guarded implications** (`guard → req`), never bare edges, and the
feature-unification context (target/kind, weak `dep?/feat`) is part of the
premise identity (`adjudication.md:93-95`).

### 1.2 Absence as a first-class subscribable premise (`adjudication.md:42-44`)

This is the load-bearing addition the prototype lacks entirely. A fact whose
derivation observed "P has no version in range Y" (a closed-world premise:
version-universe enumeration, "feature F does not exist", "P@v has no dep Z")
must record an `AbsenceWitness(P, Y, evidence)` and be indexed under the
**absence key `(P, Y)`** in an inverted `premise → facts` index. A publish into
Y then probes the absence index directly (`adjudication.md:42-44`,
invalidation in §3).

Closed-world premises are exactly the facts entailment-monotonicity does **not**
protect (`adjudication.md:23-34`): dep-added and feature-map additions are
purely-additive edits that FALSIFY them. Because Rodin resolves features as
propagated literals, any fact that forced a feature literal false via
non-existence has a closed-world premise; **negative feature literals in the
support set are the danger markers** (`:31-34`) and the `Premise` enum must be
audited kind-by-kind (`:30-33`). The absence key is the mechanism that makes
that audit a subscription, not a re-solve.

### 1.3 Proof digest and the widening witness

`proof_root` / `proof_digest` give the fact its content-addressed proof identity
(`rodin-warm-facts-derivations.md:146-171`). The proof graph is a
content-addressed DAG over **stable premise witnesses**, not local ids
(`:166-190`) — because index-in-durable-artifact is the `fn_ref` bug class,
caught in vix the same day (RULING 2, `:311-313`).

`widen` is present iff the region was widened past its point. Widening becomes a
**real `RuleKind::Widen` proof node** whose certificate is a `WidenCert`
(RULING 3, `:314-317`): a widened fact's proof must prove the *widened* claim,
not the narrower point. `WidenWitness` carries support edge set, selected edge
state, widened parent-version coverage, and the pinned read-set/hash evidence
that made the edge state stable over that coverage
(`rodin-warm-facts-derivations.md:185-186`). Today that evidence is
declared-structure (byte-equal `EdgeSignature`, prior-art `:448-452`); the
observation-tied form is R2 (§5).

Per-edge certs COMPOSE — the meet of two facts' certs is the cert of anything
derived from both — so `Derived` nodes get certs for free during replay
(`adjudication.md:89-92`). The cert form is `(must-exist, upper-bound
VersionSet)` per supporting edge plus absence keys (`:89-91`).

### 1.4 Validity scope (`rodin.md:47-50`, item 9)

`FactScope` is the pinned world: facts derived from manifests are keepable
scoped by the **index-file hashes consulted**, no sharing across users for now
(`rodin.md:47-50`). A fact's validity range is its read-set's stability range
(`rodin.md:113-116`, `rodin-c-evidence` invariant restated). This is the scope
gate's key (§2).

---

## 2. The lifecycle

Five phases. Today's prototype implements only the first, in its degenerate
form. The **restart-with-retained-facts line** is crossed between *persist* and
*fetch*: the moment a fact outlives the process that learned it, it must be
proof-bearing and it must be replayed before install.

```
  learn  ──►  persist  ──►  fetch  ──►  REPLAY  ──►  install | reject
 (in-session,  (only proof-  (under      (before      (fail-closed
  cheap)        bearing)      scope gate)  install)     per fact)
```

### 2.1 learn — in-session, cheap (today's shape is the degenerate case)

The stage-0 prototype IS this phase, minus the recorded support and proof:
`learn()` builds a point region from conflict support, widens it, and installs
(`rodin.vix:961-967`). Learning stays cheap and in-session; the only growth is
that `learn()` now **records** the premises it already touches (it computes
`conflict_support` transiently at `rodin.vix:812-818` and throws it away) and
attaches the conflict derivation as a proof node. No I/O, no scope gate, no
replay for facts that never leave the process — an in-session fact used only by
its own solve is trivially replay-valid against the world it was learned in.

### 2.2 persist — only proof-bearing facts (`rodin-warm-facts-derivations.md:242-259`, RULING `:326`)

A fact may be persisted only if it carries `support`, `proof_root`,
`proof_digest`, and (where widened) a `WidenWitness`. **No-proof facts are
rejected by default** (RULING, `:326`); a "trusted / no-replay" escape hatch
exists only as an explicit policy gate (`:298-300`). Persistence changes
residency, not the proof obligation — the receipt travels with the fact (§3,
receipt analogy from `vx-store-as-vix-memo.md:255-268`).

### 2.3 fetch — under the scope gate (`rodin-warm-facts-derivations.md:192-201`)

Scope-gate the fact **before** fetching large proof material (`:194-198`,
step 1). If the fact's `FactScope` does not match the current pinned world or
trust policy, skip it — cheaply, no proof I/O, and do **not** mark the producer
bad (failure policy, `:245-247`). `WarmProofSource::Inline` needs no I/O;
`WarmProofSource::Lazy` fetches by content digest from the claim namespace / CAS
policy layer (`:199`). Per RULING 1 (`:308-310`), `Inline(Arc<ProofGraph>)`
ships now; `Lazy` is a declared, unimplemented variant — the seam ships, the
mock does not.

### 2.4 REPLAY — before install (`rodin-warm-facts-derivations.md:202-213`, `:222-227`)

This is the gate's teeth. For v0.1: **full replay on every imported fact** used
by a process-local warm solve (`:223-226`). The steps (`:202-213`):

1. **Verify the graph digest** before interpreting it. Bad bytes = content
   integrity failure; bad claims = policy/replay failure (`:202-203`).
2. **Rebind stable premise witnesses** to the current solve (`:204-209`):
   clauses found by normalized `ClauseWitness`, not producer `ClauseId`; pins
   checked against current roots/decrees/index hashes/scope; nested exclusion
   premises recursively import or verify the referenced fact; hypotheses
   admitted only under conflict rules. No local-id remap (RULING 2, `:311-313`).
3. **Replay the conflict proof** from the imported root, re-reading through the
   same traced choke-point interface the fact was learned through
   (`adjudication.md:38-42` — otherwise fact and proof silently agree and are
   both wrong).
4. **Verify the widening witness separately**, as an explicit `RuleKind::Widen`
   node above the conflict proof: rerun the support-edge stability check over
   the pinned index rows and confirm the imported region does not exceed the
   certified coverage (`rodin-warm-facts-derivations.md:210-213`).

### 2.5 install | reject — fail-closed per fact (`rodin-warm-facts-derivations.md:214-219`, `:242-259`)

Install only after replay succeeds: allocate local ids, append the exclusion,
register the exclusion-to-derivation mapping, then push the fact into the
learned set (`:214-217`) — the same containment-frontier install/deactivate the
prototype already does (`rodin.vix:918-959`). Reject paths (`:245-253`):

- Proof unavailable / scope mismatch → **skip** the fact, continue cold.
- Digest mismatch / malformed graph → **reject** that proof digest, report a
  claim-integrity event.
- Replay failure → **reject** that fact digest, do not install, continue cold.
- Widen-witness failure on an otherwise-valid conflict proof → **shrink to the
  replay-proven point region**, minted as a NEW fact whose derivation is the
  conflict proof alone (fully verified, ours to claim) — RULING 4 (`:314-317`).

Namespace poisoning is policy-controlled: **quarantine the exact claim digest
first**, the source only after multiple independent failures (`:255-259`). The
solver receives either a verified imported fact or no fact — spot-checking is a
claim-namespace policy, never a solver shortcut (`:228-239`). `solve()` accepts
only `VerifiedWarmFacts` (RULING 5, `:318-320`).

---

## 3. The invalidation story — receipt-driven, mechanical

**The receipt analogy is exact.** `vx-store-as-vix-memo.md:255-268`: a tier-2
build output "carries the observed read set. On lookup, open vix reruns the
existing `verify()` logic against the current world before accepting the output
… Persistence changes residency; it must not change the proof obligation." A
warm fact is the solver's tier-2 record: **its premise-keyed support IS its
receipt**, and the `WarmFactVerifier`'s replay IS the fact's `verify()`.
**Learned facts need receipts like builds need receipts** — a fact without
support/proof is a build output without a read-set, and both are rejected by
default (`store-architecture.md:45-58`: Rodin's facts are claims-with-evidence,
shareable within trust domains under the same policy machinery as the build
cache — "same spine, third customer").

This ties the invalidation directly to the machine's demand/observation model:
the fact's `support` is a set of reads; a premise change is an observation that
one of those reads is no longer stable; the inverted `premise → facts` index
turns each change into a subscription probe, not a re-solve. Mechanically, by
premise change:

| Premise change | Probes | Kills | Rule |
| --- | --- | --- | --- |
| **Publish** (new version of P into range Y) | Absence index `(P, Y)` where `Y ∩ {new} ≠ ∅` | Every fact whose support has `Absence(P, Y')` with `Y' ∩ {new} ≠ ∅`; and any fact that forced a feature literal false via P's non-existence | Closed-world premise falsified by additive edit (`adjudication.md:23-34`). The 134 feature-map co-occurrences are the danger zone (`:34`). |
| **Yank** (remove version P@v) | — | **Nothing.** Yank only shrinks the universe; it never adds a satisfying assignment | Entailment monotonicity: `F ⊨ C ⇒ F∪G ⊨ C`, and shrinking the domain preserves no-goods (`adjudication.md:16-22`). A yank can make a fact *inapplicable*, never *unsound*. |
| **Manifest edit — dep added** | Absence index for the added edge Z | Facts whose support has `Absence(P@v, ¬Z)` | Purely-additive edit falsifying a closed-world premise (`adjudication.md:23-34`, `:42-44`). |
| **Manifest edit — dep narrowed** (`^0.x.y → ^0.x.(y+1)`) | The narrowed edge's `ClauseWitness` used-region | Facts whose used region on that edge changed; **survives** if `O∩UsedSet == N∩UsedSet` | Per-operand harmlessness (`adjudication.md:79-84`); the 417 narrowings are no-ops under declared transport (`:96-100`) — but transport itself is Stage 4 (§4). Absent transport, a changed used-region invalidates conservatively. |
| **Index refresh** (new index-file hash) | `FactScope` (pinned index-file hashes) | Facts whose `FactScope` no longer matches | Scope gate (`rodin.md:47-50`, `rodin-warm-facts-derivations.md:194-198`). This is the coarse outer gate; premise probes are the fine inner ones. |

The distinction that makes this sound: **monotonicity protects consequences of
positive clauses; it does NOT protect closed-world premises**
(`adjudication.md:23-34`). Publish and dep-added are the only two changes that
falsify facts additively, and both are caught by the absence index. Everything
else is either monotone-safe (yank), scope-gated (index refresh), or handled by
the declared-transport stage (narrowing). Byte-change is not semantic change:
the fact store is a memo store keyed by observed reads, invalidated by semantic
change (`adjudication.md:118-128`).

---

## 4. The delta plan — stages against the landed prototype

Labeled stages, incremental. **Stage 0 is the landed in-session prototype,
preserved.** Each stage is a shape the fact can grow into without discarding the
previous one; the prototype's containment-frontier install/apply machinery
(`rodin.vix:918-1121`) is untouched throughout — only the fact's payload grows.

### Stage 0 — in-session prototype (LANDED, preserved as the degenerate case)

`LearnedNoGood { region, active }` (`rodin.vix:150-154`); `learn()` builds a
point region from `conflict_support` (`:812-818`, `:820-829`), widens it via
declared-structure `clause_same_effect` (`:848-856`, `:886-916`), and installs
with region-containment subsumption (`:918-959`). No support recorded, no proof,
no persistence. **This is the degenerate `learn` phase (§2.1) and stays valid
for facts that never leave their solve.** Nothing here is deleted.

### Stage 1 — premise-keyed support (grow `LearnedNoGood` → `WarmFact.support`)

Promote the transient `conflict_support` (`rodin.vix:812-818`) from a
point-region seed into a **stored `[Premise]`**. Emit `ClauseWitness` /
`PinWitness` / `HypothesisWitness` for the premises already consumed, and add
`AbsenceWitness` capture at the point the solver observes an empty candidate set
(the `candidates_for` path). Route all index reads through **one choke-point
accessor** so support is never under-recorded (`adjudication.md:35-44`). Build
the inverted `premise → facts` index including the absence keys. Still
in-session; no persistence. This alone closes CONTRADICTS finding #2
(`corpus-match-audit:106`).

### Stage 2 — proof-bearing facts (add `proof_root`, `proof_digest`, `RuleKind::Widen`)

Build the conflict derivation as a real proof node and widening as a real
`RuleKind::Widen { certificate: WidenCert }` node (RULING 3, `:314-317`) — a
widened fact's proof proves the widened claim. `proof_digest` = the demand
machine's content hash (content-addressing is free on the vix machine, per
`CLAUDE.md`/`rodin-in-vix.md`). Facts now *can* be persisted because they carry
proof. Closes CONTRADICTS finding #3 (`corpus-match-audit:110`). The declared
`WidenCert` stays declared-structure (prior-art `:448-452`) — closing finding #1
(`corpus-match-audit:105`) means *labeling* it declared-per-edge widening, NOT
R2 (prior-art `:453-463`).

### Stage 3 — restart-with-retained-facts (`WarmFactVerifier` + `VerifiedWarmFacts`)

**This is the gated line.** Add `WarmFactBundle` / `WarmProofSource`
(`Inline` now, `Lazy` declared-unimplemented — RULING 1) and the
`WarmFactVerifier` that runs the §2.3–2.5 import algorithm OUTSIDE the solver,
producing `VerifiedWarmFacts`; `solve()` accepts only that type (RULING 5,
`:318-320`). Full replay v0.1, fail-closed per fact + continue cold,
shrink-to-point on widen-cert failure (RULING 4), quarantine claim-digest before
source, no-proof rejected by default (RULINGS, `:323-326`). After Stage 3, a
fact learned in one solve can be retained across a restart and reused in the
next — verified, not trusted.

### Stage 4 — declared monotone transport (in scope, later)

The replay-gated semantic lift for narrowing manifest edits: per-premise
classifier (equal / stronger / weaker-or-incomparable / absent), with **one new
proof rule — VersionSet WEAKENING** (enlarge a set appearing positively) so old
proofs replay under narrowed premises by prefixing a weakening step
(`adjudication.md:66-75` step 3, `:85-88`). This makes the 417 common narrowings
no-ops (`:96-100`). Declared-structure only. Residuals last, behind the replay
gate, if ever (`:75`, `:53-56`). This is **not** R2 — it is
declared-per-edge/entailment transport, which is real but not the novel claim
(prior-art `:453-463`).

---

## 5. Explicitly OUT of scope — R2 observed-read-set learning

**R2 — widening a learned incompatibility justified by VERIFIED read-set
stability of OBSERVED reads — is deliberately not designed here.** This is the
crown-jewel novel claim (prior-art `:465-484`, adjudication `:118-128`), and it
is real in **rodin-core (Rust)** as `WidenEvidence::ObservedReadSet
{read_set_digest, snapshot_id}` (prior-art `:465-484`). It is **not** in the vix
production implementation, and this spec does not build it there.

**The named dependency (do not design it here):** R2 in vix requires
**`ObservedReadSet` plumbing that does not exist** — per-decision / per-premise
**field-level** read tracking (the `ReadSet` / `FieldPath` shape: which index
FIELDS the solver consumed — `actual` / `kind` / `optional` / `req` — as opposed
to which *edges*, prior-art `:465-484`), captured through the choke-point
accessor during solve, plus the `WidenEvidence::ObservedReadSet` variant on the
widening witness. The boundary is sharp:

- **In scope (Stages 1–4):** *edge-level* premise-keyed support +
  *declared-structure* widening (`clause_same_effect` / byte-equal
  `EdgeSignature`). Records WHICH premises a fact watched.
- **Out of scope (R2):** *field-level* observed read-set widening — collapsing
  neighbor versions that differ only in **unconsumed** fields (the killer
  scenario: cosmetically-different manifests with identical consumed read-sets,
  where declared equality CANNOT widen but read-set stability CAN, prior-art
  `:478-483`). Records WHICH FIELDS within a premise were actually consumed.

The two compose (`adjudication.md:58-65`): premise-keyed support decides which
premises a fact watches; the observed read-set decides which changes to those
premises are harmless at field granularity. Stage 1's edge-level support is the
substrate R2 later refines to field granularity — but only once the
`ReadSet`/`FieldPath` choke-point plumbing lands. Naming that dependency is this
section's whole job; designing it is a separate charter.

---

## 6. Open choices (adjudications leave these genuinely open for the vix port)

The `rodin-warm-facts-derivations.md` OPEN 1–6 were all RULED (`:306-326`) and
are inherited as decided (§2). What those rulings do **not** settle is how they
lower onto the vix demand machine specifically. Three real choices, each with a
recommendation in the agreed direction:

**C1 — Proof residency in vix.** RULING 1 mandates `Inline(Arc<ProofGraph>)`
now. In vix, the demand machine is content-addressed and memoized for free, so
the proof graph could either be (a) a standalone inline `ProofGraph` struct
carried on the problem, mirroring the Rust sketch, or (b) demand-native — the
"proof" is the memoized derivation demand subtree and `proof_digest` is just its
content hash, so replay is re-demanding the subtree. **Recommend (a) for
Stage 3** (faithful to RULING 1, smallest delta, keeps the verifier outside the
machine), and treat (b) as a Stage-4+ evolution once the derivation itself is a
first-class vix demand. Both ship the same seam; (b) is strictly an internal
substitution later.

**C2 — Absence-premise encoding.** The adjudication mandates absence be
first-class and subscribable (`:42-44`) but not its vix representation. Either
(a) a distinct `Premise::Absence(AbsenceWitness)` variant indexed under an
explicit absence key `(P, Y)`, or (b) fold absence into an existing
version-universe read premise carrying an empty observed `VersionSet`.
**Recommend (a):** the adjudication is emphatic that absence is a *first-class
subscription* and that negative literals are the danger markers to audit
kind-by-kind (`:31-34`, `:42-44`); a distinct variant makes the audit and the
publish-probe mechanical rather than a special case of a general read.

**C3 — Support-extraction granularity at Stage 1.** Today `conflict_support`
collects only `(conflict pkg, guard parent pkg)` (`rodin.vix:812-818`). Either
(a) record support by reconstructing it from clause premises as today, or (b)
instrument every index read through a single traced choke-point accessor so
support is captured as it is consumed. **Recommend (b):** the adjudication names
under-recorded support as THE soundness bug and requires replay to re-read
through the *same* traced interface (`:38-42`); (b) is also the exact substrate
R2's field-level `ReadSet` later refines (§5), so building the choke point once
serves both. (a) risks the fact and proof silently agreeing and both being
wrong — the failure mode the adjudication specifically warns against.

---

## Citation index (every inherited element)

- **Premise-keyed support, choke point, under-recording bug, build sequence** —
  `rodin-fact-validity/adjudication.md:35-44`, `:66-75`.
- **Absence as first-class subscription; closed-world premises; negative-literal
  danger markers; 134 feature-map danger zone** — `adjudication.md:23-34`,
  `:42-44`.
- **Entailment monotonicity (yank safe, no-goods preserved)** —
  `adjudication.md:16-22`.
- **Per-operand harmlessness `O∩UsedSet == N∩UsedSet`; VersionSet WEAKENING
  rule; composing certs; guarded implications** — `adjudication.md:79-95`.
- **417 narrowings no-op under transport; memo store keyed by observed reads** —
  `adjudication.md:96-100`, `:118-128`.
- **Import shape (`WarmFactBundle`, `WarmProofSource`, witnesses); import
  algorithm; full replay; failure policy; six RULINGS** —
  `rodin-warm-facts-derivations.md:140-190`, `:192-219`, `:222-259`, `:306-326`.
- **Declared-per-edge widening real but NOT R2; R2 built in rodin-core as
  `WidenEvidence::ObservedReadSet`** — `prior-art-adjudication.md:439-484`.
- **Three CONTRADICTS findings; below-the-bar prototype; required-reading** —
  `corpus-match-audit-2026-07-07.md:99-116`, `:195-203`.
- **Facts monotone/keepable; range facts verified; fact scoping; validity range
  = read-set stability range; every exclusion carries its derivation** —
  `rodin.md:18-22`, `:40-50`, `:74-82`, `:113-116`.
- **Facts are claims-with-evidence, shareable within trust domains; cache-
  poisoning defense in depth** — `store-architecture.md:45-58`.
- **Receipt analogy: tier-2 carries observed read set, reruns `verify()` before
  accepting; persistence changes residency not proof obligation** —
  `vx-store-as-vix-memo.md:255-268`.
- **Prototype (read at commit `f0f74179a`, the audited commit; the file moved on
  branch `rodin`):** `LearnedNoGood` `rodin.vix:150-154`; `learn()` `:961-967`;
  `conflict_support` `:812-818`; `point_region_for_conflict` `:820-829`;
  `clause_same_effect` `:848-856`; `widen_region` `:886-916`; install/subsume
  `:918-959`; apply `:1030-1121`.
```
