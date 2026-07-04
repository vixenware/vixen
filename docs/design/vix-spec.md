# vix-spec v1 — the normative index

*This is the capstone distillation: the contract of the system in numbered,
enforceable statements. It graduates vix-synthesis.md (v0) from working
notes to spec. Detail lives in the documents each section names; when this
file and a detail doc disagree, THIS file is wrong and must be fixed loudly
(it is an index, not a fork). The language (vix) is open; this spec is
drafted in the vixenware working set and graduates to the open repo when
den exists.*

Binding documents:
- engine-demand-semantics.md — the engine constitution (node vocabulary,
  demand kinds, strictness points, equivalence contract, amendments A1–A3).
- rodin.md (+ rodin-c-evidence.md) — the resolver: 12 fixed requirements.
- sketches/cargo/decisions.md — the cargo-cycle ledger (solver seam,
  lockfile authority, boundary, patches direction).
- open-proprietary-division.md — what is open, what is product.
- vix-synthesis.md — full working record, superseded as index by this file.
- taste-record.md — every open question with the recommended decision.

## 1. Vocabulary (load-bearing terms only)

**Demand** — the only driver of evaluation; enters at the edge (selection),
backpropagates through the graph. Demand is the await. Demand has a KIND:
Identity (whole value, hashable) or Path (one path out of a tree).
**Thunk / Node** — deferred computation / unit of memo+execution (thunks
fused by aggregation). Nodes are plain data in an arena.
**Projection** — a composed access path over an undemanded value; free.
**Run** — one external execution (exec/fetch/solve). Three moments:
Created (thunk born), Scheduled (first demand — paying starts), Finished
(identity flush; outputs + serving class Fresh/Joined/Tier1/Tier2).
**Closure** — canonical content-addressed AST of a computation plus
everything it captures (values, capabilities, referenced type decls). Its
hash joins every memo key. The unit shipped to executors.
**Observation** — any fact learned from outside pure computation; always
pinned. **Pin** — a journaled observation value; addressable; upgrade =
re-eval ignoring it. **Journal** — the pin store; pluggable backends;
doubles as provenance/attestation.
**Primitive** — runtime-supplied effectful operation (exec, fetch, VFS,
solve). The only carriers of effect.
**Capability** — typed value granting use of a primitive resource; kinds
in types, instances in ACLs; fingerprinted; grammar rides with it.
**Tree** — the artifact currency: Merkle tree, lazily materialized,
Resolved (immutable hash) or Producing (progressive, per-path demand).
**Fingerprint / Read-set / Two-tier key** — coarse identity × observed
relevance; tier 1 = closure × plan × capability × mount fingerprints;
tier 2 = read-set-verified reuse (incl. listings and NEGATIVE lookups).
**Command grammar** — a snark grammar per external tool; invocations are
typed, normalized, role-annotated objects (five products: roles/ceiling,
identity, salience, validation, highlighting).
**Ecosystem closure** — everything registry-specific (manifest parsing,
cfg, renames, exclusion tables) shipped as vix closures by the registry
capability: versions_of (pinned observation) + deps_of (pure parse).
**Decree** — a version-steering root clause injected into the solve
problem statement, origin-labeled (root manifest / user).
**Fact (solver)** — a learned exclusion region with its derivation
(proof, explanation, widening certificate) in one object.

## 2. Invariants (numbered; provenance; enforcement)

### Purity and effects
- **V1. All vix code is pure; effects exist only in primitives.**
- **V2. Primitive reentrancy: a primitive communicates with the engine
  only via issued demands, never shared state.** (Boundary ruling; exec
  already obeys it; solve must.)
- **V3. No async surface, ever. Demand is the await.**

### Demand
- **V4. Nothing forces locally** — not application, member access,
  operators, conditionals. Everything becomes a graph; demand
  backpropagates from selected outputs; aggregation BATCHES demand,
  never adds forcing. (Killed attempts 1–4 when violated.)
