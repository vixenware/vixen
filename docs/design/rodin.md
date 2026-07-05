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

11. **PROVEN OPTIMALITY IS THE ONLY MODE.** Not the default — the only
    mode. Early stopping is not allowed, ever: first-found-and-quit is the
    door through which scheduling leaks into answers (cargo's disease).
    Bounds closed or it isn't a result. [Amos, 2026-07-04]
12. **The result is a QUERYABLE ARTIFACT, not a version list.** Resolution
    returns the dependency DAG *plus* the complete decision record: every
    node carries its derivation (which requirement edges and facts chose
    its version; which facts killed each higher band). Two query classes:
    PROVENANCE (pure reads over the record: who pulls in openssl, why is
    this feature on, why not 1.0.228) — instant; COUNTERFACTUAL (what
    would it take to: drop openssl / unify hyper / reach tokio 1.40) —
    each is an ORDINARY RE-SOLVE with an amended problem statement,
    cheap because same pins ⇒ all learned facts stay valid (round 3's
    warm-resolve measurement is literally this feature's latency budget).
    Counterfactual answers are DIFFS + minimal manifest amendments +
    apply-button; the decree→manifest-edit translation belongs to the
    registry capability (bidirectionality doctrine, same as command
    grammars). UNSAT counterfactuals return the impossibility derivation
    ("not without relaxing Z") — even 'no' is actionable. [Amos's
    openssl/hyper queries, 2026-07-04]

## OPEN — the co-design agenda, in proposed order

A. **RULED (2026-07-04): TWO STORES, married by derivation.** Clauses
   (manifest-compiled implications incl. guarded/weak, indexed by the atoms
   they mention) feed propagation; exclusions (learned regions, unions of
   intervals, hole-tolerant) feed the frontier. Every exclusion carries its
   derivation — proof (replay/scoping by pins), explanation (4-layer),
   and widening certificate in one object. Prototyped standalone as
   crates/rodin-facts (type design normative in the brief/dispatch).
B. **RULED (2026-07-04): lexicographic over (source, true name), versions
   highest-first, absent beats present.** Package identity = (source, name)
   — third-party registries and git/path sources are distinct nodes even
   under one name (also the March PackageInstanceKey dragon, honored).
   Renames NEVER reach the resolver: edge labels, resolved inside deps_of
   (fancy/derive → Feat(true-id, derive)); Rodin never learns aliases
   exist. Total, pure of search, one sentence to explain.
   PROTOTYPE FINDINGS (rodin-facts, 0416fd46): PkgIx must intern the
   (source, name) PAIR — follow-up sent. Prerelease admission holes cannot
   be encoded in plain intervals (cargo admits prereleases only when the
   req mentions one) — documented in from_req, release-equivalence tested;
   Rodin must model admission eventually (per-interval flag or ordering
   trick), ON THE RECORD as a v2 requirement.
C. **ROUND 1 VERDICT (2026-07-04): THE STRONG BET FAILED.** Real data
   (400 crates, 15,309 non-yanked versions → 8,139 whole-manifest blocks):
   ratio 1.88 overall, median 1.75, p90 5, max 26 — modest collapse, NOT
   hundreds-to-few. Counterexamples: serde, clap, wasm-bindgen,
   async-graphql… Region algebra itself held (max 11 fragments, tame).
   Evidence: docs/design/rodin-c-evidence.md. STRUCTURE IN THE FAILURE →
   two round-2 hypotheses under measurement, NOT declared true: (1) FAMILY
   FUSION — lockstep =pins (serde↔serde_core) aren't search freedom;
   families fuse into one search variable; (2) PER-EDGE granularity —
   exclusions only need the RELEVANT EDGE stable, not the whole manifest.
   ROUND 2 (5a6e4c56): fusion real but MARGINAL (1.8% of edges strictly
   lockstep; whole-manifest 8139→7894 blocks — keep fusion as optional
   preprocessing + UX, not load-bearing). PER-EDGE stability is the real
   structure: 13.4x collapse, median 11 versions/edge-block, p90 41.7,
   77.3% of edges ≤5 blocks.
   **PROPOSED RESOLUTION OF C (pending Amos):** regions stay as validated
   (boxes + feature literals; algebra held). WIDENING is per-exclusion,
   across the parent-version range where the derivation's EDGE-SUPPORT is
   invariant — "a fact's validity range is its read-set's stability
   range" (tier-2's move inside the solver). SPLITS are conflict-driven,
   at the change-points of the edges relevant to the refuting derivation
   — not global manifest change-points (March's framing, corrected by
   data). Evidence: rodin-c-evidence.md rounds 1+2.
   **RULED CLOSED (2026-07-04, round 3):** rodin-core v0 exists (sequential
   set-domain core, guarded clauses native, 64 workspace tests green) and
   dynamic evidence supports the design: aggregate widening factor 7.5x
   (13 point exclusions → 98 widened) across the six burial shapes, warm
   facts saved decisions+conflicts on re-solves (= the counterfactual-query
   latency budget), generated cascade trace in the report appendix.
   Uneven by fact kind — the per-kind table is the tuning map, not a
   threat. KNOWN GAP from review: learned regions are FATTER than their
   support (trace step 14: one-edge support, four-package region) — v0.1
   must minimize facts to their derivation support; fat facts widen worse.
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

## RULING (2026-07-05): Cargo.lock-shaped emit bridge

Rodin now emits the build-side resolved-manifest artifact directly:
`SolveResult` carries deterministic lock rows, and
`rodin_core::emit_lock(&SolveResult)` renders Cargo.lock format v4 with
Cargo's header, package field order, source/checksum when known, and direct
dependency entries. The open `crate.vix` consumer already accepts this
subset as data (`version`, `[[package]]`, `name`, `version`, optional
`source`, and `dependencies`), so the architectural round-trip is now:

```text
resolve with Rodin -> emit_lock -> crate.vix build-from-lockfile -> run
```

Two pins cover the bridge. The schema pin reconstructs the open
`mini_app -> alpha_lib -> core_lib` plus `mini_app -> formatting_lib`
fixture as a real `SolveResult` and pins the emitted bytes. The oracle pin
uses the facet-cc fresh-lock evidence workspace: for the lock-union
projection's 853/892 exact Rodin-vs-Cargo rows, each emitted package block is
byte-identical to Cargo's fresh `Cargo.lock`; documented accepted-divergence
rows remain outside that byte-for-byte assertion.

What remains for the live round-trip is wiring, not design: pass the emitted
lock string/tree into the open `crate.vix` build-from-lockfile entry point,
locate package source trees with the open fixture/source convention, and route
the produced build/run demand through the existing vix execution surface.

## Sources

March verbatim record: march-solver-recovered-verbatim.md (quotes, dragons).
Decisions ledger: sketches/cargo/decisions.md. Placeholder + oracle:
crates/vix-solve, crates/reg-mock.

## RULING (Amos, 2026-07-04): multi-version coexistence
Rodin adopts CARGO'S MODEL: version domains keyed by (package name ×
semver-major compatibility class), not one domain per name. syn 1.x
and syn 2.x are separate domains that coexist in one solution. 'We
have to align with cargo here.' Region/domain shapes change
accordingly; the 68 MultipleLockedVersionsUnsupported divergences
are the acceptance test.
