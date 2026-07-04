# The engine: demand semantics for real vix (the keystone)

*The design for vix/src/engine.rs — the demand-driven evaluator that replaces
the oracle's eager tree-walk as the REAL semantics, with the oracle kept
forever as the differential spec. This document is normative; the code
implements it; the harness enforces the equivalence contract at the bottom.*

## Why this exists

"Nothing forces locally. Not application, not member access, not operators,
not conditionals, NOTHING. Everything becomes a graph and demand
backpropagates from the selected outputs. Aggregation doesn't violate this,
it BATCHES demand." — the rule that killed attempts 1–4 when violated.

The oracle (vix/src/oracle.rs) is eager inside fn bodies and lazy only at
the exec seam (PendingTree) and the edge (deep_force). It is CORRECT about
values (purity guarantees that) but over-approximates WORK. The engine is
lazy everywhere; the oracle is its oracle.

## 1. The graph is a thunk store; demand is the only driver

Nodes live in an arena; `NodeId` is an index. The graph is built
INCREMENTALLY: demanding a node may create child nodes (dynamic expansion —
a Cond's arm exists only after its scrutinee resolves; a glob's fan-out
exists only after the listing arrives). Static-graph thinking is a trap;
the toy engine (graph.rs) already proved dynamic Cond demand.

```
Node = Thunk   { expr: ExprRef, env: Env }        // unevaluated code
     | Value   { value: Value }                    // resolved (Trees may be pending)
     | Call    { func: FnRef, args: Vec<NodeId> }  // memo boundary
     | Project { subject: NodeId, path: Path }     // pending / p"…"
```

`Env` maps names → NodeId (NOT values): a let binding is a node; using it
twice demands the same node; never using it never evaluates it. Sharing is
identity in the arena — the toy's shared-node-computes-once invariant.

`Value::PendingTree { run }` stays exactly as in the oracle: the exec seam
is unchanged (runs registry, PendingRun, two-tier cache, three-moment
events). The engine changes WHO demands, not what a run is.

## 2. Demand has a KIND — this is the semantic heart

```
Demand = Identity          // the whole value, hashable/comparable
       | Path(path)        // one path out of a tree-shaped result
```

"Projection doesn't force; identity does" generalized:

