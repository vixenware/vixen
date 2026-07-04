# cargo.vix ⊗ version resolution — decisions

*The design cycle's ledger: each D-marker in cargo.vix gets its ruling here.
Sketch and rulings co-evolve; the sketch is the test of every ruling.
D3/D4 RULED WITH AMOS 2026-07-03; D8 direction confirmed (not two-phase),
depth in progress.*

## D3 — the solver seam (RULED with Amos)

Confirmed as proposed ("solver calls back into vix for node evaluation" —
Amos's original words). Additions from the ruling: vix STAYS DECLARATIVE —
no search state in the language, ever. `solve` must be flexible enough to
service cargo/npm-class ecosystems (PubGrub-shaped, ranges + backtracking);
SAT-requiring ecosystems (debian) are explicitly LATER. The fully-opaque
alternative (runtime index access, unpinned) is dead.

Original proposal text:

`solve` is a primitive (like exec/fetch): the PubGrub-style SEARCH runs
runtime-side, but every FACT it consumes is demanded back from vix as a
pure, memoized, pinned question:

- `available_versions(registry, name)` — an OBSERVATION (journal-pinned;
  this is the only place "what exists in the world" enters resolution).
- `manifest_of(registry, pkg_id)` — PURE (checksum-addressed content).

Resolution is therefore a pure function of (roots × pinned registry views).
The runtime search is an implementation detail exactly the way an executor
is: it answers a question whose correctness the language can restate.

## D4 — RULED with Amos: pin the RESULT (cargo's strongest property)

"Ignoring a Cargo.lock without an explicit `cargo update` is not a cute
quirk, it's a security liability." So the journal pins BOTH the
observations AND the solve result. Precedence semantics:

- The RESULT PIN is the authority: warm or cold, same pins ⇒ same graph,
  across solver upgrades, forever — cargo's stability property, kept.
- Recompute-from-pins becomes the AUDIT: re-derive under the current
  solver and compare. Divergence is a visible diagnostic ("solver would
  now choose differently"), never a silent change. This slots straight
  into the trust ladder: a lockfile is spot-checkable like a cache hit.
- Explicit `update` (whole slice or per-package) refreshes observation
  pins AND re-derives the result pin, atomically.
- The result pin's key includes the roots + observation-pin set; the
  solver fingerprint is recorded IN the pin (provenance) but does NOT
  key it — that's exactly what makes old pins keep serving after
  upgrades.

Superseded proposal (recompute-only) kept below for the record:

The journal pins only the OBSERVATIONS (version listings; manifests need no
pin, they're content-addressed). The solve result is recomputed — it's pure
given the pins, memoization makes recomputation one hash lookup warm, and
there is exactly one source of truth. **The lockfile is not an artifact: it
is the named, committable journal slice covering resolution's pins.**
`cargo update` = ignore-and-refresh that slice. Lockfile merge conflicts
become journal-slice merges (pin-per-line; mostly append/replace — a better
story than Cargo.lock's).

## D8 — feature unification (DIRECTION RULED: inside solve, NOT two-phase; depth in progress)

Amos: two-phase's phase 1 is overly broad, making phase 2 extremely
expensive. Sharper still: two-phase is WRONG, not just slow — phase 1 must
over-include all optional deps, so a version conflict in an optional dep
that no feature ever activates can fail a build that should succeed.
Depth (term language, universes, determinism, additivity) being designed
in-thread; this section gets the outcome.

VERIFICATION vs the March verbatim record (march-solver-recovered-verbatim.md
§tension-iii): the features-as-propagated-Horn-terms framing is a JULY
invention, NOT recovered — March never modeled feature activation as
propagation (features were always a union-join INPUT threaded through
recursive calls; msg 22 sketch). What March DOES support verbatim: the
decision-vs-propagation architecture itself — msg 4: "make an explicit
decision (an assumption)... keep the learned incompatibility but retract the
assumption level. That is basically PubGrub/CDCL recast in Vixen terms" —
and incompatibilities-over-terms (msg 4, line 252). July's framing SUBSUMES
March's union view: the final feature set IS the union of derived positive
literals. The new claim to scrutinize when ruling: "version choices are the
ONLY decision points" — believed true for cargo (features are only ever
demanded, never freely chosen; weak features dep?/feat are guarded Horn
implications, still propagation), stated here so the ruling is on exact
words, not paraphrase.

Superseded framing below:

Features are mutually recursive with version selection: enabling a feature
can enable an optional dep, which changes the graph, which changes
resolution. That mutual recursion is the actual hairiness of cargo's
resolver, and it belongs INSIDE the primitive, not in language-level
fixpoints. `solve` returns the fully unified graph:

```
struct ResolvedPkg {
    id: PkgId,               // name/version/checksum
    features: [String],      // UNIFIED: union over all dependents,
                             // per resolver-v2 universe rules
    deps: [Int],             // indices into the resolved graph
}
solve(reg, roots) -> [ResolvedPkg]
```

Consequences that make the design sing:
- `build_dep` stops filtering by name (the D8 hack dies): it walks
  `resolved[i].deps` indices — the graph shape is DATA, sharing via memo
  identity: a diamond dep is one (id × features × …) memo key, compiled
  once, without any explicit build-graph bookkeeping.
- Features enter `crate_obj`'s identity naturally (they become --cfg
  argv items → part of the normalized plan → part of both cache tiers).
- Resolver-v2 universes (build-deps vs normal, per-target) are just
  SEPARATE solve() calls with different roots — no special casing.

## D1 — toml()/json() (direction, unruled details)

Pure fn from tree-path to a vix Value, shape-checked against a declared
struct. Rides the existing journal/identity machinery (the tree is
content-addressed; no pin needed). Needs: the shape-checking semantics
(what's a soft vs hard mismatch) — a types-round question, deferred.

## D2, D5–D7 (open, next sketch iteration)

- D2: VersionReq as capability-provided type vs language type.
- D5: registry fetch layout probing (index formats, URL templates — all
  capability business; fingerprint covers index RULES).
- D6: the rmeta projection naming convention (`lib.rmeta` path inside the
  producing tree — coordinate with the rustc command grammar).
- D7: recursion + memo as THE build-graph mechanism — holds up in the
  sketch; needs the engine's memo (keystone) to be the evaluator for the
  claim to be load-bearing.


## Walkthrough rulings (2026-07-03, easing-back-in session)

- **Import, don't imitate**: divergent lockfiles vs cargo are FINE (Amos's
  March position, reaffirmed); an existing Cargo.lock imports as the
  initial result pin. Bit-compatibility with cargo's tie-breaking is a
  non-goal — cargo's result provably-someday depends on THE ORDER OF
  DEPENDENCIES IN Cargo.toml (Amos; prove it with the mock registry).
- **Vocabulary**: standard solver jargon — DECISIONS vs PROPAGATION.
  Decisions are provisional HYPOTHESES; prefer the "carving out chunks of
  the search space" framing over "unmaking choices". Features form a
  join-semilattice (additive, union).
- **Features**: never chosen BY THE SOLVER — user-opted features of direct
  deps are part of the problem statement (roots); everything else is
  propagation. The solver's only decision points are versions (claim
  survives, restated precisely).
- **THE ORACLE IS CARGO, not a second homemade solver**: build a mock
  crates.io API (index + downloads) so real cargo runs against constructed
  registries in tests. NO "boring reference solver" — two novel
  implementations checked only against each other is a known agent
  antipattern (Amos). Harness asserts: (a) our solution validity;
  (b) equality with cargo on instances with a UNIQUE solution;
  (c) optimality under OUR canonical order elsewhere. The
  Cargo.toml-order-dependence proof falls out of this harness free.
- **Shared fact cache across users**: parked. Space too vast to matter
  for now; if the problem space of crates.io ever compresses/distributes,
  that's a Vixen-Cloud-tier product dream (the plural is the product),
  not OSS core. Facts stay process/snapshot-scoped.


## Walkthrough rulings, round 2

- **Separate compilation contexts**: "not even a question" — no feature
  unification across normal/build/proc-macro contexts, separate solves.
- **Default features**: implementation properties only, no solver
  machinery. Closed.
- **BEYOND CARGO — declared feature exclusions (Amos)**: mutual-exclusion
  lists and negative features (multiple TLS backends: pick ONE) are
  first-class incompatibility clauses in OUR manifest layer — resolution-
  time conflict with a derivation-tree diagnostic instead of cargo's
  duplicate-symbol compile error. Features stay additive for PROPAGATION;
  declared exclusions make some demand-combinations infeasible (drives
  version re-choice or a good error; the solver still never chooses
  features). Delivery hook: cargo ignores [package.metadata.*] tables —
  crates could ship [package.metadata.vixen.features] exclusions on
  crates.io TODAY without cargo caring.
- **The marble-sculpture realism worry (Amos)**: pure negative carving
  doesn't scale as a search strategy — recorded position: the sculpture
  describes the KNOWLEDGE (facts = negative space, compact after range
  widening), not the SEARCH (hypotheses walk points; a trail is tiny; the
  "infinite remainder" is never represented). The volumes/frontier layer
  is SEVERABLE: with frontier = one region and one worker it degenerates
  to CDCL — so the exciting engine is an additive parallelization layer
  over a sound core, and its realism question reduces to "can the frontier
  representation stay compact", answerable empirically later.


## The evening it became real (2026-07-03)

crates/reg-mock (c2d26a49, 26724f35): mock sparse registry + REAL cargo as
oracle. Order-dependence hypothesis: NEGATIVE on a simple multi-solution
fixture (cargo normalized; kept #[ignore] with verdict — now an open hunt,
not a fact). crates/vix-solve (ba73c793, 814e5c62): pubgrub 0.4 placeholder
core behind the ecosystem-neutral Facts trait; features as virtual packages
(collapsed from output); declared EXCLUSIONS work with derivation-tree
errors (the beyond-cargo feature, already resolving); 5 differential tests
green vs real cargo; deterministic-result asserted. FINDING for the March
engine: pubgrub's positive-dependency model CANNOT express weak deps
(dep?/feat) faithfully — deferred, surfaced structurally in
SolveResult::deferred_weak_dependencies. The March engine must support
guarded implications natively; that is now a requirement, not a nicety.


## Patches/overrides — the three-layer model (PROPOSED 2026-07-04)

Amos: cargo's [patch] "suuuuuuuucks" — changes patched identity, requires
whack-a-mole sibling patching, semver theater. Root cause: ONE mechanism
(source replacement) conflating THREE intents. Ours, layered on machinery
that already exists (overrides are ordinary INPUTS to the pure resolution
function, never mutations of the world):

1. **Version steering (decrees)**: "resolve with P in X..Y" = one root
   clause Ver(P, X..Y) injected into Rodin's clause store, derivation
   Premise::Decree. The solver adjusts everything else — Amos's "trivial
   query" IS this. Conflicts produce derivation-tree diagnostics naming
   exactly which requirement blocks the decree; an explicit "force"
   escalation amends the blocking requirement clauses (recorded as
   decree-derived — explainable, no theater). BONUS: solve is pure+memoized
   ⇒ version SWEEPS are a vix map over solves ("test against tokio
   1.35..1.40" = matrix builds as an ordinary query).
2. **Content overlay**: "use MY tree for P@V" = rebind pkg_src(P@V) to a
   different tree hash. Downstream of resolution entirely; resolution
   identity unchanged, BUILD identity changes honestly (different tree ⇒
   different memo keys ⇒ correct rebuilds, not churn). No version lying.
3. **Metadata overlay**: fork changed P's deps ⇒ deps_of amendment for
   (P,V) — an input layer to the same pure function, journal-visible.

Sibling whack-a-mole dies at the source level: one overlay decree names a
TREE (a workspace fork); the registry closure enumerates its member
packages (pure) — one line covers tokio + tokio-util + tokio-macros.


## The boundary (RULED with Amos in the walkthrough, banked late)

- **Ecosystems are closures.** The solver's native vocabulary is neutral
  atoms (names, version sets, requirements, feature demands, exclusion
  clauses). Everything cargo-specific — manifest interpretation, cfg
  filtering, renames (edge labels, resolved inside deps_of), our
  metadata.vixen exclusion tables — lives in ordinary vix closures shipped
  by the registry capability: versions_of (wraps the pinned observation;
  ONE observation per crate: its index file — both lookup levels arrive in
  one fetch, resolution never touches tarballs, cksum is the build-time
  handoff) and deps_of (pure parse of pinned content). Rejected: a
  declarative clause-schema (cfg evaluation kills it; closures are
  memoized pure vix calls, cacheable across solves).
- **solve IS A RUN.** Registers in the runs registry; its execution is the
  search; every fact it needs is a demand issued back into the engine
  (deps_of = Call node, versions_of = observation node). Consequences:
  resolution is observable in trace/lanes with the existing three-moment
  events; resolution gets tier-2-style reuse (its read-set = pins +
  deps_of keys consulted); suspension story already solved (blocked
  fact-requests are frontier entries).
- **The primitive reentrancy rule**: a primitive may only communicate
  with the engine via issued demands, never shared state (exec already
  obeys it unknowingly). Goes into vix-spec as a named rule.
