# Prior-art adjudication — vix & rodin

*Synthesis of three independent adversarial scrubs (Gemini, Claude
Research, GPT) run against the kill-format prompt on 2026-07-04. Each
was instructed to kill claims C1–C7 (vix) and R1–R2 (rodin), quote
evidence, and report search trails. This document adjudicates their
disagreements and records what survives as claimable, what is ceded,
and what must be read before publishing anything.*

*Trust weighting: Claude and GPT cite real, checkable papers
(PubGrub, gg, BuildXL docs, Vesta SRC papers, RIKER, IncPy). Several
of Gemini's quotes look confabulated (a "sota.io 2026" Unison deploy
post; a "Vesta manual" provenance quote matching no known Vesta text).
Where Gemini alone kills, treat as unverified.*

## Scoreboard (adjudicated)

| Claim | Verdict | One-line resolution |
|---|---|---|
| C1 full combination | **SURVIVED** (3/3) | No single system: pure language + never-materialize demand + every-call content-hash memo + durable cross-machine journal + observed-read-set effects. |
| C2 closure-hash identity | **KILLED as worded** — Unison | Cede the identity mechanism. Rewrite: "every call as automatic durable memo boundary in a build runtime" — that survives (Value.memo is opt-in). |
| C3 demand tunneling | **WOUNDED; core SURVIVED** | Nix laziness stops at derivation granularity; Vesta's path-deps are cache precision under call-by-value. MERGE tunneling (one path of a merged tree → only the contributing producer) found nowhere. |
| C4 two-tier + negative lookups | **KILLED on mechanism** — BuildXL | Weak fingerprint × observed pathset incl. absent probes + dir enumerations is BuildXL, verbatim. Survives: the soundness posture (VFS mediates all reads → bugs cause only false negatives) and call granularity. |
| C5 journal as attestation | **NEAR-KILLED** — gg + Trustix | gg's "memoization assertions" are signed thunk-hash→value-hash provenance; Trustix is a Merkle log of build input/output pairs used as a cache-trust index. Survives: external observations as journal entries; lockfile = journal slice as semantic identity. |
| C6 command-line grammars | **WOUNDED; core SURVIVED** | goma/reclient input processors and ccache parse CLIs for cache identity. Unfound: commuting-flag normalization for key equality + sandbox ceilings derived from grammar roles, one grammar as single source of truth. Patents unchecked. |
| C7 in-flight join | **KILLED on the ingredient** — GHC blackholing | Same-heap-thunk joining is decades old; Skyframe/DICE join declared node keys. Survives: joining independent computations discovered by content hash, fleet-wide. |
| R1 queryable resolution + counterfactuals | **SURVIVED** (3/3) | PubGrub derivation graphs exist for FAILURES only; no resolver ships selected-version kill-derivations or counterfactual re-solve-with-manifest-diff. Kill vector left unsearched: unsat-core/MUS-extraction literature. |
| R2 widening via read-set stability | **SURVIVED** (3/3, "deeply novel") | PubGrub is CDCL-with-ranges (cede that); widening a learned incompatibility across neighbor versions justified by VERIFIED read-set stability of metadata fetches exists nowhere found. |

## What this changes

1. **merge-demand.vix is the novelty demonstration, not just the
   acceptance test.** The one language-semantics claim every scrubber
   failed to kill is precisely what THE MILESTONE demonstrates: demand
   one file of a merged output tree, the non-contributing producer
   never materializes, proven by trace absence. When we publish, the
   demo IS the argument.
2. **Stop claiming the ingredients; claim the composition and the
   four surviving cores.** Claimable, in strength order: R2's
   verified-stability widening; C3's merge tunneling; C1's
   composition; R1's counterfactual product surface; C6's
   grammar-as-single-source; C4's soundness posture (design argument,
   not mechanism).
3. **Positioning sentence writes itself now**: Vesta is the ancestor
   (call-by-value, bridges, no demand semantics), Unison is the
   sibling on identity (opt-in memo, no build effects, no read-sets),
   BuildXL is the sibling on effects (action granularity, no
   language). vix is the point where those three lineages intersect,
   and none of the three can reach it by a small step — Unison lacks
   the effect layer and every-call memo; BuildXL lacks the language;
   Vesta lacks demand.
4. **Skyframe's restart-not-suspend model** (missing deps restart the
   SkyFunction; state-keeping is an ongoing retrofit) independently
   confirms that our "suspension ONLY for started-and-blocked, frames
   as the whole cost" ruling is a real differentiator, not just an
   internal aesthetic.

## Mandatory reading before publishing (the second-pass list)

- **Vesta book** (Heydon/Levin/Mann/Yu), caching + fine-grained
  dependency chapters. Open question that decides how much of C2's
  rewrite is safe: does Vesta's cache key cover the closure's CODE
  transitively (likely yes for models) and TYPES (unknown)?
