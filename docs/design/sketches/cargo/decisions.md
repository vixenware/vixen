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
