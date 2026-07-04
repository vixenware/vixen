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
one, across compactions. IMPLEMENTATION GOES TO GPT-5.5 HIGH ONLY
(provider "codex/gpt-5.5", thinkingOptionId "high", modeId "auto-review" —
NOT "auto": auto parks the agent on human approvals; auto-review routes
them to the auto-reviewer. Learned when the rebase agent sat 20 minutes on
a git-worktree fetch approval).
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

## Queued for delegation (write prompts as needed)


warm-oracle actor thread (daemon statefulness; the anti-Nix whitespace-edit
demo), executor-side step gating, top-down DAG view + lanes/rails view
(events carry `at` micros already), fetch primitive for real
(http+checksum+extract), JSON/TOML → vix values, seatbelt re-land (vixen
side), snark alphabet parametrization (PL core), DWARF suspend-point
annotations, den repo creation (NOT yet — wait for an installable
artifact).
