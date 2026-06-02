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

## What's DONE (20 commits this session, all green, see `git log`)

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
- **#3b, the eval `where`-hook (DONE):** `exec(cmd) where { observer }` is a real language
  feature now, and `exec(…)` **returns the observer's real value** (no placeholder).
  `ExprKind::Where` arm in `eval.rs` (`eval_where`): recognise exec (callee is a `Call` of
  `NameRef "exec"`, mirroring `discover`), project the command to its argv (`exec_command`),
  pull the observer closure body out of the `where` record (`observer_body`),
  `vx_ir::reachable(&arena, env, observer_body)` → `Bundle`, then `dispatch_observe(client,
  ObserveRequest{bundle, command})`; reconstruct the result via `from_wire` (poison
  detonates). New orchestrator entry `eval::eval_body_with_exec(…, exec_client)` threads the
  client onto a new `Evaluator.exec_client` field; off-strand / no-client `exec` poisons
  honestly. `strand::ObserveClientId` is now `pub`; `ObserveSlot`'s request boxed + a
  collapsible-if collapsed (clippy clean).
- **The value crossing (real, not stubbed):** the `Observed{U64|Clean|Poisoned}` placeholder
  is gone. `observe` now returns `Observed{Value(WireValue)|Poisoned(String)}`. `WireValue`
  (`observe.rs`, Facet-derived — the vox contract) is the data subset of `eval::Value`
  (scalars + tuple/array/struct/variant, every field forced). The executor **deep-forces**
  the observer's whole result into a `WireValue` (`eval::eval_body_wire` → `Evaluator::to_wire`,
  forcing every nested thunk) and ships it; the orchestrator rebuilds a `Value` (`from_wire`,
  all-`done` thunks). So `exec(cmd) where { observer: (run) => run.ok }` evaluates to the
  actual `Exit` struct. `observe`'s own test asserts the crossed `WireValue::Struct([stdout,
  stderr])`; `crates/vx-runtime/tests/exec_on_strands.rs` asserts the strand's `exec(@[ ${cmd}
  ]) where { observer: (run) => run.ok }` evaluates to that `Exit` struct on `true` (exit 0),
  and the failure crosses back as a poison on `false` (exit 1).

  **What the verify-first found (the §5/#3b grounding was partly stale):**
  - `exec` is **already declared** — in `crates/vx-stdlib/std/build.vx` (not core.vx), with
    the *full* §5 sig incl. defaults + `#sandbox`: `fn exec[T](command: Command) where { env,
    mounts, writable, create_dirs, observer: Fn(Run) #sandbox -> T = (run) => run.ok.out } ->
    T`. No new declaration needed.
  - The type checker **already fully handles `where`-kwargs** — `calls::infer_where` binds
    the `where { observer: … }` record to the sig's `keyword_params`, binds the closure's
    `run: Run`, and substitutes `T` from `run.ok : Exit`. `keyword_params` come from a fn
    decl's `where { … }` clause (`type_collect::collect_fn_sig_from_body`, `param.is_keyword`).
  - **Surface syntax is `where { }`, not `with { }`** (the notes' §5 "`with`" was conceptual):
    `parser::fn_decl_contents` parses fn-decl keyword params with `WHERE_KW`, and a call-site
    `f(a) where { k: v }` is `ExprKind::Where` (a `Record` keywords expr). `ExprKind::With` is
    the unrelated functional-update `value with { field: new }`. The `zoo/*.vx` files using
    `exec(...) with { observer }` are stale and don't parse on today's grammar.
  - `@[ … ]` CLI literals type as `Poison` (`infer_cli_literal` — "Command not modelled in
    the type system yet"), which is compatible with `command: Command` (poison branch), so
    `exec(@[ … ])` type-checks fine.

## NEXT (pick up here)

The data value crossing is real; the remaining pieces are the *handle* crossing and the
genuinely-remote outputs. Assemble them one by one, then the next real thing:
- **Remote handles for the genuinely-remote leaves (task #5, ③).** `WireValue` carries only
  data. `run.ok.out` (the producing write-set `Tree`) and `run.stream.*` (the live
  `Stream`s) are *handles*, not inline data — they must cross as `Stream`/`Tree` handle
  variants (the `remote.rs` machinery: `RemoteHandle`/`force_remote`/parts is already built)
  and join via `eval.force → force_remote`. This is the piece behind streaming observers
  (`-> Stream[Artifact]`) and `run_value`'s currently-stubbed `.stream` face (empty struct)
  + `Exit.out` (not built). `to_wire` poisons on these today (honestly), which is the seam.
- **The structured failure crossing.** `Observed::Poisoned(String)` still crosses a bare
  message; the failure model wants the domain error *value* (`ProcError`) + the
  `FailureContext` (span/demand-path) to cross, so a caught `run.ok?` → `Result[Exit,
  ProcError]` carries real data. Its own piece (cf. the catch-typing deferred item).
- **Executor *selection*.** `exec_client` is "the local executor" (one, registered by the
  test). §4 routing — choosing an executor from the command's ability provenance — is later.

## Deferred / stubbed (don't forget, not now)
- `.stream` face = empty struct; `Stream`/`Tree` handle `Value` variants + the
  `eval.force → force_remote` join for **outputs** (③, task #5) — the genuinely-remote part.
- `run.ok? → Result[Exit, ProcError]` *catch typing* (gate poisons with a ProcError value
  already; teaching the checker `.ok` is fallible-with-ProcError is its own piece).
- Real `Bytes` value (today `Exit.stdout` renders lossily as a `Str`).
- The data value-crossing (`WireValue`) is **done** (see above); the *handle* crossing
  (`Stream`/`Tree`) and structured failure crossing remain. `ExecCommand` is program+args;
  the full `Command` value + `run_exec`-under-sandbox (mounts/outputs/the write-set `Tree`)
  land with input/output handling. The other `exec` `where`-kwargs (`env`/`mounts`/
  `writable`/`create_dirs`) and the signature's *default* observer are unread by `eval_where`.
- Daemon-host `Observe` (so colocated/separate is the daemon's call, not a test's
  `local_observer`).
- FSKit block-VFS follow-ups (#19), doctor ladder (#17), Vixen Management install (#18).

## Hard constraints (Amos)
- Never call a codec directly (no `facet_postcard::…` in our code; Facet derives are
  correct — that's the vox contract). Everything crosses a **vox service**.
- Never `block_on` an async RPC from eval — park via the strand boss.
- cargo-nextest; never suppress clippy/compiler warnings; commit-often, never revert.
- Mirror never allocates; ids stay the orchestrator's, verbatim.
