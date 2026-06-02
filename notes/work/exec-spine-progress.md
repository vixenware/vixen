# exec spine — progress + handoff (continue here)

**Mission:** build `exec` on the demand-driven runtime (`vx-runtime`), shipping the
observer to an executor **over vox** (never a hand-rolled codec, never assume local).
`exec(command) where { observer: (run) => T } -> T`. The orchestrator ships the
observer's *code* to a fresh executor; the executor runs the command, builds a `Run`,
evaluates the observer there, returns `T`. Outputs (`run.stream.out`, the producing
tree) cross back as remote handles.

## The converged architecture (settled with Amos — do not relitigate)

- **Mirror never allocates.** The orchestrator's front-end is the *sole* id allocator
  (parse→resolve→typecheck→intern mints every `ExprId`/`TypeId`/…). The executor is a
  **mirror**: evaluator-only, it *installs* a bundle in the orchestrator's id-space
  **verbatim** and never re-derives/re-allocates. This kills both the id-clash and the
  "two inferences must agree" divergence. Eval **cannot** allocate ids — it only borrows
  `&VirProgram`/`&TypeEnv` (verified).
- **Static bundle.** What ships = the observer body's *static reachable closure* (nodes
  + types), computed by `vx_ir::reachable()`, following **resolution edges** (NameRef→Item,
  calls, struct/variant defs, captured bindings' bodies) + the transitive type closure —
  not just the syntax tree. Completeness is the whole job.
- **Captures are nodes, not forced values.** An in-program capture rides as its
  *defining nodes* (already in the closure), forced lazily on the mirror — so a bomb in
  an untouched field stays cold. Only genuinely-external leaves (a sibling exec's `Tree`)
  resolve to identity. (Slice-1 observers have no captures yet.)
- **Everything crosses a vox service.** In-process = `transport-local` memory link;
  remote = different transport, **same service**. **Vox owns serialization** — never name
  the codec (postcard is a vox-internal detail under active replacement). The daemon
  decides colocated-vs-separate placement; we never special-case transport. *(I screwed
  this up once mid-session — called `facet_postcard` directly — and ripped it out. Don't.)*
- **`Run` is a struct, not a `Value` variant.** D1=B: the general mechanism is a
  **native (intrinsic) thunk** — a struct field whose value is Rust, lazily/memoised.
  `Run = Value::Struct([("ok", Native(gate)), ("stream", Native(live))])`. `run.ok` is
  *ordinary field access*; forcing it runs the gate → an `Exit` struct, or poisons a
  `ProcError`. Only opaque handle primitives (`Stream`/`Tree`) stay as `Value` variants
  (not built yet). The `.ok`/`Run` are **local on the executor** (observer runs where the
  Run is built) — not remote; `force_remote` belongs on the **outputs**.
- **eval ↔ async-observe via the strand boss.** eval is sync; observe is an async vox
  RPC. The honest bridge = `force_remote`-class: `dispatch_observe` registers, **parks**
  the strand (`AwaitingObserve`), the **boss** drives the vox `observe()` on its runtime,
  resolves, wakes the strand. No `block_on` cheat. **Built + tested.**

## What's DONE (18 commits this session, all green, see `git log`)

Foundation (shippable typed IR):
- `c02dcb9c`..`8e8c9a1b` — `vx-span` (rowan-free `SourceId`/`SourceSpan`), re-point all
  consumers, Facet-derive the **VIR universe** (`vx-ir`) + the **type universe**
  (`vx-types`: `Ty`/`TypeEnv`, `repr(u8)` enums). Facet is the contract with vox.
- `253d97f6` — `vx_ir::reachable(arena, env, entry) -> Bundle`: the static closure walk
  (`crates/vx-ir/src/bundle.rs`). Completeness test in `crates/vx-ir/tests/bundle.rs`.
- `855b415f` — `Bundle::install() -> (VirProgram, TypeEnv)` (verbatim ids, holes get
  placeholders), `TypeEnv::from_sparse`. Fidelity-tested.

Eval / mirror:
- `682fed45`,`bbe2e67f` — `vx_runtime::eval::eval_body(program, env, body, args, mode)`:
  the mirror entry (no front-end). `crates/vx-runtime/tests/mirror.rs`.
- value model: `2715cb43` native intrinsic thunks (`ThunkState::Native(Intrinsic)`,
  `Thunk::native`/`Thunk::done`; `crates/vx-runtime/tests/intrinsic.rs`);
  `bb1976d8` `Run` as a struct + `.ok` gate (`crates/vx-runtime/src/run_value.rs`:
  `ProcOutcome`, `build`; `crates/vx-runtime/tests/run_value.rs`).

Over vox:
- `af92c42c` — `remote.rs` producing handle serves **stream chunks** (`PartKey::StreamItem`).
- `b7118ab8`,`365d6cfb` — `crates/vx-runtime/src/observe.rs`: the `Observe` vox service
  (`ObserveRequest{bundle, command:ExecCommand}` → `Observed{U64|Clean|Poisoned}`).
  `ObserveService` runs the real command (`tokio::process`), builds a `Run`, installs +
  `eval_body`s the observer. `local_observer()` stands it up over `transport-local`.
  Tested: ship observer + `true`/`false` → `run.ok` gates on the real exit, over vox.
- `5b364721` — **#3a, the hard part:** `vx_runtime::strand::dispatch_observe(client_id,
  request)` + `Scheduler::register_observe_client`. Mirrors the remote-part machinery
  (`ObserveSlot`/`observe_clients`/`AwaitingObserve` ≈ `PartSlot`/`handles`/
  `AwaitingRemotePart`; `drive_observe` ≈ `force_part`; boss select-branch added).
  `crates/vx-runtime/tests/observe_on_strands.rs`: a strand dispatches observe, parks,
  the boss drives the vox call, resumes — clean on exit 0, failure crossing back on exit 1.

## NEXT: #3b — the eval `where`-hook (make `exec(cmd) where {observer}` real)

The strand→observe bridge is built + tested underneath; what's missing is the **language
surface**. Grounding found (eval.rs has NO `ExprKind::Where` handling; `exec` is NOT a
stdlib fn):

1. **Declare `exec` in `crates/vx-stdlib/std/core.vx`** — the §5 signature
   `fn exec[T](command: Command) with { observer: Fn(Run) -> T } -> T` (or a stripped
   start: just `command` + `observer`, no `env`/`mounts`/…), so the program type-checks.
2. **OPEN QUESTION (decides the size):** does the type checker already handle `where`-kwargs
   (the §5 "surviving kwargs" — binding a `where { observer: … }` record to a function's
   `with`-params + the `with`-clause signature + `Fn(Run)->T`)? **Verify first.** If yes,
   #3b is mostly the eval hook. If no, #3b also includes that type-checking work
   (vx-ir/src/type_check) — a separate chunk.
3. **eval `ExprKind::Where { callee, keywords }` handling** (`crates/vx-runtime/src/eval.rs`,
   add an arm; today Where is unhandled). Recognize the exec case: `callee` is a `Call`
   whose callee is `NameRef "exec"` — mirror the `if n.name == "discover"` pattern at
   eval.rs:~850. Extract: the command (eval the call's arg → a `Command`/`CommandValue` →
   `ExecCommand{program=argv[0], args=argv[1..]}` via `command.rs::CommandValue.projected
   .execution_argv`); the observer (the `keywords` record's `observer` field expr → a
   `ClosureExpr` → its `BodyId`).
4. **Dispatch:** `reachable(program.arena, env, observer_body) -> Bundle`, then
   `strand::dispatch_observe(client_id, ObserveRequest{bundle, command})` → `Observed` →
   convert to a `Value`. eval **must run on a strand** (so `dispatch_observe` can park) —
   run the test program via the `Scheduler`, with the local `Observe` client registered
   (`register_observe_client`). client_id is trivially "the local executor" for now;
   executor *selection* (multiple) is §4 routing, later.
5. Test: a `.vx` program doing `exec(@[ ... ]) where { observer: (run) => run.ok }` runs
   end-to-end over vox on a strand and returns the result.

## Deferred / stubbed (don't forget, not now)
- `.stream` face = empty struct; `Stream`/`Tree` handle `Value` variants + the
  `eval.force → force_remote` join for **outputs** (③, task #5) — the genuinely-remote part.
- `run.ok? → Result[Exit, ProcError]` *catch typing* (gate poisons with a ProcError value
  already; teaching the checker `.ok` is fallible-with-ProcError is its own piece).
- Real `Bytes` value (today `Exit.stdout` renders lossily as a `Str`).
- `Observed` is a placeholder result enum; the general value-crossing (`WireValue`/`T`)
  is later. `ExecCommand` is program+args; the full `Command` value + `run_exec`-under-
  sandbox (mounts/outputs/the write-set `Tree`) land with input/output handling.
- Daemon-host `Observe` (so colocated/separate is the daemon's call, not a test's
  `local_observer`).
- FSKit block-VFS follow-ups (#19), doctor ladder (#17), Vixen Management install (#18).

## Hard constraints (Amos)
- Never call a codec directly (no `facet_postcard::…` in our code; Facet derives are
  correct — that's the vox contract). Everything crosses a **vox service**.
- Never `block_on` an async RPC from eval — park via the strand boss.
- cargo-nextest; never suppress clippy/compiler warnings; commit-often, never revert.
- Mirror never allocates; ids stay the orchestrator's, verbatim.
