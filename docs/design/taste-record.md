# The taste record

*Every open question, with the decision I would make and why — so future
agents inherit direction instead of re-deriving it. These are NOT rulings:
each entry is a recommendation awaiting Amos unless marked RULED (then the
remaining work is a brief, not a decision). Scope filter for everything
here: the MVP is "resolve roughly what cargo does, end to end" (ruled
2026-07-04); an entry that doesn't serve the MVP says so.*

Format: question → decision I'd make → why → what would change my mind.

## Language semantics

**1. Type-closure hash contents.** RULED (2026-07-04, Amos, walkthrough):
"not just types, but also CODE" — fn identity = closure hash over the
fn's canonical AST plus the canonical ASTs of EVERYTHING it references,
transitively: functions AND type declarations. Convergent proof the same
day: the warm-daemon blast-radius test showed a real leaf edit leaves the
caller's memo entry hitting (stale result served — a false positive, the
forbidden outcome); pinned as warm_reload_leaf_semantic_edit_currently_
hits_stale_entry + an #[ignore] test of the correct behavior. FIX
LANDED same day (facet-cc b5f838e2d): SCC-aware closure hashes in
module tables, both evaluators lockstep, the pinned test un-ignored and
green (…misses_exact_theoretical_blast_radius), type-decl edits miss
exactly their transitive users, verified here 56/56 zero skipped.
Generics status honestly: the SURFACE exists (grammar v0.2 has generics
in types/patterns) but neither evaluator instantiates them semantically
yet — the instantiated-closure note is forward-looking, not current.

**2. Closure captures (constitution amendment A2).** RULED — and in
plain words (confirmed with Amos, walkthrough): the OLD behavior is that
creating a closure evaluates its captured bindings right then and stores
the values; the NEW behavior is that a closure just holds handles to the
(possibly unevaluated) bindings, and evaluation happens only when the
closure's identity is needed (hash/ship/compare) or its body actually
runs. Creating a closure computes nothing — consistent with everything
else. The flip is a lockstep change (oracle + engine + harness in one
commit); no window where the two evaluators disagree.

**3. toml()/json() shape-checking.** RULED (Amos, walkthrough): declared
shape ⇒ mismatches are HARD errors, no coercions, no defaulting; no
declared shape ⇒ structural Value form. Coercion rules are a tarpit and
crates.io wouldn't admit non-conforming manifests anyway. Depends on the
user type surface (entry 8) for declaring shapes in vix itself.
Follow-on question surfaced by implementation (real-world-inputs agent,
NOT yet ruled): JSON null currently maps to Option::None — whether vix
wants a first-class null/option story at the Value level needs a ruling
before shapes land.

**4. Per-field struct laziness.** WANTED (Amos, walkthrough). Today the
engine constructs structs strictly except for tree-typed fields (a
recorded deferral in the constitution, not a version of the language —
the earlier "v1/v2" phrasing here collided with the spec's invariant
numbers and is gone). The design target: constructors bind field NODES,
giving per-field laziness for free, consistent with env-maps-names-to-
nodes. Sequence it as a lockstep change when it next brushes real work.

**5. Path-demand through merged trees — FOUNDATIONAL, RULED (Amos,
2026-07-04).** Say a program builds `let m = merge(a, b)` and someone
demands one file: `m / "x.h"`. Today the engine treats reaching through
the merge as needing the WHOLE merged tree's identity — both a and b
flush completely to serve one file. My original entry called tunneling
the path demand through the merge (right-wins: ask b, fall back to a)
an optimization to defer until a program visibly paid. Amos overruled,
and the reasoning is structural: "we're going to pay for it 100% of the
time, because the entry point is returning a tree" — every build's
result IS a tree assembled by merges, and demand ENTERS as paths out of
that tree. Without merge-tunneling, every selection at the edge forces
the whole output tree — the whole build. "Tracking demand through merge
is not just a nice optimization later on, it is the very foundation of
demand-driven evaluation for builds." Dispatched, together with entry
7's PathMissing/PathPending distinction (entangled: refining through a
merge whose right side is a Producing tree requires missing-vs-pending
to be answerable). The oracle stays eager (canonical); the harness
gains per-program divergence assertions (engine Finished ⊊ oracle
Finished on a merge-projection program, asserted, not tolerated).

