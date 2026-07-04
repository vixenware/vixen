# Rodin — the carving resolver (March engine, specified)

*Codename Amos's (2026-07-04): the sculptor. Start with every possible
dependency graph; remove everything that isn't the solution. This document
is being co-designed in the final thread — sections marked OPEN are the
agenda; sections marked FIXED are settled by prior rulings or by evidence.*

## FIXED — inherited requirements (each with its provenance)

1. **Drop-in behind the Facts seam.** Rodin replaces pubgrub inside
   crates/vix-solve behind the same ecosystem-neutral trait
   (versions_of/deps_of). Double oracle waiting: real cargo (via
   crates/reg-mock) AND the pubgrub core it replaces. [tonight's build]
2. **Guarded implications are NATIVE.** pubgrub's positive-dependency model
   provably cannot express weak deps (dep?/feat) — surfaced in
   SolveResult::deferred_weak_dependencies. For Rodin this is a correctness
   capability, not a nicety. [tonight's finding]
3. **Decisions vs propagation** (standard jargon per Amos). Decisions are
   provisional HYPOTHESES over versions ONLY — features/optional-deps/
   exclusions are propagation (user root opt-ins are problem statement,
   not solver choices). Facts are monotone and keepable; hypotheses are
   retractable/discardable. [walkthrough rulings]
4. **Exclusions native.** Declared mutually-exclusive features are
   first-class incompatibilities with derivation-tree diagnostics.
   [Amos's beyond-cargo ruling; already working in the pubgrub placeholder]
5. **The dragon rule.** The canonical preference order is a pure function
   of (roots × pins) — NEVER derived from anything the search discovered
   (no floor-derived priorities, no incumbent-topology). Search may
   EXPLORE in any order; optimality is judged only against the canonical
   order. [March's unfixed #1 recommendation, fixed at birth]
6. **Determinism through optimality.** Parallel schedules must not leak
   into the result: the canonical order defines a unique optimum, so any
   COMPLETE search returns it regardless of worker scheduling. Determinism
   is a theorem about the contract, not a property of the scheduler.
   Asserted by running solve twice (already in the harness).
7. **Severability.** Frontier of one region + one worker degenerates to
   plain CDCL. The volumes layer is additive; its failure mode is "no
   speedup," never "wrong answer." [the marble-realism position]
8. **Range facts are verified, never interpolated.** Widening a point
   no-good to a range requires checking the manifests are identical (in
   the relevant respects) across the range — pure and cheap against
   pinned index files. Holes allowed: facts are unions of intervals, not
   assumed-contiguous. [March dragons, banked]
9. **Fact scoping.** Facts derived from manifests are keepable within the
   pinned world (scoped by the index-file hashes consulted). No sharing
   across users for now (parked as cloud-tier dream). Heuristic anything
   never persists. [March walk-back + tension ruling]
10. **Observable like everything else.** Rodin emits progress events in
    the three-moment spirit — resolution already appears in trace/lanes as
    a run; Rodin's interior (regions claimed, facts learned, incumbent
    improved) should feed the same console. [the dream, thrice-yes'd]

## OPEN — the co-design agenda, in proposed order

A. **The fact/term language.** What exactly is a fact? Sketch: terms over
   (package, version-set) and (package@v, feature) with guarded Horn
   implications; no-goods as incompatibilities over unions-of-intervals.
   Question: one representation for both propagation clauses and learned
   facts, or two (clauses vs region-exclusions)?
B. **The canonical order, precisely.** OURS (import-don't-imitate). Needs
   a total order over complete solutions, pure of search. Candidate:
   lexicographic over packages sorted by name, comparing versions
   highest-first — simple, defensible, explainable. Decide + write down.
C. **The frontier representation** — the realism question. Regions as
   per-package interval boxes (orthotopes) were March's sketch and its
   critique's biggest doubt (unions, feature-conditional shapes).
   Compactness is THE empirical risk. Decide the v1 representation and
   the measurement that would falsify it.
D. **The worker model.** In-process parallelism first (workers claim
   regions, pool facts). Later: could workers be fleet executors? (The
   solver's facts-plus-regions shape looks suspiciously shippable.)
E. **Widening mechanics.** What "manifests identical in relevant
   respects" means operationally (metadata-stable blocks as split
   boundaries — March's good idea); who computes change-points.
F. **Incrementality.** Reusing facts across solves under pin-hash scoping
   (process-local). Relationship to the audit path.
G. **Events + explain.** Rodin's event vocabulary (region claimed/refuted,
   fact learned/widened, incumbent improved) and the four-layer explain
   (witness/join/pruning/exclusion → demand-chain diagnostics).

## Sources

March verbatim record: march-solver-recovered-verbatim.md (quotes, dragons).
Decisions ledger: sketches/cargo/decisions.md. Placeholder + oracle:
crates/vix-solve, crates/reg-mock.
