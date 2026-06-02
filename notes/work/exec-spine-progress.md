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

## What's DONE (25 commits this session, all green, see `git log`)

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
- **The eval↔remote join (real):** the join `remote.rs` flagged as missing ("eval's own
  `force` calling `force_remote` … comes with exec") is built. `Value::Tree(HandleId)` is a
  remote producing-tree handle (eval holds only the scheduler `HandleId`, never the vox
  client). `tree.file(@p)` in eval forces that path via `strand::force_remote` over vox —
  parking the strand until the producer yields the part (bytes → a `Str` for now, the lossy
  convention; an absent part → poison). `crates/vx-runtime/tests/remote_tree_via_eval.rs`:
  a `.vx` `tree.file(@out)` on a strand forces a remote tree over vox — already-produced,
  park-then-resume, and never-produced→poison, all through eval. Fixed a real bug on the way:
  **bare `@path` literals lowered to empty text** (`build_path_literal_expr` only read the
  interpolated / quoted-`@"…"` forms; the bare segmented form `@out`/`@Cargo.toml`/`@src/lib.rs`
  fell through to `vec![]`) — now reconstructed from the segment tokens. `Value::Tree` also
  got a `<tree>` display arm in vx-daemon's `test_runner::map_value`.

## Vox mechanics for the crossing (settled with Amos — do not relitigate)
- A vox service method **returns plain data and cannot return a channel** (returning channels
  used to work; it was confusing and got forbidden). Channels ride **in args only**. Arg
  directionality (0.8.2 `channel.rs`, verified by round-trip): `Tx<T>` ⇒ handler sends → caller;
  `Rx<T>` ⇒ handler receives ← caller. So to **push** a stream back, the method takes a `Tx<T>`.
- **Request/response demand-pull is NOT faked over a channel pair** — that's what **virtual
  connections** are for (Amos was explicit). The executor serves the `Producing` service on
  inbound virtual connections; the orchestrator does `observe_client.session.open::<ProducingClient>(
  ConnectionSettings { parity: Odd, max_concurrent_requests, initial_channel_credit })` to get
  a client, wraps it `RemoteHandle::new` → `register_handle` → `HandleId`, and `force_remote`
  pulls `force(PartKey)` over the vconn. **Verified end-to-end:** `tests/remote_tree_over_vconn.rs`
  (force a tree path over a Producing vconn, on a strand). The vconn opens fine over our
  `memory_link_pair` + `acceptor_on`/`initiator_on` + `Bare` establish.
- **Conduits are a different mechanism — not this.** **Do NOT read or compile against `~/vox`**
  — divergent, different codec; the contract is the published **vox 0.8.2** the workspace locks.
- Built toward it: `remote::producer() -> (ProducerService, Producer)` (shared state, no
  transport — the executor serves the service where it wants) and `RemoteHandle::new(ProducingClient)`
  (wrap a vconn-opened client); `channel()` now DRYs through both.

## The tree output crossing — DONE (end to end, over vox, on a strand)

`exec(cmd) where { observer: (run) => run.stream.out }` returns the run's **output tree**, and
forcing a file out of it pulls the bytes over vox. Test: `tests/exec_output_tree.rs`
(`out.file(@out.txt)` → `CONTENT`). The pieces:
- **Value model.** `run_value`'s `live_face` builds `out: Value::OutputTree` (executor-side output
  tree); `to_wire` marks it `WireValue::Tree` (a remote leaf, not inline); `from_wire` is fallible
  and a tree *nested* in a value still poisons (only the top-level case is crossed — see below).
- **Serving (executor).** `ObserveService` runs the command in a fresh scratch dir, captures every
  file it wrote into a `remote::producer()` (`capture_outputs`), and serves that on inbound
  `Producing` virtual connections (the `local_observer` acceptor reads the producer slot). *(Slice-1
  capture = all files under the scratch dir. The faithful set is the command's grammar-declared
  outputs (`Command.outputs`, dropped by `ExecCommand`) under the VFS sandbox — the deferred
  `run_exec` piece. Approved (A) by Amos as the interim.)*
