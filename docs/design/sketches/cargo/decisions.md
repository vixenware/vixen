# cargo.vix ⊗ version resolution — decisions

*The design cycle's ledger: each D-marker in cargo.vix gets its ruling here.
Sketch and rulings co-evolve; the sketch is the test of every ruling.
STATUS CORRECTION: D3/D4/D8 below were marked RULED unilaterally — process
breach, called out by Amos. They are PROPOSED until ruled together. The
sketch v0.2 embodies the proposals so they can be critiqued concretely.*

## D3 — the solver seam (PROPOSED, pending Amos)

`solve` is a primitive (like exec/fetch): the PubGrub-style SEARCH runs
runtime-side, but every FACT it consumes is demanded back from vix as a
pure, memoized, pinned question:

- `available_versions(registry, name)` — an OBSERVATION (journal-pinned;
  this is the only place "what exists in the world" enters resolution).
- `manifest_of(registry, pkg_id)` — PURE (checksum-addressed content).

Resolution is therefore a pure function of (roots × pinned registry views).
The runtime search is an implementation detail exactly the way an executor
is: it answers a question whose correctness the language can restate.

## D4 — pin the solve result, or recompute from pins? (PROPOSED: recompute — but see the solver-upgrade question)

The journal pins only the OBSERVATIONS (version listings; manifests need no
pin, they're content-addressed). The solve result is recomputed — it's pure
given the pins, memoization makes recomputation one hash lookup warm, and
there is exactly one source of truth. **The lockfile is not an artifact: it
is the named, committable journal slice covering resolution's pins.**
`cargo update` = ignore-and-refresh that slice. Lockfile merge conflicts
become journal-slice merges (pin-per-line; mostly append/replace — a better
story than Cargo.lock's).

## D8 — feature unification (PROPOSED: inside `solve` — the two-phase alternative deserves real discussion)

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
