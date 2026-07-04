# The vix lowering — machine slice 3+ (the arc's constitution)

*How vix lowers onto weavy::task. Governed by A5/A6 and the tooth-2
ABI; written before implementation so the shape can't wobble. The
acceptance is already pinned: merge-demand.vix's absolute corpus
contract — the machine runs it, left.o is ABSENT from the trace, both
lanes. Amos's line, never re-blurred: demand is vix's; fable is
imperative; never-started belongs to the demand graph, started-but-
blocked belongs to weavy::task.*

## The three spawn-suppression mechanisms (stacked, in order)

1. **Memo hit** — computed before: demand reaches a Call node, the
   closure-hash × args key hits the memo store, the node resolves with
   NO task, NO frame, nothing. (Warm daemon: most of the graph.)
2. **Undemanded** — never asked: a node no selection reaches is pure
   data forever. No task exists to suppress.
3. **Parked** — asked and waiting: only THIS mechanism costs a frame,
   and the frame chain in the arena IS the whole cost.

## Node → task

- A vix Call node (memo boundary) lowers to ONE weavy::task Fn: frame
  layout = declared record of (args, locals, spills) via mem::declared;
  body = the fn's typed ops (instruction selection from the checked
  types — vix's checker work rides the same discipline as fable's).
- The lowering is CACHED content-addressed (canonical hash → compiled
  Fn + frame layout), per the lowering-cache-is-primary ruling. The
  machine driver instantiates tasks from cached lowerings.
- Spawn happens ONLY when demand reaches the node AND the memo misses.
  Spawn = allocate entry frame, write args frame-direct (arg values
  are handles/scalars — see Values below), enter via the driver.

## Demand edge → await input