- `Project(subject, p)` propagates `Path(p)` to its subject. On a pending
  run that means `demand_path` (Scheduled fires, Finished doesn't). On a
  Thunk whose expr is itself a projection/merge, the path demand REFINES
  through it without materializing the whole tree.
- hash / cmp / splice-into-command / ship / memo-arg positions propagate
  `Identity` (flush; Finished fires).
- For non-tree values, `Path(_)` degenerates to `Identity`.
- `Path` demand on a Thunk still evaluates the thunk's EXPRESSION (control
  structure is cheap; the tree contents are what's expensive) — but the
  Value it produces keeps its pending trees pending except along the
  demanded path.

## 3. Strictness points (exhaustive list — nothing else forces)

1. **The edge**: `Engine::demand(entry, Identity)` deep-forces the result —
   demand ENTERS at the edge (same as oracle's call/invoke deep_force).
2. **Memo boundaries**: a `Call` demands each arg with `Identity`, JOINTLY
   (one batch — this is "aggregation unit = memo unit": the batch set is
   the memo key's support). Then key = (fn type-closure hash × args hash);
   hit returns the memoized node; miss expands the body Thunk.
3. **Scrutinee of match/if**: `Identity` on the scrutinee ONLY; arms are
   Thunks; only the selected arm's node is demanded (toy invariant:
   unused-Cond-arm-never-runs).
4. **Operators**: strict in their operands (`Identity`) — but note this is
   demand PROPAGATION, not local forcing: `a + b` demanded means a and b
   demanded jointly; `a + b` NOT demanded means neither is.
5. **Command-block splices**: `Identity` (a mount is a whole-tree use).
   Command blocks THEMSELVES are Thunks: `let x = cc! { … }` never demanded
   ⇒ no Created event, no run registered. (DIVERGENCE from the oracle,
   which registers the run eagerly while evaluating the body — see §5.)
6. **Structural iteration**: `units.map(f).collect()` — collect demands the
   collection's Identity, which demands each element's Identity JOINTLY
   (parallel fan-out is discovered, not annotated); map produces element
   Thunks (Call nodes), so the fan-out is a batch of memo boundaries.
7. **Observations** (fetch/acquire): strict when demanded (they produce
   values/pins, not trees-in-progress) — journal semantics unchanged.

Everything not listed is lazy: let-bindings, fn application (creates a Call
node, computes nothing), member access on structs (projects the field node
— structs get per-field laziness for free by making constructors bind field
nodes, v2; v1 may treat struct construction as strict in fields EXCEPT
tree-typed ones — record which choice v1 makes in the code).

## 4. Events (same enum, honest cardinalities)

Same Event enum, emitted by the engine: Miss/Hit at Call resolution (when
DEMANDED, not when written); Created when a command Thunk resolves to a
registered run (first demand of the cmd! expression); Scheduled on first
run-touch; Finished on identity-flush. The engine's event stream is the
lazier one; the trace/lanes views work unchanged.

## 5. The equivalence contract (the harness enforces this)

For every program in the corpus (eval.vix, types.vix, lua.vix; grows with
the language):

- **Values**: `engine.demand(entry, args)` == `oracle.call(entry, args)`,
  EXACTLY (deep-forced, structural equality). No exceptions, ever.
- **Finished**: multiset of (command, serving-class) EQUAL on the corpus
  (every run in these programs is reachable from the result). If a future
  corpus program has a genuinely dead run, the assertion for it flips to
  engine ⊊ oracle and THAT direction is itself asserted.
- **Scheduled ⊇ Finished** within the engine's own stream (sanity).
- **Work direction**: engine Misses ⊆ oracle Misses; engine Created ⊆
  oracle Created (the engine may be lazier, NEVER eagerer).
- **Journal**: identical pinned observations for identical results
  (observation keys compared as sets).

The harness is a differential test in vix/tests/engine.rs and it is the
definition of "didn't break vix" for every optimization that follows
(weavy lowering, JIT, fleet scheduling replay all test against it).

## 6. Suspension (why this shape lowers to weavy later)

The engine is a synchronous recursive driver in v1 — recursion IS demand
propagation, the arena IS the state. Because all state lives in the arena
(no host-stack captures beyond the recursion itself), the weavy lowering
(suspend points at run-boundaries; frontier = the set of demanded-but-
blocked nodes) is a mechanical transformation of the same structure. Do
NOT introduce host callbacks or closures into node state; keep nodes plain
data. The pending frontier must be enumerable at any suspension (the toy's
`pending_frontier()` generalizes to: all nodes blocked on runs).

## §6 addendum (from chunk-2, implementation fact)

The Scheduled/Finished latches need per-run state OUTSIDE the node
vocabulary: `scheduled: HashSet<run>` (first-touch latch) and
`unlogged_runs: HashMap<run, (command, span)>` (once-only Finished +
metadata). Consequence for the weavy lowering: the suspended state is the
arena PLUS the run-latch tables — both must live in the lowered Ctx.

## Amendments (from chunk-1's ambiguity report, ruled 2026-07-03)

**A1 — logical operators are conditionals, not strict operators.** §3.4 as
written was wrong for `&&`/`||`: they demand the LEFT operand's Identity,
and the right only when the left doesn't decide. Short-circuit is demand-
driven evaluation in miniature; the oracle already behaved this way and the
engine mirrors it. (§3.4 applies to arithmetic/comparison.)

**A2 — closures capture NODES; identity forces captures.** The oracle
captures already-evaluated frame values (eager); chunk-1 mirrored it for
parity, forcing visible bindings at closure creation. The RULING is the
opposite and follows the system's own logic: closure creation captures env
NODES (lazy — creating a closure computes nothing); hashing/comparing/
shipping a closure (identity uses) forces its captures, exactly like every
other identity-forces case. Implementation is a LOCKSTEP change (oracle +
engine + harness updated together) — deferred to its own brief; until then
both evaluators agree on the eager behavior and the harness pins that.

**A3 — duplicate named-argument bindings are an ERROR.** Chunk-1 found the
oracle lets a duplicate binding through: memo hashing sees the first value
while body lookup can see the later shadowing — memo key and evaluation can
diverge, which is false-positive-shaped. Both evaluators must reject
duplicate bindings at the call boundary. Small lockstep fix; fold into
chunk 2.

**Parity note recorded**: unselected match arms are unevaluated in BOTH
evaluators (oracle parity, not an engine-lazier divergence).

**A4 — merge refinement is FOUNDATIONAL, not an optimization (Amos,
2026-07-04).** §7's second bullet framed path-demand refinement through
merges as deferrable. Overruled: a build's entry point returns a TREE,
assembled by merges (collect-over-trees, outputs-as-mounts), and demand
enters the system as paths out of that tree. Widening Path to Identity
at merge nodes therefore forces the whole output tree on EVERY edge
selection — the build always pays, 100% of the time. "Tracking demand
through merge is not just a nice optimization later on, it is the very
foundation of demand-driven evaluation for builds." The widening below
stands only as a temporary implementation gap and must close. The
refinement ships with: (a) the PathMissing/PathPending typed
distinction (on a Resolved tree, missing is decidable immediately; on a
Producing tree, only when the producer FINISHES — until then a
demanded-but-unwritten path means wait), which right-wins merge lookup
requires; (b) a per-program divergence assertion in the harness —
engine Finished ⊊ oracle Finished on merge-projection programs,
asserted, the oracle staying eager as the canonical reference.

**A5 — THE TWO-EVALUATOR ERA ENDS; AIM FOR THE FINAL SHAPE (Amos,
2026-07-04, stark correction, landed same day).** The eager oracle is
the disease this document existed to cure: it is what every agent
builds when handed the demand-driven vision ("let me start with
something essentially imperative"). Naming it an oracle and building
the lazy engine against it was one step better and still wrong —
"there is no intermediate path. We have to go straight for the final
thing." Ruled:
- THE evaluator of vix is the weavy-async lowering: this document's
  demand graph, driven as an async runtime (workers, work-stealing —
  suspend-ABI ruling), interpreter lane first. JIT stencils are the ONE
  deferrable part (interpret-first survives unchanged).
- The oracle and the recursive-driver engine are FROZEN as of this
  amendment: no new semantics land in them, ever. No more lockstep.
  Semantics land ONCE, in the final evaluator. The differential harness
  converts to ABSOLUTE assertions (pinned values, event multisets,
  journal pins, laziness invariants); at corpus parity both frozen
  evaluators are DELETED. "Kept forever as reference" is revoked —
  tests pin behavior; implementations die.
- THE VALUE/TYPE MODEL (ruled): types come from Rust OR from vix.
  Rust-defined types: facet describes their layout — that is how
  lowering knows how to build and read them. Vix-defined types: their
  ABI is ours to choose and must be OPTIMIZED (roughly what a Rust enum
  would do — never everything-boxed-forever), as long as the layout is
  RECORDED as a first-class description and OBSERVABLE from Rust
  (introspection). One description vocabulary, two authorities. Scalars
  live unboxed on the operand stack; composites/trees/closures are
  handles into content-addressed stores; the shared boxed Value enum is
  a frozen-evaluator artifact and dies with them.
- WORKING-MODEL CHANGE: the design line DRIVES the foundation
  implementation directly (vix/src/machine) — conventions are
  established in code, not briefs, so later agents contribute to an
  established shape instead of reproducing the disease.

**A6 — THE ALIGNMENT (Amos + design line, 2026-07-04, evening; supersedes
every reference-implementation notion including parts of A5 and V24).**
- **ONE implementation of vix, ever.** Not a reference, not an oracle,
  not an interpreter, not a simplified/eager/early one. Testing is
  TRACE-BASED: the implementation has a record mode; tests assert
  sequences, presence, and ABSENCE of events in traces (absence is how
  laziness is proven). The pinned corpus contracts are that artifact.
- **NO dynamic-language machinery. None.** No tags on operands, no
  kind-markers consulted at runtime, no fallible arithmetic, no checked
  "reference lane". Typed instructions (AddI64 ≠ AddF64) over untagged
  operands; every "what is this" is answered at lowering time by the
  types. A sum type's discriminant is the TYPE's own layout, matched by
  code that statically knows the type — that is not a tag. Layouts are
  compile-time knowledge + introspection metadata (DWARF-like), never
  runtime dispatch. If a safety net is ever wanted it is STATIC
  validation of lowered programs (wasm-style), never runtime checks.
- **Portability is weavy's concern, not vix's.** Vix lowers to weavy;
  how a target executes (JIT or not) is invisible above the waterline.
  No implementation tier is a semantic authority; the written semantics
  and the pinned traces are the reference.
- **Fable is a SIBLING, not a lowering target.** Vix does not lower
  through fable (generating fable to express laziness is wrong-shaped).
  Both lower to weavy. Fable — extremely minimal today — grows teeth in
  exactly the directions vix needs from the shared substrate: typed
  everything (user structs/enums with layouts), async interaction with
  the host, the custom ABI/frame model. Every scary substrate unknown
  becomes a small fable×weavy question first.
- **Suspension is only for the started-and-blocked.** A computation
  that began and cannot make forward progress suspends. An undemanded
  node is pure data — no future, no frame, no cost (graphs have
  millions of nodes). Never spawn-everything. Joint-demand batching at
  memo boundaries is a SCHEDULER POLICY about when to begin — not a
  default, never a return to spawning.
- **One unified trace, and instrumentation is weavy IR.** You emit IR
  with however much instrumentation you want; weavy strips/ignores it
  by mode (tests trace innards; production keeps suspension points for
  observability). Language events, memo hits, suspension — one channel,
  feeding tests AND the lanes/playground views. The no-println
  discipline becomes structural.
- **The milestone ("this afternoon means the right shape")**: a
  compound value assembled from several producers, one path selected,
  the unselected producers PROVABLY never executing (trace absence),
  through the VM and JIT. Not cargo. Green is not the metric; zero
  passing tests for a day is acceptable while the foundation is shaped
  right. "We're building a cathedral."

## 7. What v1 defers (recorded so nobody "fixes" them silently)

- Per-field struct laziness (v1: pick strict-except-trees, note in code).
- Path-demand REFINEMENT through merges/subtree chains — RECLASSIFIED by
  A4 above: not deferrable, foundational; the temporary widening remains
  only until the A4 work lands.
- Batching POLICY beyond memo boundaries (cost-model scheduling) — the
  policy socket exists (the joint-demand sets); tuning is post-keystone.