- **gg**: "From Laptop to Lambda" + the earlier draft ("A Thunk to
  Remember") for memoization assertions. Closest thing to our journal
  entries; decide whether C5 is worth claiming at all afterward.
- **PIE / Pluto** (Pluto OOPSLA 2015; PIE ‹Programming› 2018; ASE
  2018; Konat's TU Delft thesis). The modern academic echo of "a
  language whose evaluator is an incremental persistent build
  system." Deltas hold today (task-boundary keys, declared `require`
  deps, local persistence, bottom-up capable) but this is the
  literature a reviewer will cite first.
- **Mill** code-hash invalidation (callgraph reachability) — nearest
  industrial cousin to closure-hash blast radius.
- **Trustix / laut** — the C5 adjacent in the Nix world.
- **BuildXL wiki + CloudBuild paper** (Esfahani et al., ICSE-SEIP
  2016) + Microsoft build-cache patent family — C4/C6 patent risk
  lives here; no scrubber ran a patent search.
- **Unsat-core / MUS extraction for version solving** — the one
  unsearched kill vector against R1.

## Gaps all three scrubs share (honesty ledger)

No patent search anywhere. No Vesta citation-descendant chase.
Tvix/Snix, Fac, OMake, Jenga, redo unexamined. No mailing-list/HN
archaeology. IBM/Rational ClearCase's derived-object winking
(VFS-based build audit + cache substitution!) was not examined by any
scrubber — closest commercial ancestor to the VFS soundness posture;
add to the second pass.

---

# ROUND 2 (2026-07-04, same day): pinned-question pass, adjudicated

*Gemini got patents/commercial archaeology (artifact-only, behaved
this time — real patent numbers, NOT FOUND answers); Claude got the
primary-source deep reads. GPT (kill vectors: unsat-core/MUS for R1,
observation-justified widening for R2, dune/memo + build-system tail)
STILL PENDING — final novelty scoring waits for it, since its
assignment holds the two vectors rated most dangerous.*

## Corrections to Round 1 (do not repeat these errors)

1. **Vesta owns negative-lookup and directory-enumeration
   dependencies (2000), not BuildXL.** PLDI 2000 §4.6.1: a failed
   file lookup during a tool run records a dependency on the object's
   NONEXISTENCE (X-type); a directory listing records a domain
   (D-type) dependency. C4's ancestry chain is Vesta → BuildXL.
   (This also supersedes Gemini's R2 conclusion that "the soundness
   posture belongs to BuildXL" — BuildXL industrialized the two-tier
   mechanism; Vesta had the observation classes first.)
2. **Unison's runtime Value.memo is NOT durable as shipped.** Round 1
   said "cached durably at a location incl. Unison Cloud." Primary
   docs: memo is keyed by computation hash but the shipped cache
   ability (Scratch) is EPHEMERAL and per-Location ("Scratch values
   do not persist after the Cloud job"). Unison's durable hash-keyed
   caches are codebase-level (compile/test). The Unison delta is now
   THREE separate cited deltas: every-call (vs opt-in), durable (vs
   ephemeral), journaled cross-machine (vs location-scoped).

## Q-by-Q outcomes

**Vesta key composition (Claude, primary text + US 6,145,056):**
primary key = IMMEDIATE function body + scalar-arg fingerprints;
callee bodies covered DYNAMICALLY as secondary deps (E-type recorded
at application); NO types in any key (SDL dynamically typed).
CONSEQUENCE: our claim survives as "STATIC transitive closure-hash
identity with exact blast radius computable before execution" —
credit Vesta for achieving equivalent SOUNDNESS dynamically; never
imply Vesta lacked code-identity in keys. Call-by-value confirmed
flatly for the core (§4); model-imports corner UNVERIFIED (hedge:
"modulo import loading" until RR-177/SDL-spec imports section read).

**gg (Claude):** cache = thunk hash → output hash ONLY; no external
observations anywhere; "memoization assertions" (signed third-party
claims, per-entity trust, dishonesty detectable post-hoc) exist in
the 2017 DRAFT only and were DROPPED from the ATC'19 paper.
CONSEQUENCE: C5 is publishable as exactly two deltas on top of
gg+Trustix: (a) external observations (registry listings, toolchain
fingerprints, negative lookups) as first-class journal entries;
(b) lockfile = journal slice / recompute-from-pins = audit. Cite
gg's draft prominently or a reviewer does it for us.

**PIE/Pluto (Claude):** task keys = invocation identity + STAMPS
(timestamps/content hashes of files/outputs), never task code —
benchmark J (semantically-equivalent spec refactor is INVISIBLE to
change detection) proves it. Demand-driven top-down mode exists;
persistence local-only. Related-work paragraph, not a C1 threat.
Konat thesis still unread (could contain a code-hashing extension).

**Skyframe (Claude, primary docs):** all three verified with
citations — restart-not-suspend (SkyKeyComputeState/StateMachines
exist as Google's own workaround for restart cost), SkyKey identity
never code content, cross-machine sharing only at the action layer.
Safe to cite as the published contrast to suspension-only-for-
started-and-blocked, WITH the footnote that StateMachines show they
know the cost — present ours as a design position they approximate.

**Patents (Gemini):** US 11,687,362 B2 (Microsoft, filed 2022) —
syscall-interception (code injection) to discover task inputs for
output reuse; the BuildXL-lineage mechanism patent. FLAG FOR
AWARENESS (vix's VFS-mediation is a different mechanism than
injected interception, but adjacent; not legal advice). NOT FOUND:
any patent on formal-grammar CLI parsing for cache keys or sandbox
derivation; no Google patents from goma/reclient lineage on it.

**ClearCase (Gemini):** winkin reuses via config records of files
successfully opened/read; NO evidence MVFS tracked failed lookups or
enumerations → false winkins possible. ClearCase is the VFS-build
grandfather but NOT the soundness ancestor.

**ElectricAccelerator (Gemini):** EFS tracks accesses for conflict
detection + JobCache (with C/C++-aware source hashing); no evidence
of absent-path or enumeration tracking.

**C6 upgraded (Gemini, corroborated by IRHash/cHash literature):**
ccache = ad-hoc flag lists; compiler-cache research normalizes the
SOURCE (CPP/AST/IR level), never the command line; Bazel/Buck2 hash
typed actions they GENERATED. "Raw CLI → formal per-tool grammar →
typed invocation AST → commuting-flag-normalized cache key + sandbox
ceilings from grammar roles" stands unclaimed in both patent and
practice corpora. C6 core: SURVIVED, now with the patent search done.

## GPT report (round 2) — kill vectors, adjudicated

- **R1**: SURVIVED exact wording. Spack has constraint-origin
  tracking + unsat cause trees, but its own Result.to_dict excludes
  "anything related to unsatisfiability" from SATISFIABLE results —
  direct evidence selected-version derivations are not its output
  artifact. OPIUM = the suggest-what-to-remove/optimization ancestor.
  No counterfactual-manifest-diff product surface found anywhere.
- **R2**: SURVIVED exact wording, and gained its theory lineage:
  generalized nogoods + symmetric explanation learning widen learned
  constraints by STRUCTURAL/logical justification; uv ASSUMES
  per-version metadata uniformity (declared, unverified — lockfile
  unsound when violated); nobody widens by OBSERVED read-set
  stability, per-edge. Cite the CP literature as ancestry.
- **C3 merge tunneling**: SURVIVED the targeted hunt — the top
  suspect CLEARED. dune Memo is a serious incremental engine (tens
  of millions of nodes, dynamic deps, cycles) but memoization is an
  EXPLICIT API call, cells are not content-keyed by code, and
  nothing tunnels through merged outputs. Determinate Nix lazy trees
  = INPUT-side demand (source copying granularity), not merged-output
  producer selection. Buck2 projection = staging, not selection.
- **C2-rewrite related work grows**: Mandala (@op Python decorator,
  content-addressed code+dependency versioning per call — MUST cite),
  R targets (function + globals dependency-closure hashing),
  charmonium.cache (durable, code+env-sensitive). All opt-in
  decorators; none every-call, none types, none language-runtime.
  joblib explicitly FAILS transitive code invalidation (documented
  issue: nested helper edit doesn't invalidate).

## FINAL SCOREBOARD (two rounds, six reports)

SURVIVED, claimable, strength order:
1. **R2 widening-by-verified-observation** (unanimous both rounds;
   theory ancestors are structural, not observational).
2. **C3 merge tunneling** (survived a targeted second-round hunt;
   THE MILESTONE demo is the demonstration).
3. **C1 the composition** (six reports, no full-combination system).
4. **C6 grammar-as-single-source** (survived practice corpus AND
   patent search; ccache=ad-hoc, Bazel=generates-not-parses).
5. **R1 counterfactual product surface** (Spack/OPIUM adjacent).
6. **C4 soundness posture** (VFS mediation, false-negatives-only) —
   design argument; mechanism ancestry Vesta(2000)→BuildXL.
CEDED / REWRITTEN: C2 → "static transitive closure hash incl. TYPES,
every call, durable" with Vesta/Unison/Mandala credits; C5 → two
deltas atop gg(draft)+Trustix; C7 → content-hash-discovered fleet
joining only.

## Remaining reading debt

Vesta RR-177 imports/evaluation section (the one call-by-value
hedge), Konat thesis, Atria-era MVFS whitepapers (physical only),
dune Memo code-level inspection (GPT recommends; medium confidence),
Mandala paper close-read before writing C2's related work.