- A node body that needs another node's value awaits an INPUT SLOT;
  the machine driver owns per-task readiness/value arrays (exactly
  TaskExec's shape — the Advance seam holds either lane).
- When node B completes, the driver fills B's slot in every parked
  task awaiting it and wakes them. One completion, many resumptions —
  the demand graph's sharing, unchanged.
- JOINT NEED at memo boundaries is a SCHEDULER policy: when a body's
  input set is statically known, the driver demands the batch before
  entering, so bodies don't start-park-start-park. Policy about when
  to begin; never spawning undemanded work.
- Exec/fetch/solve runs stay DRIVER-SIDE primitives (runs registry,
  journal, two-tier cache unchanged); a run's completion feeds await
  slots exactly like a node's — the primitive reentrancy rule (V2)
  holds: primitives speak to the machine only through demanded slots.

## Values across node boundaries

- Inside a frame: untagged bytes per the tooth-2 ABI.
- ACROSS nodes: results are content-addressed store entries. A
  completed task's Ret bytes become the node's value under the node's
  result layout; an await delivers either an unboxed scalar (one word)
  or a HANDLE to a store entry (also one word). Composites never copy
  across node boundaries by value — sharing is identity in the store,
  which is also what memo/hashing/shipping already key on.
- TotalF64 canonicalization is applied at the language boundary (value
  creation), never inside machine arithmetic (IEEE there, bitwise-
  pinned across lanes).

## Let bindings are demand-sunk statically (ruled during parity B1)

The lowerer discovered eager `let` lowering violates G06/G07 (unused
binding must create nothing). The fix is NOT runtime thunks — that is
A6's banned dynamic machinery. RULING: within a body, demand is
resolved AT LOWERING TIME. A binding's initializer is emitted at the
earliest point dominating its uses on each execution path: unused →
never emitted (the ops simply don't exist in the task program —
absence at the PROGRAM level, the same proof style as everything
else); used only in some match arms → sunk into those arms (one arm
executes, so once-per-path holds); used before and after a branch →
emitted once at first-dominating use. At most one evaluation per
path; no thunks, tags, or force flags ever. Purity makes elision
unobservable except through demanded slots (V2), which is the point:
this is "the graph materializes as bodies reach call sites" carried
down INTO bodies — code no demand path reaches is never emitted.

**THE PROMOTION SLICE (Amos: "charge head-on into that situation
instead of waiting for it to creep up on us" — ruled 2026-07-04,
supersedes waiting):** build promotion NOW, deliberately, with a
forcing construct. The design that fell out of charging:

P1. ONE REPRESENTATION. PendingInvocation (fn closure-hash ×
canonical args-so-far × remaining arity) becomes a first-class
store value kind. Partial applications, closures-as-values, pending
tree branches, pending array elements, and promoted bindings are
ALL this one thing. There is no closure runtime — a closure IS a
pending invocation with remaining params; calling it supplies the
rest and demands through INVOKE. (This also discharges the O12
shipping debt by construction: pending identities are
content-addressed, hence wire-stable.)

P2. IDENTITY IS NEVER OBTAINED BY FORCING. Pending values hash by
invocation identity (input-addressing); forced values hash by
content (content-addressing). Memo keys over pending args are
therefore FINER than eventual content equality — that is the SAFE
direction (extra misses possible, wrong hits impossible). Forcing
a value in order to hash it is FORBIDDEN — it would let hashing
destroy laziness. Language-level equality (==) on possibly-pending
values is a DEMANDING operation: it forces both sides, like any
other touch. (This is the Nix-input-addressing vs
Unison-content-addressing tension, resolved by holding both with a
strict direction rule.)

P3. DEMAND-ON-TOUCH stays the one dynamic mechanism: touching a
pending handle in a demanding position goes through the driver's
INVOKE path (hit = sync fill, miss = spawn+park). Promotion adds no
machinery — it REUSES the graph.

P3-AMENDED (Amos caught the regression: "so you've reintroduced a
forcing function into the language"): the first implementation made
Option.unwrap the touch — that is `force` wearing a nicer name (one
library fn elevated to a semantic primitive, and it over-forces:
passing an unwrapped value to a callee that ignores it would run
the producer). CORRECTED RULE — **bits are strict, identities are
lazy**: demand is a TYPED COERCION the lowerer inserts exactly
where a Pending<T> flows into a bits-consuming position (arithmetic
operand, match tag read, string/path comparison, bits-typed Ret,
scalar arg-write at a memo boundary). unwrap is a pure Option-shell
op (result may stay pending); Map.get demands NOTHING (map shape is
known at construction — key-presence is structural; contrast
pending TREES where path-presence costs a run because shape IS the
producer's output: demand exactly the question asked). Composites
cross memo boundaries as identities (lazy); scalars coerce at the
boundary (strict).

P3-AMENDED-AGAIN (Amos: "you've painted yourself into a corner
where you can't represent values in the map that aren't quite
there yet" — CONCEDED; the earlier "pendingness tracked statically,
no runtime checks anywhere" was an OVERCLAIM): static tracking
holds for FRAME SLOTS; it CANNOT hold for container entries
selected by runtime data (a Map<String,Float> holding a literal
next to a promoted producer is representationally heterogeneous,
and get(k) with runtime k cannot be statically resolved). THE
HONEST MECHANISM: a pending-capable value is a DECLARED SHAPE —
Realized<T> = Ready(T) | Pending(invocation identity) —
discriminant IN LAYOUT (A6-legal: the ban is
kind-markers-on-operands, not declared discriminants). The
coercion at a container-fed bits-position is a READ BARRIER: one
discriminant branch at a COMPILER-PLACED site — Ready loads bits,
Pending INVOKEs through the driver. Statically PLACED, dynamically
RESOLVED. AddF64 still eats raw bits; barriers run before ops, at
chosen sites, never inside them. Representation is the LOWERER's
choice per context: raw bits where realization is provable (frame
scalars, strict boundary args — fib pays zero barriers),
Realized<T> where not (containers that ever receive a promoted
insert). Users never see Realized — the coloring lives in lowering
as a representation choice, not in the type system as a user
obligation (GHC boxed/unboxed with polarity flipped: flat by
default, boxed only at promotion sites). COST PATH: the barrier is
a GUARD — the gingembre-era guard/IC stencil machinery (speculate
Ready, deopt to the demand path) is the designated optimization,
already built, already DWARF'd. No operation in the language is
named or shaped like `force`.

P3-DOCTRINE (Amos: "'raw bits where provable' only works in a
universe where we don't have Eval — and we don't know that"; ruled
2026-07-04): provability is a CLOSED-WORLD notion and vix's world
is deliberately open three ways — warm reload (new code generations
into a live memo store), closures shipping across the fleet, and
the self-hosted direction (vix evaluating vix). "This container
never receives a promoted insert" quantifies over future code;
analysis can't have it. Therefore: (1) raw-bits representation is
justified by RULES, not proofs — frame-local scalar dataflow
(structural: nothing external reaches into a frame mid-execution)
and ABI-scalar strictness at boundaries (a clause, not an
inference). Rules survive open worlds. (2) Everything beyond the
rules — containers, store values, anything crossing a boundary,
reload, or wire — is Realized-capable BY DEFAULT. (3) SPECULATION
IS THE COST MODEL, not a later dial: assume Ready, guard, deopt.
Runtime facts ("Ready 10^4 times") are cheaply checkable forever
though never provable — the V8/JSC lesson, and the moment the
guard/IC/deopt investment becomes load-bearing rather than a lane.
Devirtualization-by-analysis is demoted to opportunism where a
world happens to be locally closed.

P3-ENCODING (Amos: "ABI-wise, we can pack presence bits anywhere —
[A][B][C][realization bitset]"; queued as the slice AFTER warm
reload): presence bitsets replace boxed Realized<T>. STORE SIDE
(bits are INSERT-TIME FACTS — store values are immutable, so
realization never flips a stored bit; second touches are cheap via
the memo store, not entry mutation): realized values inline as raw
bits (no per-value box/entry/indirection), pending values as
identity handles, one bit discriminates; hashing per P2 unchanged
(bit selects content-vs-identity contribution). The bitset is a
SYNTHESIZED DECLARED FIELD via mem::declared — debugger reads it
like any field. FRAME SIDE (frames are mutable scratch — bits DO
flip): lazy temporaries are (flag word, value word); barrier =
JumpIfZero on the flag, ZERO host calls on the Ready path (kills
the STORE_TAG round-trip); a demanding coercion writes bits back +
sets the flag so later touches in the body pay only the test. No
bit-packing in frames (space is cheap); packing pays in store
layouts (hashing, cache line, dedup). Semantics identical to boxed
Realized — tests change only in host-count assertions.

P-HAZARD (flagged in review, must die before warm reload): the
pending store encoding carries fn_ref — a program-table INDEX —
inside a content-addressed value. Indices go stale across reload;
resolution must go through the closure hash (identity_hash →
fn_hashes lookup), with fn_ref at most a cache invalidated by
reload.

P4. FORCING CONSTRUCT (zero grammar changes): lazy Map values —
inserts store pending refs for promotable call initializers; .get
with a computed key demands ONLY the selected entry. Test: map of
two expensive producers, computed key selects one, the other is
trace-absent, both lanes. This is data-dependent demand that
sinking can never resolve, met on our terms.

P5. First-class closure SYNTAX (callee-as-expression, closures
returned from fns) waits only on grammar admission; the value
representation ships in this slice, so grammar admission later
costs the machine nothing.

**The escalation rule (Amos flagged the cliff; ruled with them,
2026-07-04):** one more construct — first-class closures capturing
bindings, or loops — makes static demand placement research-grade
(strictness analysis / lazy code motion territory; undecidable in
general for higher-order code). We do NOT go there. When sinking
can't decide, PROMOTE: the binding lowers as a pending invocation
ref (closure-hash × captures) and demand flows through the driver's
existing INVOKE/await/memo path. Not a thunk — no tags, no force
checks; the ONE dynamic demand mechanism is the graph, and
promotion escalates to it. Analysis precision is thus a COST dial
(dumb = more frames, still lazy, still correct), never a
correctness question, never a second mechanism. Third valve: the
admission gate — no construct enters the language without a
decidable demand story or an explicit promotion rule (combinator
iteration already routes per-element demand through memo boundaries
by construction; that is why body-level loops may never need to
exist).

## Trace (the unified stream)

- The vix lowering EMITS instrumentation ops carrying the three-moment
  vocabulary: node-demanded, memo-hit/miss, spawn, park, resume,
  finish — static ids mapping to nodes/spans via the lowering's
  tables. Innards mode = full story (tests, wtf, playground lanes);
  Production strips to structural suspension events. Tests are trace
  assertions; ABSENCE is the laziness proof.

## The word "store" — one addressing scheme, three residencies

Amos flagged the collision: THE STORE, unqualified, means the
durable shared CAS for build results (artifact bytes, fleet-shared,
GC'd — the vixen store). The machine's ValueStore must always be
called the VALUE STORE. Relationship: one addressing scheme
(everything keys by content hash — values by canonical content,
pendings/memo by invocation identity, artifacts by bytes), three
residencies: (1) eval-lifetime RAM (frames/arenas — dies); (2)
daemon-lifetime value store + memo map (survives reloads — that IS
the daemon's warmth; its durability path is already ruled:
"persistence only at content-addressed boundaries: memo, journal,
lowerings" — serializing this tier is the journal/C5 story, staged
post-parity); (3) the store (durable CAS). The value store is the
SPINE (small structural values holding identities), the store is
the FLESH (artifact bytes, moved on demand); artifact bytes NEVER
enter the value store. Fleet shipping: spines travel cheap, flesh
moves on demand.

## The driver (the demand scheduler)

- Owns: the demand worklist, the memo store, the journal, the task
  set (running/parked), the readiness plumbing, the store.
- Single-threaded first, written work-queue-shaped: workers/stealing
  arrive by sharing the queue (legal: canonical order makes schedules
  unobservable). Persistence only at content-addressed boundaries:
  memo, journal, lowerings. Arena and worklists die with the eval.

## Order of implementation

1. Driver skeleton + node vocabulary over cached lowerings; scalars
   only. DONE (vix::machine::driver, d47d31cf5): the three
   suppression mechanisms trace-asserted; INVOKE protocol (memo
   boundary = HostCall+Await; hit = sync fill, miss = spawn+park).
   CORRECTION from recon: eval.vix is NOT an arithmetic core — it is
   the self-hosting shape (recursive enums, strings, Map, match) and
   belongs to slice 2/3. Slice 1's corpus program is a NEW minimal
   sample (scalar fns, lets, arithmetic, recursive memo-boundary
   calls — fib in vix) with its own pinned contract; the AST lowering
   (machine/lower.rs) targets it first.
2. Store + handles; composite results. DONE (3e5fc0d5e, de16419cc,
   fb6014559): content-hash dedup store, STORE hosts, memo-key handle
   canonicalization, canonical Maps (sorted pairs, last-wins,
   history-independent hashes), TotalF64 at boundaries/IEEE inside,
   eval.vix demo()==42.0 bits-exact on the machine.
3. Exec seam as await-fed runs; merge-demand.vix — THE MILESTONE.
   **DONE** (b7863f8d8 interp + 5387c9b95 branch stencils +
   bf3648f12 lane seam): pending tree branches ARE invocation
   identities (content = closure-hash × canonical args); collect
   never demands; `/` walks only the demanded branch; selected():
   object spawns EXACTLY once for two units, run set exactly
   {wanted.o}, left.o RunRequested-ABSENT, warm = 2-event trace —
   asserted identically on BOTH lanes (Lane::Interp | Lane::Jit
   through one LaneRuntime/LaneTask seam; burst() sees only
   advance()). fallback() pins absence-costs-a-run; subtree_chain
   clean. 212/212. Parked: JIT lane seeds root args via a
   per-invocation wrapper program (full re-JIT per spawn) because
   JitTask lacks driver-side frame seeding — fix is weavy-side
   spawn-with-initial-frame, then the wrapper dies. Parked: general
   closures (only partial-application-of-named-fn lowers).
4. Corpus parity → delete the frozen evaluators (A5's funeral
   clause). ← NEXT.