**6. Batching policy beyond memo boundaries.** CLOSED (Amos: "I don't
think we're even going to revisit"). Batch = memo unit, period; the
joint-demand sets remain the socket if the future disagrees.

**7. demand_path conflation wart.** The oracle (mirrored by the engine)
conflates "path not in tree" (error) with "path not yet produced" (wait).
Decision: typed distinction (PathMissing vs PathPending). Amos's deeper
framing, banked from the walkthrough: the hard question about trees has
always been progressive-vs-fixed — a fully-Merkled tree can't also be
progressively available, since content arriving later would change the
root hash. The design's answer, which the wart fix should follow: a tree
is either Resolved (hash exists, immutable — "missing" is decidable
immediately) or Producing (per-path availability now, whole-tree hash
only at flush — "missing" is only decidable when the producer FINISHES;
until then the honest semantic for an unwritten path is wait). So:
PathMissing fires immediately on Resolved trees, and on Producing trees
only after Finished; before that it's PathPending. Lockstep change.

**8. User type surface (enums/structs/match in user code).** DIRECTION
AGREED (Amos, walkthrough): already went over this — sketches exist
(types.vix), and the target is a language very close to Rust, because
vix targets Rust builds and Rust shapes are the right fit. Grammar and
AST carry the surface; what remains is evaluator semantics + the
checking story, and that remainder is implementation-round work against
the agreed direction, gating shape declarations (entry 3) and
self-hosted intelligence queries.

**9. Capability-threading sugar (implicits).** CLOSED (Amos: "not
actually an open question"). Don't. Explicitness is part of the
auditability story.

## Engine and substrate

**10. Weavy suspend ABI.** Suspend ONLY at run boundaries (the places
demand blocks — strictness points against pending runs); suspended state
= arena + run-latch tables in the lowered Ctx (constitution §6+addendum);
resume keyed by run id; frontier enumerable always (V26). Prove with the
suspension-trace differential exactly as weavy::jit::async did. The one
place a wrong shape means a rewrite — design line, not delegable.

**11. Engine warm reload (arena persistence).** The MEMO TABLE and the
journal persist (content-addressed, canon-hash-keyed — same as the
oracle's warm story); the ARENA does not (node ids are positional,
per-eval). Warm eval = fresh arena + persistent memo + persistent
journal. Why: persistence belongs only at content-addressed boundaries;
persisting positional state is how incremental systems rot (rustc's
dep-graph is the cautionary tale we already rejected). Would change my
mind: measurements showing arena rebuild dominates warm evals on real
programs — then cache the LOWERING, still keyed by canon hashes.

**12. Engine-in-daemon.** After the current warm-daemon stream lands:
vixd evaluates via the ENGINE with the oracle shadow-evaluating and the
equivalence contract asserted in the serve path for a while (production
differential). The harness already proves corpus equality; shadowing
catches what the corpus misses. Then the oracle retreats to tests.

**13. JIT lane for vix.** Not until the fleet path is boring. The
interpret-first invariant (V24) makes this safely postponable forever;
the weavy substrate is proven and waiting. Nothing in the MVP needs it.

## Resolution (Rodin)

**14. Fact minimization to derivation support.** v0.1 requirement, not
optional: at learn time, shrink the recorded region to the packages/
versions actually in the refuting derivation's support (trace step 14
showed one-edge support recorded as a four-package region). Fat facts
widen worse and explain worse. Brief-ready once the resolve view lands
(the view makes the gap visible; the fix makes it disappear on screen).

**15. Worker model.** In-process workers claiming regions from a shared
frontier queue, facts pooled through the two stores; determinism is free
(V21 — any complete search returns the unique optimum). Workers-as-fleet-
executors is genuinely tempting (facts + regions are plain shippable
data) but wait for in-process measurements first; the severability rule
(frontier=1, workers=1 ⇒ CDCL) is the safety net.

**16. Prerelease admission.** Model as a per-interval admission flag in
VersionSet (cargo admits prereleases only when the requirement mentions
one), not an ordering trick. On the record as v2; the mock-registry
oracle will catch any fixture where it bites earlier.

**17. Solver events → lanes.** The SolveRecord's step vocabulary
(Demand/Decree/Intersect/ForcedPick/Expand/Conflict/Learn/Solution) IS
the event vocabulary — don't invent a second one. A solve run's interior
is a nested lane under its run row; same three-moment discipline.

**18. Order-dependence hunt (cargo).** Keep the #[ignore] test with its
negative verdict. Escalate fixtures toward feature-unification
interactions (optional deps activated by features across multiple
parents) — my hypothesis is the order-dependence lives there, not in
plain version selection. The harness makes the proof free whenever it
appears; do not spend design-line time hunting.

**19. VersionReq: capability type vs language type.** Capability-
provided. The language never learns semver: ecosystems are closures
(V18's boundary), each brings its own requirement semantics; vix needs
only opaque values with hash+order; the solver consumes compiled
interval sets runtime-side (rodin-facts). Debian-class ecosystems later
confirm this: their constraint language differs, the seam doesn't.

**20. Registry layout probing.** Capability business entirely: index
URL templates, sparse layout, auth — config data inside the registry
capability's closure, fingerprinted like everything else. The language
sees versions_of/deps_of, nothing lower.

**21. Lockfile import mechanics.** Import Cargo.lock as the initial
result pin (ruled); the import is a pure parse producing the same
artifact shape a solve produces, flagged with provenance "imported".
The audit (recompute-from-pins) then reports divergence exactly as for
any old pin. No special compatibility mode.

## Commands, tools, product

**22. Command-grammar spec (the §3⅞ expansion).** The one large design
document still unwritten. Contents when written: annotation format for
roles/commutativity/salience, the conservative-fallback law (V29) stated
operationally, and ONE worked grammar (cc) end to end — roles feeding
mount ceilings, normalization feeding identity, salience feeding
descriptions. Everything else (rustc, ar, tar) is then mechanical.
Design line owns the spec; grammars themselves are delegable.

**23. rmeta naming convention.** Decide inside the rustc command
grammar, not in language code: the grammar's output roles name what the
tool produces; consumers project `out / "lib.rmeta"` against the
producing tree. Never string-match paths in vix source. Blocks nothing
until the rustc grammar exists.

**24. Who ships command grammars.** The capability does (synthesis
already leans there; make it a ruling when convenient). A toolchain
advertisement without its grammar is degraded per V29: usable, tier-1
only, loudly annotated.

**25. Streams details (chunking/replay/backpressure).** Producer-defined
chunks, in-order replay from the store, pull-based backpressure —
i.e. keep the current simple story until real fleet loads argue.
Content-defined chunking is an optimization with provenance costs;
don't reach for it speculatively.

**26. den/skulk timing.** Unchanged: wait for an installable artifact.
The division doc governs what goes where when it happens.

**27. Windows VFS route.** Proprietary side; the language only assumes
read-set observability exists (V9). ProjFS's enumeration blindness is
established; NT driver vs paid-fuse is a business call, not a design one.

**28. Aggregation heuristics.** Wait for measurements (entry 6's
sibling). Sound by purity; only profitability is in question.

## Meta

**29. The two-agent structure after July 7.** Keep it: a design line
that rules with Amos and writes self-contained briefs with planted
judgment questions, implementation fanned out to cheaper agents, every
brief carrying its oracle. The pattern is proven (keystone, Rodin core,
warm daemon). The resurrection note's comms rules are load-bearing;
read them before talking.

**30. What NOT to build.** No second homemade reference solvers (cargo
is the oracle). No serde, ever. No hand-emitted JSON. No backwards
compatibility unless requested. No time estimates. No silently
simplified versions of agreed plans — fidelity to the plan is the
metric, not green checkmarks.
