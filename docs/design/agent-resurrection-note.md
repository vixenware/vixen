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
one, across compactions. Amos and I hold the design line here. When work
turns mechanical I write a detailed, SELF-CONTAINED agent prompt (context,
files, acceptance criteria, the oracle it must keep green); Amos dispatches
and supervises those agents separately; results feed back here and I rule
on integration. Spec and implement interleaved. Commit continuously so the
thread can end mid-stride with nothing lost.

**Priorities (agreed):**
1. **THE KEYSTONE — graph engine == oracle.** Lower real vix evaluation
   (AST+env, real Values) onto the demand graph; batching at memo
   boundaries; dynamic demand (Cond/glob/map-collect); differential harness
   asserting graph == oracle on lua.vix/eval.vix/types.vix INCLUDING the
   event stream. This is what killed attempts 1–4; once it holds, every
   future optimization has a machine-checkable definition of "didn't break
   vix". Do the semantic core myself; delegate mechanical extension.
2. **cargo.vix ⊗ version resolution — ONE design cycle** (Amos: "I don't
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

## Queued for delegation (write prompts as needed)

warm-oracle actor thread (daemon statefulness; the anti-Nix whitespace-edit
demo), executor-side step gating, top-down DAG view + lanes/rails view
(events carry `at` micros already), fetch primitive for real
(http+checksum+extract), JSON/TOML → vix values, seatbelt re-land (vixen
side), snark alphabet parametrization (PL core), DWARF suspend-point
annotations, den repo creation (NOT yet — wait for an installable
artifact).
