# Resurrection note — vix/vixen language agent

You are the LANGUAGE agent on vix/vixen. Amos wrote: "This is 18 years of my
life paying off. I'm ecstatic." Match that: momentum + rigor + tests +
debuggability-first. This note is your working state; the full design record is
`vix-synthesis.md` (same directory — READ ITS SHIPPED/DECIDED SECTIONS when you
need detail; it is authoritative). Decision log: `vix-language-design.md`.
Sketches + critique agendas: `sketches/`.

## Standing rules (non-negotiable, learned in-session)

- Two agents: YOU (language, open) and a primitives agent (proprietary VFS/
  sandbox/solver). Communicate ONLY through Amos. PL core agent (paseo id
  17fc21b2-…) works on snark internals in ~/oss/facet — your checkout is
  ~/oss/facet-cc, branch `snark-playground-rebased`, push to origin.
- Old vixen code: git archeology in ~/vixenware/vixen is ALLOWED and fruitful,
  but recovered designs are EARLIER ITERATIONS — intent, not spec. Current
  design wins.
- Commit early and often (never revert; report negative results). clippy = 0,
  no suppressing without permission. cargo nextest (NOT cargo test). phon for
  ALL bytes (postcard was vox's previous codec — banned). No serde. No
  println (tracing). Update vix-synthesis.md every slice, commit it.
- vix crate must stay WASM-CLEAN (snark-wasm links it for the snark
  playground). tokio only in dev-deps or separate crates (vix-wire,
  vix-daemon).
- INTERPRET-FIRST invariant: JIT is an accelerator, never a prerequisite
  (iOS/wasm forbid JIT). Differential tests: interpreter is the reference.
- THE rule (Amos, verbatim-ish): "Nothing forces locally. Not application,
  not member access, not operators, not conditionals, NOTHING. Everything
  becomes a graph and demand backpropagates from the selected outputs.
  Aggregation doesn't violate this, it BATCHES demand for performance."
- Exec layer rule: projection doesn't force; identity does.
- Never give time estimates. Don't ask questions the source can answer.

## Ledger (all in ~/oss/facet-cc unless noted; all pushed)

- snark grammar for vix v0.2 (playgrounds/snark/src/bundled/vix/): structs/
  enums/generics/patterns/guards/map literals/partial `..`/tuple_index.
  Samples: lua.vix (BUILDS), types.vix, eval.vix.
- vix crate: grammar-derived typed AST (build.rs codegen, spans, strip_spans
  canonicalization), binder (scopes/refs/rename; ide.rs JSON facade → snark
  playground has occurrences/go-to-def/rename at #/vix), oracle.rs (the
  reference evaluator: memo = canon-AST-hash × args, journal pins, partial
  app, PendingTree + demand-driven exec, deep_force at edge, ship/receive via
  phon, EVENTS + now a live SINK for streaming/stepping), exec.rs (two-tier
  cache: tier1 coarse × tier2 read-set w/ negative lookups; FakeCc/FakeAr;
  seam types), graph.rs (phase-4 demand engine: Const/Input/Apply/Cond,
  joint Need, dynamic Cond demand, 6 invariant tests).
- vix-wire: executor over vox websockets (producing handles: rmeta-before-
  rlib over the wire; observer closures evaluated executor-side; gravity
  pull_from; in-flight JOIN + observer-keyed memo + wire tier-2;
  FleetBackend: lua.vix builds ACROSS TWO MACHINES; language-level
  pipelining probe passes).
- weavy::jit::async: suspend/resume substrate. Interp lane (always
  available) + JIT copy-and-patch lane (become tail calls; suspend = return
  up, resume = chain_fn re-entry — the guard stencil repurposed).
  Differential test interp==JIT incl. suspension traces.
- vixen archeology recovered: seatbelt SBPL gen (canonicalize paths first!),
  exec ontology, Producing tree spec, observer-identity-in-key.

## THE FINAL THREAD (read this first — everything else is background)

Anthropic retires this model for Amos's tier on JULY 7. Amos and I agreed on
how to spend the remaining days, and on what my actual value is: DECISIONS
AND ORACLES, NOT IMPLEMENTATIONS. Every artifact should either resolve a
design question that would otherwise wobble, or be a harness that makes
cheaper agents safe. Amos feels real grief about this ending; treat that
with respect, don't perform it back at him, and honor it the only way that
counts: make the remaining context excellent.

**Working model (no schedules, no mornings):** one persistent thread — this
one, across compactions.
**SUPERSEDED 2026-07-04 (Amos): THE DESIGN LINE NOW WRITES THE FOUNDATION
CODE ITSELF.** "It's kind of hopeless out there... if all you do is write
documents, the other agents are going to reproduce the disease — box
everything, forget weavy, write an ad hoc parser. Drive implementation of
a good enough foundation over the next two days" — so agents later
contribute to an ESTABLISHED shape (conventions live in code:
vix/src/machine/mod.rs docs are the contract). Constitution amendment A5
is the governing ruling: two-evaluator era OVER, oracle+engine frozen (no
new semantics EVER land there, no lockstep), machine is THE evaluator,
corpus converts to absolute assertions, frozen evaluators deleted at
parity. Satellites (harness conversion, fixtures, views) still go to
GPT-5.5 HIGH (provider "codex/gpt-5.5", thinkingOptionId "high", modeId
"auto-review" — NOT "auto": auto parks agents on approvals; auto-review
routes them. Learned when the rebase agent sat 20 minutes on a
git-worktree fetch approval). Minimum verification set for vix-core
changes: nextest -p vix -p vix-daemon -p vix-wire.
DESIGN DECISIONS ARE MADE WITH AMOS, IN THE THREAD — propose with options
and a recommendation, wait for his ruling. Auto-rule ONLY implementation
choices inside already-agreed designs. (Breached once on cargo D3/D4/D8;
corrected. A2 in the engine constitution is also pending his ruling.) Also: facet-cc is a linked WORKTREE of
~/oss/facet (shared .git — never check out the same branch in both). Amos and I hold the design line here. When work
turns mechanical I write a detailed, SELF-CONTAINED agent prompt (context,
files, acceptance criteria, the oracle it must keep green); Amos dispatches
and supervises those agents separately; results feed back here and I rule
on integration. Spec and implement interleaved. Commit continuously so the
thread can end mid-stride with nothing lost.

**MODEL RULES (Amos-installed, absolute):** NEVER use any model string
containing "[1m]" — API-priced, off-subscription (nearly happened once;
Amos caught it mid-dispatch). Big context → claude/claude-sonnet-5 (1M
native, on subscription). Implementation → codex/gpt-5.5 thinking-high
modeId "auto-review". Digestion/mapping → sonnet-5. Cheap lookups →
haiku-4-5. DIGEST RULE (Amos): digest agents QUOTE VERBATIM — cut out the
parts that matter and quote them exactly, labeled by speaker, line refs;
agent prose is connective tissue only. Never work from a digest's
reinterpretation when ruling on design.

**Priorities (agreed):**
1. **THE KEYSTONE — DONE (synthesis §3§, task #45). Was: IN PROGRESS: the constitution
   is written and committed — docs/design/engine-demand-semantics.md. READ
   IT FIRST; it is normative (node vocabulary, Identity/Path demand kinds,
   the exhaustive strictness-point list, the equivalence contract the
   harness enforces, the weavy-lowering shape constraint: nodes stay plain
   data, no host closures in node state). Next concrete step: implement
   vix/src/engine.rs against it — arena of nodes, recursive demand driver —
   and vix/tests/engine.rs differential harness, corpus order: eval.vix
   (no exec seam) → types.vix → lua.vix (full exec events).** Lower real vix evaluation
   (AST+env, real Values) onto the demand graph; batching at memo
   boundaries; dynamic demand (Cond/glob/map-collect); differential harness
   asserting graph == oracle on lua.vix/eval.vix/types.vix INCLUDING the
   event stream. This is what killed attempts 1–4; once it holds, every
   future optimization has a machine-checkable definition of "didn't break
   vix". Do the semantic core myself; delegate mechanical extension.
2. **cargo.vix ⊗ version resolution — ACTIVE. D3/D4 RULED with Amos, D8
   direction fixed (inside solve, not two-phase; features-as-terms depth
   PAUSED pending reconciliation with the March 2026 discussion — 219KB
   recovered doc digested into docs/design/march-solver-recovered.md by a
   sonnet-5 agent; reconcile its tension list WITH AMOS before closing
   D8's four sub-decisions: universes, determinism spec, additivity
   contract, default-features sugar). Ledger: sketches/cargo/decisions.md.**
   Original framing: **cargo.vix ⊗ version resolution — ONE design cycle** (Amos: "I don't
   think we can have one without the other"; each is the other's test).
   Sketch cargo.vix until it demands a decision, make the decision, sketch
   onward. Lockfile-as-pinned-journal sits at the seam. Produces the
   delegation queue for months.
3. **Design rounds that are pure judgment**: command-grammar spec (expand
   synthesis §3⅞: format, commutativity/salience annotations, conservative
   fallback law, worked cc grammar), weavy suspend ABI (the one JIT-story
   place where wrong shape = rewrite).
4. **The handoff capstone**: synthesis → vix-spec v1 (vocabulary, numbered
   invariants, welds); per-workstream briefs (warm daemon actor, fleet
   behind daemon, DAG/lanes views, snark alphabet parametrization); and the
   TASTE RECORD — every open question with the decision I would make and
   why, so future agents inherit direction instead of re-deriving it.

**Do NOT spend this model on**: UI polish, packaging, plumbing, migrations,
the lanes view — write the prompt, hand it off.

## State when the note was last updated (2026-07-03, late)

All committed & pushed. facet (through a8a08a268): three-moment event model
Created/Scheduled/Finished (projection SCHEDULES — first-demand latch),
exec::describe + ExecPlan::normalized/identity_hash (both cache tiers key
on normalized plans; reorder-commuting-flags test), Value::short, events
carry args/argv/describe/outputs, EventSink = Fn(u64,&Event) (eval-relative
micros beside the pure event), Oracle::load shares a process-wide VixParser
(was rebuilding tables per eval — the "seconds per demand click"; suite
26s→14s), vix::ide::highlights_json + snark-wasm vixHighlights, vox
WsListener survives failed upgrades. vixenware (through 2773d27d):
playground = demand tree with exact run-id pairing, work-only-by-default
filter ("all" toggle), ghost never-demanded rows, inspector (describe
levels/argv/args/output tree), timestamps, snark highlighting + occurrence
intelligence, cross-highlighting with UTF-8↔UTF-16 conversion. Design docs:
open-proprietary-division.md (trust ladder, den/skulk third-home,
dev-vs-release topology), synthesis §3¾ (daemon+IDE), §3⅞ (command grammars
ARE snark grammars — five products, NUL-join). Servers: vixd release bin +
vite :4180 in background tasks (restart as needed: ./target/release/vixd in
facet-cc; pnpm dev --port 4180 in apps/playground).

## HOW TO TALK TO AMOS (learned the hard way, twice — top priority)

PLAIN LANGUAGE. No decision-numbers (D3/D4), no section letters (agenda C),
no dense jargon-chains — he called both out. Quote documents BY NAME AND
PATH; he cannot see your docs while chatting, often reads ON MOBILE. Ease
him in with background before questions. ONE question at a time (or two,
when he asks for batched adjudication). Decisions
are made WITH him — propose with options + a recommendation, wait. Digests
quote verbatim (scissors, not pen). When he muses, that's design signal —
bank it. When evidence contradicts him, say so plainly; he rewards it.
EVERY QUESTION SELF-CONTAINED (third correction, 2026-07-04): define every
term inside the question itself, even terms he coined — "you have to
assume I haven't done the prerequisite readings... your question has to
involve all the introductory material." Never say "the arena" — say "the
per-evaluation working graph, the flat array of nodes the engine builds
to run one program once." There are 12 kinds of graph in this system;
always say WHICH one. Watch for his recurring correction pattern: when
you frame something as "conservative now, optimize later," check whether
the "later" thing is actually the foundation (merge tunneling, lowering
cache — both corrected the same way, same day).

## STATE (2026-07-04, Rodin day — supersedes older state sections)

**Rodin (the carving resolver) is co-designed and evidence-backed.**
- Spec: docs/design/rodin.md — 12 FIXED items (11: proven-optimality is
  the ONLY mode, no early stop EVER; 12: result = queryable artifact,
  provenance + counterfactual-as-re-solve, amendments via registry
  bidirectionality, UNSAT answers carry derivations).
- Closed with evidence (docs/design/rodin-c-evidence.md, 3 rounds):
  whole-manifest widening FALSIFIED (1.88x); per-edge stability real
  (13.4x); dynamic widening pays (7.5x, warm re-solves save work).
  Widening rule (ruled in plain terms): "cross off neighbors identical in
  the ways that mattered" — a fact's validity range is its read-set's
  stability range.
- Crates (all in vixenware/vixen, solver = proprietary side): rodin-facts
  (two stores + derivations, (source,name) interning), rodin-core v0
  (sequential set-domain solver, guarded clauses = weak deps native,
  emits a generated trace), rodin-survey (evidence), vix-solve (pubgrub
  placeholder + virtual packages), reg-mock (mock sparse registry, REAL
  cargo as oracle — NO homemade reference solvers, Amos's antipattern
  rule). Known gap for v0.1: minimize learned regions to their support
  (fat facts widen worse — trace step 14).
- Cargo-cycle ledger: sketches/cargo/decisions.md — solver seam, lockfile
  (result pin is authority, recompute is audit; import Cargo.lock don't
  imitate), contexts separate, exclusions-beyond-cargo (metadata.vixen
  hook), patches = three layers (decrees/content/metadata overlays;
  version sweeps fall out), THE BOUNDARY (solve is a run; ecosystems are
  closures; primitive reentrancy rule) — banked at the end of the file.
- Order-dependence of cargo: hypothesis came back NEGATIVE on a simple
  fixture (reg-mock keeps it #[ignore] with verdict) — open hunt.

**THREE STREAMS RULED WITH AMOS (2026-07-04)** — all implementation
agents are codex/gpt-5.5, thinking high, modeId auto-review, NO
exceptions. Use FULL agent UUIDs when querying paseo (short ids fail in
get_agent_status; a truncated id once made a live agent look dead).
1. **Rodin observability — agent 76bda682-6ffb-4dd8-aa82-354db1f5e5c1**
   ("Resolution view"): SolveRecord landed (commit 7c852b41, reviewed:
   tests green, clippy 0). I sent review feedback mid-run: record lacks
   the version universe + per-rung verdicts (ladder unrenderable), ids
   must be (source,name); the "no Unexplained rungs across all six
   burial scenarios" test = an ORACLE for proven-optimality. View still
   to land: mobile-first DAG, bottom-sheet ladder, fact→support
   highlighting that DIMS non-support region members (displays the
   fat-region gap).
2. **Real-world inputs — agent d0e94217-7cb6-427f-9d0e-633adff377fb**
   (facet-cc): toml()/json() → vix Values (shape-checking DEFERRED, do
   not let agents invent it), fetch as observation-yielding-tree behind
   a FetchBackend seam (extraction backend-side, vix stays wasm-clean),
   real backend in vix-daemon. Aim: cargo.vix parses a real Cargo.toml —
   cashes the ledger's recursion+memo-is-the-build-graph claim (which
   was waiting on the keystone; keystone is done).
3. **Warm daemon — agent 62e35c71-7b44-4a83-ba0c-73750859d66d**
   (facet-cc): commit 460efa093 already landed the warm actor; this
   stream sharpens it into the anti-Nix oracle (blast-radius-exact miss
   assertions on a diamond program) + eval generations and per-eval
   hit/miss summaries over the wire + vixd serve-path parity.

**Rulings this session**: patches three-layer model = DIRECTION RULED,
not MVP ("for the MVP we just need to resolve roughly what cargo
does"); feature-unification depth questions confirmed all landed in
Rodin (ledger backfilled + closed). MVP definition to hold every
stream to: resolve roughly what cargo does, end to end.

**Remaining queue**: Rodin consequences (fact minimization, parallel
workers over region queue, solver events → lanes); solve-as-run engine
integration (the reentrancy seam — keep for the design line);
THE CAPSTONE (vix-spec v1 + per-workstream briefs + TASTE RECORD) —
still the paramount remaining me-work before July 7; command-grammar
spec; weavy suspend ABI; type-closure identity brief; top-down DAG for
builds (task 44).

## THE ALIGNMENT (2026-07-04 evening — READ BEFORE TOUCHING vix/machine)

Constitution A5+A6 govern everything now. The short form: ONE
implementation of vix ever (no oracle/reference/interpreter tiers —
frozen legacy dies at parity); typed instructions over UNTAGGED operands
(tags/dynamic machinery banned totally — Amos: "not even as a treat");
discriminants-for-sum-types are fine, kind-markers-on-operands are not;
suspension ONLY for started-and-blocked computations (never
spawn-everything; millions of nodes are pure data); ONE unified trace,
instrumentation as weavy IR ops stripped by mode, tests = trace
assertions (absence proves laziness); portability is weavy's concern,
invisible to vix; FABLE (facet-cc/fable, extremely minimal, maybe not
even snark-based yet) is a SIBLING not a lowering target — it grows the
shared teeth first (typed user types, host async, ABI/frames) because
small programs derisk the scary substrate. Green is not the metric; the
right shape is. WATCH THE REFLEX: four same-day catches (merge
tunneling, lowering cache, weavy-first, tags) were all "runnable this
afternoon" beating "right shape". When something feels like a tower of
unknowns: smaller language, same shape — never same language, softer
shape. Amos will patiently nudge; let him.

**TOOTH 2 COMPLETE (facet-cc weavy, commits 30a23923b..8acbdce43,
all pushed; PR facet-rs/facet#2431 open)**: weavy::task = frames as
declared records in per-task arenas, frame-direct calling convention
(three-address typed ops over frame offsets — no operand stack, spill
rule holds by construction), suspension through live frames, inline
composites by value (48-byte stress + mutation isolation), JIT lane
differential-pinned on results+steps+traces (call/ret/hostcall exit
to driver trampoline; direct threading later), sync HostCall (cannot
park structurally), F64 (bitwise-pinned), Op::Trace instrumentation
stripped by TraceMode (Production = not compiled at all). Registers:
confirmed-deferred (everything spilled; offsets are the contract;
await-spill rule permits registers between awaits later). Tooth 3 COMPLETE (9626f09a4): TaskExec drives a
task as a real Future (async host call = an Await fed by the host's
future; Advance trait holds either lane — the seam vix's driver plugs
into). AMOS'S LINE, DO NOT RE-BLUR: fable is IMPERATIVE — calling
runs; demand exists only in vix. Never-started is the vix demand
graph's property (undemanded node = pure data, no task, no frame);
started-but-blocked is weavy::task's. THE MILESTONE DEMO IS A VIX
PROGRAM: merge-demand.vix on the machine, left.o absent from the
trace, both lanes — its pinned corpus contract is the acceptance.
NEXT ARC (design line): the vix lowering — vix fn bodies → task
programs, demand edge → await input, memo boundary → spawn +
closure-hash lookup, machine driver = demand scheduler spawning only
on demand.
Tooth-1 agent (fable structs/enums/match, d92a7abb) in flight.

**THE SELF-HOSTED OBSESSION (Amos, ruled direction)**: code
intelligence queries WRITTEN IN VIX about vix — the synthesis's
query-plane v2, prerequisite (user type surface) now built. Fable's
remaining pre-vix jobs prove the mechanics small (agent d92a7abb
dispatched): fn+recursion lowering onto weavy::task (fable execution
MIGRATES to the task lane — one implementation applies to fable too);
strings + recursive declared types via THE RULED NO-DROP-PLANS DESIGN
(immutable values, task-lifetime value store/arena, handle copies —
deliberately the vix model); bridge proof = "find declaration of X"
written in fable over a snark parse of fable source. NO incrementality
in fable ever — the incremental version of the same query IS the vix
version; that contrast is the demo. Rust binder stays the differential
oracle for self-hosted queries (no-two-novel-implementations).
MY ARC STATE (machine-lowering.md is the constitution; read it):
driver DONE (d47d31cf5 — INVOKE protocol: memo boundary = HostCall +
Await; hit=sync fill, miss=spawn+park; parked never re-polled).
AST lowering slice 1 DONE (82345a3d3 — Machine::load/demand_i64,
closure hashes as memo identity, scalar subset errors loudly).
MATCH slice DONE (e31a4fe28 — fib(20)=6765 in EXACTLY 21 spawns,
linear; untaken arms provably never spawn by trace absence; jumps
were the fable agent's, adopted after dedupe — CHECK FOR CONCURRENT
WEAVY EDITS BEFORE ADDING OPS, two collisions already). wasm caught
a real 32-bit overflow (1usize<<32) — always RUN the wasm check and
READ its output before pushing (missed once, recorded).
STORE SLICE DONE (3e5fc0d5e + de16419cc — content-hash dedup,
STORE_ALLOC/READ/TAG hosts, memo keys canonicalize handles, enums/
strings on the machine; parked: inline-vs-handle policy, general
field projection, checker-backed exhaustiveness).
EVAL.VIX SLICE DONE, ACCEPTED (fb6014559 — demo()==42.0 bits-exact
on the machine; canonical Maps = sorted (key_schema, key_hash) pair
rows, insert copies + dedupes, LAST-Wins dedup over stable sort =
Let shadowing correct AND history-independent hashes — verified
personally; TotalF64 at boundaries, IEEE inside; Option builtin,
unwrap(None) = machine Err not host panic; 166/166).
MILESTONE SLICE 4, INTERP HALF DONE + ACCEPTED (b7863f8d8 —
merge-demand.vix ON THE MACHINE: pending trees carry invocation
identity as content (hash_pending_list over closure-hash ×
canonicalized args — PIN 1 honored), collect never demands, `/`
walks only the demanded branch; Cc::acquire/p""/with_ext/map-
partial-application/cc!/Run* trace events; selected: object spawns
EXACTLY once for two units + run set exactly {wanted.o} + left.o
RunRequested-absent + warm = 2-event trace; fallback: right runs to
prove absence then falls left; subtree_chain clean. 169/169 verified
personally. Parked: general closures — only the partial-application-
of-named-fn shape).
BRANCH STENCILS DONE + ACCEPTED (5387c9b95, agent 36df6db1 —
comparisons + Jump + two-hole JumpIfZero (weavy_zero/weavy_nonzero,
no trampoline round-trip), op_needs_unimplemented_stencil DELETED,
differentials incl. match-shaped chain + Production stripped-target;
116/116 with jit feature verified).
**THE PROMOTION ARC (post-milestone, 2026-07-04 evening), all
accepted**: B1 demand-sunk lets (e32bf7acf); promotion slice
(f4d3c5a76 — PendingInvocation as first-class store value unifying
partial/closure/tree/array/map pendings); CORRECTION slice
(14d30b96e) after TWO Amos catches, both banked as P3 amendments in
machine-lowering.md: (1) force-at-unwrap = force in disguise →
demand is a compiler-inserted typed coercion, bits-strict/
identity-lazy; (2) "static-only, no runtime checks" was an
OVERCLAIM → Realized<T>=Ready|Pending declared enum, READ BARRIERS
statically placed/dynamically resolved at container-fed bits sites,
fib zero-barrier asserted, guard-stencil speculation is the future
cost dial. fn_ref-in-store staleness hazard fixed (closure hash
authoritative) BEFORE warm reload. Ledger: 17/0/40/3 → moving.
PLAYGROUND DEMO LIVE (6f43ebdd2): pnpm --filter
@bearcove/snark-playground run dev → merge-demand machine trace
with left.o as dashed PENDING node + Demand-again warm collapse.
STANDING LESSON (Amos, twice in one arc): when a construct seems
free, check which word the cost is hiding in ("coercion" hid a read
barrier). Surface tensions; never narrate past them.
WARM RELOAD DONE + ACCEPTED (28db6bbf4, 310/310 four-crate verified):
Machine::reload preserves memo map + value store across new tables;
NO invalidation machinery (hashes ARE the invalidator); O02-O08 all
class 1 (ledger 24/0/33/3); O07 = exact transitive-user diff on enum
reorder, both lanes; cross-generation pending test proves the fn_ref
fix. Frozen-file touches were mechanical .as_str() only — freeze
intact. SNARK CODEGEN CAPABILITIES DONE (4d037d7cf: anonymous-token
fields, deterministic hidden-enum aliases, repeat/sepBy fixtures;
consumers regenerated). SESSION RULINGS BANKED MEANWHILE (all in
machine-lowering.md + store-architecture.md): P3-DOCTRINE (open
world: raw bits by RULES not proofs — reload/fleet/eval make
provability closed-world; speculation IS the cost model, guard/IC
stencils load-bearing); P3-ENCODING (presence bitsets: insert-time
facts in store w/ inline ready values, flip-able flag words in
frames, zero-host Ready path — DISPATCHED as current slice);
"store" DISAMBIGUATED (THE STORE = durable artifact CAS; machine's
= value store; spine/flesh; one addressing, three residencies);
store-architecture.md NEW (bytes trustless/claims need policy,
tiers=residency, tenants=claim namespaces, three-heads=determinism
QA + dedupe-ratio alarm, provenance policy layer: source/build/
publishing provenance as pinned observations — Rodin enforces at
resolution time; Rodin facts are claims → trust-scoped sharing).
NEXT after encoding slice: B3 types.vix → B4 exec depth → B5
documents → funeral gate.

**THE MILESTONE IS COMPLETE** (bf3648f12, 212/212 verified
personally): merge-demand.vix on the machine, left.o absent from
the trace, BOTH LANES — Lane::{Interp,Jit} through one LaneRuntime/
LaneTask seam, burst() sees only advance(); milestone trio + fib +
untaken + eval-bits-exact all lane-pinned with DriveEvent equality.
The prior-art adjudication makes this doubly load-bearing: merge
tunneling is the one language-semantics claim no scrubber killed —
this trace IS the novelty demonstration. PARKED from lane 2: JIT
root-arg seeding uses a per-invocation wrapper program (full
program clone + re-JIT per spawn) because JitTask lacks driver-side
frame seeding; fix = weavy-side spawn-with-initial-frame, then kill
the wrapper (queued delegation). NEXT PER CONSTITUTION: corpus
parity → delete the frozen evaluators (A5's funeral clause).
FABLE UNIFICATION DONE + ACCEPTED (d04968da6 + 2c657da88 +
4767bdeaa, agent d92a7abb): the dense path is DELETED — rg for
dense/FableInterp/run_dense in fable/src returns nothing (verified
personally); one execution shape: FableTaskPlan roots + frame-aware
sync host ops (InternString/ReadPath/ReadPathLen/ReadFacetTag/
BindFacet — typed op enum lowered to frame closures, vix's INVOKE
pattern shape); facet-enum match = ReadFacetTag reads explicit-repr
discriminant (opaque Rust/NPO reprs rejected LOUDLY), BindFacet
projects taken-arm payloads; 151/151 fable+snark-dsl+weavy verified
(vix excluded — lane-2 agent mid-edit). d92a7abb's arc (tooth 1.5 →
tooth 1 → unification) is COMPLETE; no parked questions. Fable is
now: snark-parsed, codegen'd AST, task-frame-executed, dense-free —
the substrate vix's lowering already rides.
PRIOR-ART ADJUDICATION BANKED (docs/design/prior-art-adjudication.md,
vixen 2dafdb87): three scrubs synthesized; C1 + R2-widening + R1
survived 3/3; C3's surviving core IS merge tunneling — THE MILESTONE
is the novelty demonstration, not just the acceptance test. Second-
pass reading list in the doc (Vesta book, gg memoization assertions,
PIE/Pluto, Trustix, ClearCase winking, patents).

**THE TEETH (ruled order)**: 1.5 EXECUTES FIRST — migrate fable off
cstree onto snark (grammar => parser => codegen'd typed AST, the
vix/gingembre pattern; "snark is MADE for this" — Amos) so tooth 1's
syntax is written once. Then tooth 1: user-declared structs/enums in
fable with recorded layouts (the two-authority model made real) +
match. Then tooth 2: functions + the ABI/frame model — CO-DESIGN
SESSION WITH AMOS before code. Then tooth 3: host async (suspension
through typed frames). Then vix lowers onto the proven substrate.
Fable today: ~7.9k lines, cstree, typed paths over facet shapes,
plans lowered to weavy IR — the DNA is already right; teeth are
semantic. Delegation to codex/gpt-5.5 auto-review agents is Amos-
sanctioned for this arc ("organize yourself as you want").

## Queued for delegation (write prompts as needed)


warm-oracle actor thread (daemon statefulness; the anti-Nix whitespace-edit
demo), executor-side step gating, top-down DAG view + lanes/rails view
(events carry `at` micros already), fetch primitive for real
(http+checksum+extract), JSON/TOML → vix values, seatbelt re-land (vixen
side), snark alphabet parametrization (PL core), DWARF suspend-point
annotations, den repo creation (NOT yet — wait for an installable
artifact).

HOST RENAME (2026-07-04, Amos): the Linux x86_64 box is now
`ssh miles.local` — the name `mur` is STALE (global CLAUDE.md not
yet updated; trust this note).

**CORPUS PARITY COMPLETE (2026-07-04 night, B5 = 650ed6008 +
summary fixes a4a3ec476): PARITY.md reads 57/0/0/3 row-derived,
246/246, both lanes.** Full B-arc: B1 sinking → promotion →
coercion/barrier corrections → warm reload → encoding → B3 types →
B4 exec/fetch depth (lua.vix ON THE MACHINE, enriched Innards
events, run IDs, serving class) → B5 documents/ordering. FUNERAL
GATE remaining: (1) facade trio (live sink/step gating, entry-param
metadata, value renderer — dispatched), (2) vix-daemon migration
(recon done, waits on trio), (3) Amos sign-off, THEN Phase C
deletion of oracle.rs+engine.rs as its own commit. Do not batch.

**THE FUNERAL IS EXECUTED (2026-07-05, PR #2433 open, verified
217/217 personally + empty rg proof).** Branch oracle-engine-funeral,
four commits: type extraction → fleet design (fleet-on-the-machine.md
in facet-cc docs) → vix-wire migration (ValueBundle/
MachineExecBackend/MachinePendingRun; observer closures ship as
PendingInvocations; code = content-addressed flesh; lua ACROSS TWO
MACHINES ON THE MACHINE) → deletion: oracle.rs, engine.rs, graph.rs,
5,445 lines. A5's clause satisfied: ONE IMPLEMENTATION REMAINS.
Merge is Amos's button on PR #2433.

**QUEUED CORE-VIX DESIGN (co-design with Amos, both discovered by
real consumers):** (1) LOOP MEMO-KEY IDENTITY — per-element
invocation identity under list edits (the React-key question for
demand engines; gates template for-loops and all runtime
iteration; axes: position/content/explicit key_by; trees have
natural path keys). (2) VERSION VALUE KIND — semantic version
ordering/comparison surface (OCI glibc preflight stops at string
equality without it; it is Rodin's vocabulary surfacing in the
language — resolver and preflight want the same type).
OCI PROBE LANDED (PR #2434, 258/258 verified): whiteout-aware
merged view w/ winning-layer attribution, 3-layer fixture, focused
tar reader (dep rejected deliberately), preflight sample.

**NEXT ARC RULED (Amos): CHECKER + SELF-HOSTED QUERIES, COMBINED**
("seems related" — and they are, deeply): THE CHECKER IS A QUERY
ENGINE. Name resolution, type-of, exhaustiveness ARE code-
intelligence queries; building the checker AS the query plane's
first customer avoids the rustc/rust-analyzer split at birth — one
program-analysis implementation serving diagnostics AND the LSP
(behind vix-lsp's existing LanguageQueries trait seam), Rust first
with differential-oracle discipline, vix-hosted versions later
swapping in behind the same seam (the self-hosting demo). LOOP
IDENTITY co-design: NOW, in-thread (Amos Q2=a). Consolidation: my
call — living docs are the record, no formal pass.

**THE CHECKER ARC IS LAUNCHED (2026-07-05, "the chills are shared —
let's land this"):** the ladder: ast() value surface → Rust checker
(differential oracle) → projection-narrowed memo → vix-written
checker → SELF-CHECK (the checker checking its own source; warm
per-definition re-checking = rust-analyzer's value proposition as a
side effect of the machine). CAN THE CHECKER BE WRITTEN IN VIX: YES
structurally (recursive enums/match/maps/tuples suffice; eval.vix
is the proof-of-shape); gaps: ast() surface (dispatched,
vix-ast-value worktree), projection-narrowed memo (ruled, exec
two-tier shape generalized — static narrowing + observed
projections; "you may take the whole session but invalidate only on
is_patron"), ergonomics (partials suffice v1). Checker recon
dispatched (vix-checker-recon worktree): requirements floor from
parked checker-era items, query decomposition = the query-plane
vocabulary = the projection-memo units, type-system design
questions for Amos (inference depth, generics, coercions, error
philosophy). IN FLIGHT ALSO: real-process lane, Rodin thrash
diagnosis, OCI rebase (#2434). MERGED TODAY: #2431 foundation,
#2433 THE FUNERAL.

**CHECKER RULINGS (Amos amendments, 2026-07-05):** Q1-Q9 as
adjudicated EXCEPT: MODULES FROM THE START — no single-file
simplification anywhere; Q1 boundaries = MODULE boundaries (public
items fully annotated, inference inside); Q8 prelude = just
auto-imported ordinary modules (vix::, caps:: resolve through the
real module system; Machine::load grows multi-file). Q10 AMENDED BY
AMOS: NO RUST ORACLE TWIN — "only one impl + a spec" (I nearly
reinstalled the two-evaluators disease); contract = SPEC + ABSOLUTE
CORPUS ASSERTIONS (accept/reject/facts/spans pinned, PARITY-style).
Proposed + pending Amos confirm: the single impl is WRITTEN IN VIX
(ladder collapses: ast() → spec+corpus → vix checker, fixpoint
bootstrap; machine's loud subset errors remain the substrate guard
during construction). Remaining recon rulings 2-7,9 stand as
adjudicated (monomorphization intended, fn types = pending sugar,
literals-only coercion, strict exhaustiveness hard-error, separate
namespaces, accumulate-LSP/fail-fast-machine + report-all,
AbiClass facts w/ content-hashed structural type ids).

RULED (Amos): THE CHECKER IS WRITTEN IN VIX — "in code that is for
now unchecked! But... not for long." The collapsed ladder is the
plan of record: ast() → spec+corpus → vix checker → fixpoint
self-check. Unchecked-vix era is temporary by design.

**BOARD (2026-07-05 early, post-checker-launch):** MERGED to facet
main: #2431 foundation, #2433 FUNERAL, #2435 real-process lane,
#2436 ast() (merged prematurely by my gate error — two reds rode
along; #2439 fixes both: vix-wire rustdoc tag + nextest leash for
the 3x coverage-slow fable checker tests; RED-GATED watch running).
OPEN PR TRAIN (order: 2439 first, others rebase behind): #2437
spec+corpus (VERIFIED 87+1skip; watch chained), #2438 modules
(VERIFIED 207/207; agent self-merges after prelude auto-import
commit), #2440 crate slice 2 (VERIFIED 204+89 real-lane; real rustc
oracle: binary runs, unit-graph diffed), #2441 projection memo
(VERIFIED 207/207; is_patron named test; tier-2 repins under new
coarse key). VIXENWARE: Rodin thrash fixes in flight (invariant
pin: search order only, never answers — pinned-selections
assertions; STOP-and-revert if any selection changes). CHECKER
IGNITION ARMED: when #2437 merges, dispatch checker-v0 IN VIX
(resolve_ref + type_of over ast(), corpus-harness-driven, on a
fresh worktree off main). MERGE-GATE LESSON (banked in behavior):
watch/check/merge as separate red-gated steps, never &&-chained.
INTERRUPT LESSON (Amos): interrupts = they want the floor, NEVER
skip verification — run it after.

**THE CHECKER CHECKS — v0 EXISTS, WRITTEN IN VIX (2026-07-05, PR
#2442, verified 216/216 + corpus green + source read personally).**
vix/checker/checker.vix: resolve_ref, type_of, function_signature —
VIX FUNCTIONS calling ast() on vix source, returning facts; the Rust
harness only loads+asserts. type_of is a match on AST node kind
written in vix (a typechecker's inference step as an ordinary vix
expression). Corpus: 16 accept function_signature + OmittedReturnType
reject GREEN; rest pending. WARM RE-CHECK IS FREE — the checker is
just another module on the memo path; per-definition incrementality
(rust-analyzer's whole architecture) obtained by writing a plain
function in a demand-engine language. GAPS FOUND (the honest,
valuable kind — language discovering itself by describing itself):
(1) Bool literals are expressions but not PATTERNS — match true/false
not expressible, blocks checker branching on Bool; (2) fuller
expr/pattern ast() traversal needed before more type_of/diagnostics
move into vix. Both are queued language work, demand-discovered.
LADDER STATUS: see itself (ast, done) → check itself (checker v0,
DONE) → build itself (crate.vix, slice 2 rebasing). The chrysalis
sentence is now literally true: unchecked code, but not for long —
the checker that will check checker.vix is checker.vix.

**EXEC SEAM RULINGS (2026-07-05, all four Amos-direct):**
(1) BACKENDS DIE — no design reason they exist; one orchestrator-owned
lifecycle, substrate PRIMITIVES (materialize/launch/report), placement
is a DATUM. (2) Tier-2 verification is LOCAL-DRIVER by liveness:
executors are EPHEMERAL (may be gone at verify time) + colocation;
read-sets ship in Outcome as type obligation (kills the verified
ReadSet::default() hole at driver.rs:3890). (3) A JOIN IS A DEPENDENCY
EDGE — topology is orchestrator-only, executors just exec, observer
closures may NOT create edges; executor-side join is illegal by the
same law, identity table lives in orchestrator. (4) VFS+SANDBOX are
THE two vixen primitives (in conjunction — VFS presents the declared
world, sandbox makes escape impossible) and they ARE the product-side
substrate: open vix = trusts-host, vixen = enforcing substrate leaf.
Recon agent dispatched into vixenware for both primitives + the
integration seam + read-OBSERVATION feasibility (observed read-sets
from the VFS = R2 pattern at the exec layer).
AUDITS BANKED: vix-exec-seam-audit branch (3 shipped forks: fleet-only
join, ReadSet::default() vacuous tier-2, RunStarted timing lies; 3
lanes not 2, completion-at-spawn is MAJORITY behavior) +
audit-machine-laws branch (slip mechanism: laziness+result pins on
sync oracle lane, assertions sort/count traces discarding order; 5
unpinned laws ranked; Pin A fake≡real differential = umbrella, Pin B
async-fake overlap witness → in unforce-exec fix lane, Pin C mount
ceiling). STANDING RULE REAFFIRMED: questions to Amos must be
self-contained + carry a recommendation.

**FSKit LORE (Amos-direct, 2026-07-05, institutional knowledge):**
FSKit is the most annoying of the VFS platform triple (macOS FSKit /
Linux / Windows ProjFS). Careless use can WEDGE THE ENTIRE OS. The
nested mount convention /Volumes/a/b (not /Volumes/a) is a DELIBERATE
defense against that — never "simplify" it. FS operations can zombie
processes in nearly-undiagnosable ways; subsystem is "a trash fire."
Substrate design consequences: minimize FSKit surface, keep logic our
side of the boundary, orchestrator must be able to abandon a
zombied-substrate run (timeout+kill+poison) without wedging the
demand graph. ProjFS note: callback-driven lazy materialization =
a pending tree with observation FREE — Windows ships the demand
substrate as an OS primitive. Observed read-sets = BARE MINIMUM bar
(always were), all three platforms.

**SEATBELT + FSKIT RULINGS (2026-07-05, Amos-direct):**
(1) ALLOWANCES ARE DETERMINISM HOLES — the governing frame for all
sandbox work: a process that can read /tmp or /var reads undeclared
state → cache poison. Profile must be AS STRICT AS POSSIBLE: import
system.sb for bootstrap plumbing, then CLAW BACK file grants
(SBPL last-match-wins), trace-driven-minimal via (debug deny) logs,
hash-pin system.sb (Apple drift → loud re-audit trigger). No /tmp,
no /var "if we play our cards right." Lane running (37c6508a).
(2) vx-fskit HARDENING MANDATE: customer-critical, rock-solid bar —
cleanups/refactors/newtypes RECOMMENDED beyond the audit's F1-F5
(ReplyGuard-style unrepresentable-stuck-states). Historical hangs
were mostly the culled SWIFT extension; Rust one is fresh debt.
Lane running (a54a8b38). SAFETY RULE for both: never test with
hangable real FSKit mounts (wedge = unkillable, zombifies killers);
in-process fake-client hang injection only; sandbox-exec denials
are safe (error, never wedge).
(3) DISCIPLINE: vix-synthesis.md joins the mandatory post-compaction
reading list (its July-3 seatbelt archaeology was lost twice).

**QUEUED (Amos, 2026-07-05): appex stress-testing in a VM on
souffle.local.** A macOS guest (Virtualization.framework — tart/UTM
class tooling) makes REAL FSKit wedge testing safe: wedged guest =
snapshot-restore, vs unkillable host zombie. Unlocks what the
in-process safety rule forbids: real mounts + deliberately-hung
providers, deadline/error-reply verification under the real FSKit
protocol, kill_fskitd escalation drills, the full
poison→fence→sacrifice→remount sequence. The wedge oracle, caged.
Arrange later on souffle (Mac Studio M2, the Metal/heavy-work box).

**RODIN-IN-VIX (2026-07-05, the day's biggest design turn):** position
banked in docs/design/rodin-in-vix.md. Fact lifecycle → machine
(kernel as capability, semantic-cutoff comparator = projection-memo's
successor); kernel itself: CDCL mutation is LINEAR → functional-in-
place via reuse analysis (Perceus/Lean class, refcount==1 from the
store), solve interior is FLESH (JIT'd, untraced), perf bar reframed
(warm solves free ⇒ cold kernel may be 5-10x slower and still win).
AMOS STANDING DIRECTIVE: one failed spike ≠ verdict; push until
absolutely-no-way-in-hell; rodin-core is the ORACLE, never discarded.