- **Crossing (boss).** `dispatch_observe` now returns `strand::ObserveOutcome` (not the wire
  `Observed`): when the observer returns a top-level tree (`Observed::Value(WireValue::Tree)`),
  `drive_observe` opens a `Producing` vconn on the observe session and the boss `register_handle`s
  it → `ObserveOutcome::Tree(HandleId)`. eval (`eval_where`) turns that into `Value::Tree(HandleId)`;
  `force_remote` pulls its paths. Verified: `tests/remote_tree_over_vconn.rs` (vconn pull in
  isolation), `observe::tests::the_executor_serves_run_outputs_over_a_producing_vconn` (serving),
  `tests/exec_output_tree.rs` (the whole spine).
- *(Note: `WireValue` is recursive, so vox logs `vox_jit cycle guard hit for shape=WireValue` and
  falls back to the interpreted codec — benign, correctness intact.)*

## NEXT (pick up here)

The tree crossing is real for the common case. Remaining, roughly in order:
- **Nested + Exit-side trees.** Only a *top-level* returned tree crosses; a tree nested in a struct/
  tuple (`from_wire` on `WireValue::Tree`) still poisons. And `Exit.out` is deliberately unbuilt, so
  `(run) => run.ok` crosses as a plain struct — building `Exit.out` needs the general nested-handle
  walk (the boss resolving every `WireValue::Tree` in a result, not just the top one).
- **Faithful output content.** Replace the scratch-dir-walk with the grammar-declared outputs
  (`Command.outputs`) materialized under the VFS sandbox (the `run_exec` piece). `ExecCommand`
  currently drops `outputs`/env/mounts — they come back here.
- **Streams** (the *push* side, uses a `Tx`, not a vconn). `observe` grows a `Tx<…>` arg for a
  streaming observer result (`-> Stream[Artifact]`); the executor pushes items, the orchestrator's
  `Rx` feeds them in. `Value::Stream` + `PartKey::StreamItem(n)` + eval stream ops
  (`.lines()`/`.map()`/`.filter_map()`/`.collect()`). The channel round-trip is verified; cf.
  `zoo/rustc-hello.vx`.
- **The structured failure crossing.** `Observed::Poisoned(String)` still crosses a bare
  message; the failure model wants the domain error *value* (`ProcError`) + the
  `FailureContext` (span/demand-path) to cross, so a caught `run.ok?` → `Result[Exit,
  ProcError]` carries real data. Its own piece (cf. the catch-typing deferred item).
- **Executor *selection*.** `exec_client` is "the local executor" (one, registered by the
  test). §4 routing — choosing an executor from the command's ability provenance — is later.

## Deferred / stubbed (don't forget, not now)
- The eval *consumer* of a remote `Tree` is **done** (`Value::Tree` + `tree.file → force_remote`,
  see above); what remains is the executor *producing* one (the other half — see NEXT), the
  `Stream` handle + `StreamItem` forcing, and `run_value` building `run.ok.out`/`run.stream.*`
  as real handles (today `.stream` = empty struct, `Exit.out` unbuilt).
- `run.ok? → Result[Exit, ProcError]` *catch typing* (gate poisons with a ProcError value
  already; teaching the checker `.ok` is fallible-with-ProcError is its own piece).
- Real `Bytes` value (today `Exit.stdout` and a forced tree path render lossily as a `Str`).
- The data value-crossing (`WireValue`) is **done**; the *handle* crossing's producer half +
  the structured failure crossing remain (see NEXT). `ExecCommand` is program+args; the full
  `Command` value + `run_exec`-under-sandbox (mounts/outputs/the write-set `Tree`) land with
  input/output handling. The other `exec` `where`-kwargs (`env`/`mounts`/`writable`/
  `create_dirs`) and the signature's *default* observer are unread by `eval_where`.
- Daemon-host `Observe` (so colocated/separate is the daemon's call, not a test's
  `local_observer`).
- FSKit block-VFS follow-ups (#19), doctor ladder (#17), Vixen Management install (#18).

## Hard constraints (Amos)
- Never call a codec directly (no `facet_postcard::…` in our code; Facet derives are
  correct — that's the vox contract). Everything crosses a **vox service**.
- Never `block_on` an async RPC from eval — park via the strand boss.
- cargo-nextest; never suppress clippy/compiler warnings; commit-often, never revert.
- Mirror never allocates; ids stay the orchestrator's, verbatim.