- **V5. Projection doesn't force; identity does.** Projection SCHEDULES
  a run (first-demand latch) but never flushes it; hash/cmp/ship/mount/
  memo-arg positions flush.
- **V6. Strictness points are a closed list** (constitution §3). Any
  forcing outside the list is a bug, not a judgment call.
- **V7. Three moments per run, honest cardinalities**: Created ⊇
  Scheduled ⊇ Finished within a stream; events carry identity + source.
- Enforcement for V4–V7: the differential harness (vix/tests/engine.rs,
  facet-cc) — values exactly equal to the oracle; engine Misses/Created
  ⊆ oracle's (lazier, never eagerer); plus divergence tests proving the
  laziness is real (unused binding never demanded; unused cmd! never
  registers a run). This harness IS the definition of "didn't break vix"
  for every substrate that follows (weavy lowering, JIT, fleet replay).

### Identity and caching
- **V8. Caching is conservative: false negatives acceptable, false
  positives never.**
- **V9. Identity is two-tier: fingerprint × read-set** (the anti-Nix
  pillar; read-sets include directory enumerations and stat-misses).
- **V10. The closure hash joins every memo key — including the canonical
  ASTs of every type declaration the code references, transitively.**
  (The type-closure ruling, synthesis §3‡; the identity hole is real and
  must close before any shared cache exists.)
- **V11. Aggregation unit = memo unit.**
- **V12. Every vix value is hashable AND totally ordered; collect yields
  the canonical total order, never arrival order; scheduling
  nondeterminism is unobservable by construction. Floats: ordered-float
  semantics.**
- **V13. Identity = canonicalized parse, everywhere.** Vix code:
  strip-spans → phon bytes → hash. Commands: grammar-normalized
  invocations (commuting args reordered; enforced by the
  reorder-commuting-flags test on ExecPlan::normalized).

### Observations and the journal
- **V14. Every discovery is an observation; every observation is pinned;
  every pin is addressable and refreshable; all observations carry
  timestamps.**
- **V15. fetch with a checksum is pure; without one it is an observation
  that pins what it saw.**
- **V16. The result pin is the authority; recompute is the audit.**
  Same pins ⇒ same graph, across solver upgrades, forever. Divergence
  under a newer solver is a visible diagnostic, never a silent change.
  ("Ignoring a Cargo.lock without an explicit update is a security
  liability.")
- **V17. Cargo.lock is a source-of-truth journal backend** (imported,
  not imitated; bidirectional).

### Resolution (Rodin; detail in rodin.md — its FIXED list is normative)
- **V18. solve is a run**: registers in the runs registry; every fact it
  needs is a demand back into the engine; observable in trace/lanes;
  read-set = pins + deps_of keys consulted.
- **V19. Decisions are version hypotheses only; features and optional
  deps are propagation; user root opt-ins are problem statement.**
- **V20. The canonical preference order is a pure function of
  (roots × pins) — never derived from anything the search discovered**
  (the dragon rule). Lexicographic over (source, true name), versions
  highest-first, absent beats present. Renames never reach the solver.
- **V21. Proven optimality is the ONLY mode. Early stopping is not
  allowed, ever.** Determinism is a theorem about the contract (unique
  optimum under V20), not a property of the scheduler.
- **V22. The resolution result is a queryable artifact**: per-node
  derivations (which edges/facts chose the version, which facts killed
  each higher band); provenance queries are reads; counterfactuals are
  ordinary re-solves with amended problems returning diffs + manifest
  amendments; UNSAT returns its impossibility derivation.
- **V23. Facts are verified, never interpolated; a fact's validity range
  is its read-set's stability range** (per-edge, not whole-manifest —
  falsified/validated empirically, rodin-c-evidence.md).
- Enforcement: REAL cargo as oracle via the mock registry (reg-mock) —
  never two homemade implementations checked against each other;
  solve-twice determinism assertion; the ladder oracle (no rung without
  a recorded killer across the six burial scenarios) checks V21/V22.

