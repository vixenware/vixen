# The taste record

*Every open question, with the decision I would make and why — so future
agents inherit direction instead of re-deriving it. These are NOT rulings:
each entry is a recommendation awaiting Amos unless marked RULED (then the
remaining work is a brief, not a decision). Scope filter for everything
here: the MVP is "resolve roughly what cargo does, end to end" (ruled
2026-07-04); an entry that doesn't serve the MVP says so.*

Format: question → decision I'd make → why → what would change my mind.

## Language semantics

**1. Type-closure hash contents.** RULED in substance (synthesis §3‡):
fn identity = hash(fn canon AST, sorted canon ASTs of every referenced
type declaration, transitively). Remaining taste: generic instantiations
hash the INSTANTIATED closure (the type arguments are part of the
referencing site's identity, not the definition's); capability values
already join keys as captures, no special-casing. Must land before any
shared cache exists. Brief-ready.

**2. Closure captures (constitution amendment A2).** RULED: closures
capture env NODES lazily; identity-uses force captures. The flip is a
lockstep change (oracle + engine + harness in one commit) — write the
brief so the harness pins the OLD behavior in the commit before and the
NEW behavior after; no window where the two evaluators disagree.

**3. toml()/json() shape-checking.** When a declared shape is given,
mismatches are HARD errors (missing field, wrong type — no coercions, no
defaulting); with no declared shape, you get the structural Value form.
Why: coercion rules are a tarpit and cargo manifests don't need them;
hard-error keeps the pure-parse story honest (deps_of must fail loudly,
not guess). Would change my mind: a real registry corpus showing widespread
manifests that only parse under leniency — then leniency becomes an
explicit, named mode, never the default. Depends on the user type surface
(entry 17) for declaring shapes in vix itself.

**4. Per-field struct laziness.** v1 stays strict-except-trees (recorded
in code). v2: constructors bind field NODES — per-field laziness for
free, consistent with env-maps-names-to-nodes. Do it when a real program
demonstrates waste, not before; the constitution already reserves it.

**5. Path-demand refinement through merges.** v1 widens to Identity at
merge nodes (constitution §7) — correct, possibly wasteful. Refine only
with its own harness assertion (engine-refined ⊂ engine-v1 run flushes on
a corpus program built to show it). Low priority until a lua.vix-scale
program pays the widening visibly.

**6. Batching policy beyond memo boundaries.** Keep "batch = memo unit"
(V11) until measurements argue otherwise; the joint-demand sets are the
policy socket. Feed it the weavy stencil timings when tuning starts.
Never let policy change semantics (V4 guards this).

**7. demand_path conflation wart.** The oracle (mirrored by the engine)
conflates "path not in tree" (error) with "path not yet produced" (wait).
Decision: make it a typed distinction (PathMissing vs PathPending) SOON —
the resolve view, fleet timeouts, and producing-tree UX all want it.
Small lockstep change; brief-ready.

**8. User type surface (enums/structs/match in user code).** The next
language design round after the MVP. It gates: shape declarations
(entry 3), self-hosted intelligence queries (synthesis query plane), and
richer manifest modeling. Grammar and AST already carry the surface;
what's missing is evaluator semantics + the checking story. This is a
design round WITH Amos, not a brief.

**9. Capability-threading sugar (implicits).** Don't. Re-open only if
cargo.vix in practice shows unbearable threading pain. Explicitness is
part of the auditability story.

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