### Substrate and product boundary
- **V24. Interpret-first: the interpreter is the reference and is always
  available; JIT is an accelerator that must never diverge and is never
  a prerequisite** (iOS/wasm forbid JIT). Enforced: weavy differential
  interp==JIT on results AND suspension traces.
- **V25. The vix crate stays WASM-clean** (snark-wasm links it; tokio
  and the real world live in vix-daemon/vix-wire/backends).
- **V26. Nodes are plain data; no host closures in node state; the
  pending frontier is enumerable at any suspension.** (The weavy-lowering
  shape constraint; suspended state = arena + run-latch tables.)
- **V27. Capability kinds live in types; instances live in ACLs;
  permissions never appear in types.**
- **V28. Capability invariance is daemon-enforced: advertise ⇒ watch ⇒
  poison.**
- **V29. Command grammars ride with the capability and are versioned/
  pinned; an argv item the grammar can't parse degrades to the widest
  assumption — unparseable is slow, never unsound.**
- **V30. Provenance is graph-side; sub-value taint is a non-goal.**
- **V31. Syntax spends the fewest innovation points.**
- **V32. Semantics are open and plural; trust and execution are the
  product** (open-proprietary-division.md). The solver is proprietary;
  the seam it sits behind (V2, V18) is open.

## 3. The welds (contracts between subsystems)

- **Exec seam**: language owns correctness (keys, reuse legality,
  verify()); runtime answers questions (read-sets, snapshots) and
  enforces ceilings. Crossing data is facet-typed: ExecPlan (normalized,
  role-annotated), Mount, ReadSet (canonical BTreeMap incl. negative
  lookups), Outcome. A buggy runtime can only cause false negatives.
- **Solver seam**: solve is a primitive behind ecosystem closures
  (versions_of/deps_of); resolution is a pure function of roots × pins;
  results enter the journal under V16. MVP scope (ruled 2026-07-04):
  resolve roughly what cargo does, end to end; the patches three-layer
  model (decrees / content overlays / metadata overlays) is the ruled
  direction, post-MVP.
- **Wire/fleet seam**: orchestrator moves handles and plans, never
  bytes; trees move executor→executor by demand (gravity); producing
  handles serve paths before whole-tree hashes exist; in-flight runs are
  JOINED, not cached; only completed runs enter the journal.
- **Daemon seam**: vixd owns a warm evaluator behind an actor; events
  stream to clients with identity, source, and (incoming) eval
  generations; stepping = gating the event stream.
- **Grammar seam**: one snark pipeline for vix source AND command lines
  (NUL-joined argv); grammar → generated AST → binder shared by
  evaluator and IDE; rings 0–3, no parallel LSP universe.
- **Query plane**: vix's caching invariants are exactly salsa's
  preconditions — a vix function IS a query; picante is the demand
  engine seen from Rust; self-hosted intelligence queries wait on the
  user type surface.

## 4. The harnesses (what "correct" means, mechanically)

1. Engine differential harness (facet-cc vix/tests/) — V4–V7, the
   equivalence contract, the laziness divergence tests.
2. Cargo-as-oracle (vixenware reg-mock) — solution validity; equality on
   unique-solution instances; optimality under OUR order elsewhere.
3. The ladder oracle (incoming with the resolve view) — every rejected
   version names its killer; zero Unexplained rungs = V21 holds.
4. Anti-Nix event assertions (vix-daemon tests, being sharpened) —
   whitespace edits cost zero misses; real edits cost exactly their
   blast radius.
5. interp==JIT differential (weavy) — V24, including suspension traces.
6. WASM build of snark-wasm — V25.
7. Clippy at zero, cargo nextest, no serde, no println, phon for bytes —
   standing hygiene, enforced by review.

## 5. Deferred on purpose

Everything open is in taste-record.md with a recommended decision and
its reasoning. Nothing is open silently.
