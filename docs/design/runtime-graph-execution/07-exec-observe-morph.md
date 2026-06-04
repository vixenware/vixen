# Runtime graph execution - exec/observe production morph

This document is self-contained. It records the plan for morphing the current
runtime-local scratch observe path into the real production exec path, without
creating another fallback that future agents can keep green instead of correct.

## Complete task context

Vixen execution is graph-driven. The runtime driver reaches a boundary node and
reports that demand has reached a boundary. Boundary materialization may demand
only the request-shaping graph payload authorized by the driver token. It must not
walk source expressions and it must not run processes inside the graph evaluator.

`exec` is a boundary, not an intrinsic function. A demanded exec boundary is
placed by the daemon/orchestrator onto an executor daemon. The executor owns
sandbox policy, CAS-VFS mounts, process execution, read/write witnesses, output
flushing, and construction of the `Run` value. The observer closure is evaluated
on the executor using the same graph evaluator semantics, then only the observer
result or store-backed handles cross back.

The current code still has a cheating path:

- `vx-runtime::graph_exec` materializes a graph exec boundary and dispatches a
  minimal `ObserveRequest { program, args, observer }`.
- `vx-runtime::observe` defines that minimal protocol and currently still carries
  the shared wire value/result types.
- The former runtime-local `ObserveService` was renamed
  `ScratchObserveService`, but it still spawns in a scratch directory. That name
  change is not the architecture. It is only a visible wound.

The objective here is to morph that path into the production shape. Deleting the
scratch path without a replacement is not durable: a compacted agent will see red
tests and rebuild the cheat. The durable move is to make the production path the
path tests use, then make the cheat impossible to reintroduce quietly.

## Ownership boundaries

- `vx-runtime-graph`, `vx-runtime-core`, `vx-runtime-driver`: source-blind graph
  execution only.
- `vx-runtime`: source-facing front-end/lowering plus shared evaluator library
  used by both orchestrator and executor. It may evaluate an observer bundle
  against a supplied `Run` value. It must not spawn commands, create scratch
  exec directories, own sandbox policy, own CAS-VFS mounts, or flush outputs.
- `vx-services`: stable Vox protocol surface for daemon/runner/store/vfsd
  boundaries. Exec/observe request and result types should move here or into a
  narrower protocol crate once the shape stabilizes.
- `vx-runner`: executor daemon implementation. It owns mount-run-flush, sandbox
  launch, output tree serving, and executor-side observer evaluation.
- daemon/orchestrator layer: placement, runner RPC, retries, cache/index lookup,
  and cache-record state.
- `vx-store`: immutable CAS chunks/nodes. It is not the input-key index.

## Morph phases

### Phase M1 - Split observer evaluation from process execution

Create an explicit runtime library function that evaluates an observer over an
already-completed process result plus an output-tree handle producer. It should
accept only data supplied by an executor:

```text
CompletedRun {
  exit_code,
  stdout,
  stderr,
  output_tree_handle
}

evaluate_observer(completed_run, observer_bundle) -> Observed
```

Allowed in `vx-runtime`:

- install an observer bundle.
- bind a `Run` value.
- graph-evaluate the observer body.
- convert the observer result to the wire value.

Forbidden in `vx-runtime`:

- `tokio::process::Command`
- `std::process::Command`
- scratch exec directories
- output directory walking
- CAS-VFS mount or flush
- runner placement or retry logic

This phase may keep the scratch implementation only as a caller of the new helper
while the runner-backed service lands. It is not a completion point.

Current checkpoint:

- `vx-runtime::observe::CompletedRun` represents executor-supplied process facts.
- `vx-runtime::observe::evaluate_observer` evaluates an observer bundle over that
  completed run and returns `Observed`.
- This helper does not yet carry an executor output-tree identity. A returned
  `WireValue::Tree` is still only a marker until the service serving that tree is
  connected by M2/M3.
- `ScratchObserveService` still spawns in a scratch directory and captures
  outputs, but it now calls the helper instead of owning observer evaluation.
- This narrows the violation; it does not make scratch execution acceptable.

### Phase M2 - Add runner-backed Observe service

Implement the `Observe` service on the executor side, hosted by `vx-runner`, not
by `vx-runtime`.

The runner-backed observe implementation must:

1. Convert the demanded observe payload into the runner's local `ExecRequest`.
2. Run `run_exec(vfsd, request)` so the process executes through CAS-VFS and
   sandbox launch.
3. Expose returned output trees through a `Producing` virtual connection. The
   first cut may materialize the flushed CAS tree into the current producer
   service; later cuts should serve paths directly from CAS/read-set state.
4. Call the runtime observer-evaluation helper with the completed process data.
5. Return `Observed::Value` or `Observed::Poisoned` over Vox.

The runner may depend on `vx-runtime` because the executor links the shared
evaluator. `vx-runtime` must not depend on `vx-runner`.

Current checkpoint:

- `vx-runner` depends on `vx-runtime`, not the other way around.
- `RunnerService` implements the runtime `Observe` service and `RunnerAcceptor`
  routes `"Observe"` to that implementation.
- `vx-daemon` advertises and routes `"Observe"` to the hosted `RunnerService`
  when a runner is configured, so daemon-hosted runners expose the same
  runner-backed observe endpoint.
- The runner-backed observe implementation calls `run_exec` with the current
  minimal payload, converts the process result to `CompletedRun`, and calls
  `evaluate_observer`.
- This path currently sends no mounts, env, or output roots because the
  runtime-local observe payload cannot represent them yet.
- If an observer result is `WireValue::Tree`, the runner-backed path poisons
  explicitly because `Producing` output serving from flushed CAS trees is not
  wired yet. It does not fake a tree handle.
- Runtime exec tests still use `local_observer()` and therefore still exercise
  the quarantined scratch service. That is the next thing to cut.

### Phase M3 - Move tests to the runner-backed path

Runtime exec/observe tests that currently call `local_observer()` must use a
runner-backed local observe client. The important tests are:

- `observe_on_strands`
- `exec_on_strands`
- `exec_output_tree`
- `hello_world`
- any `.vx` exec boundary test that registers an observe client

If a test cannot be moved in one slice, it must be marked as an honest missing
production feature by failing through an explicit unsupported boundary, not by
continuing to use the scratch process runner.

### Phase M4 - Remove scratch process execution from runtime production code

Delete `ScratchObserveService`, `scratch_dir`, and `capture_outputs` from
`vx-runtime/src`. The `source_blind_guardrails` binary must scan
`vx-runtime/src` for:

- `tokio::process::Command`
- `std::process::Command`
- `ScratchObserveService`
- `scratch_dir`
- `capture_outputs`
- production-sounding `ObserveService`

Tests may have tightly-named fixtures only if they live under `tests/` and cannot
be imported by production runtime code. Prefer not to keep such fixtures at all
once the runner-backed client exists.

### Phase M5 - Move protocol shape out of `vx-runtime`

The minimal runtime-local protocol:

```text
ExecCommand { program, args }
ObserveRequest { observer, command }
Observed
WireValue
```

is still too close to the old cheat. Replace it with a protocol type that can
carry:

- projected `Command`
- resolved observer bundle and captures
- env
- CAS tree mounts
- writable output roots
- exemptions and host verification obligations
- cache identity material that is knowable before dispatch

That protocol belongs in `vx-services` or a narrower boundary crate. The graph
evaluator should consume only the result of a boundary materialization, not own
the service contract.

### Phase M6 - Cache/index and read/write witnesses

Once exec runs through the runner-backed path:

- input-key lookup belongs to the daemon/orchestrator cache/index layer.
- cache records and output trees live in `vx-store`.
- read/write witnesses are recorded by the executor/VFS layer.
- `BoundaryL2Cache` in `vx-runtime` must be removed or reduced to a test-only
  compatibility shim while the daemon cache/index is introduced.

## Resilience rules

- Do not reintroduce a process-spawning service in `vx-runtime` to make tests
  green.
- Red tests are acceptable when the failure is the lack of a runner-backed
  service or unsupported protocol shape. Red tests are not acceptable when the
  fix is to restore scratch spawning.
- Every phase must update this document and
  `docs/design/runtime-graph-execution/06-verification-and-resume.md` with the
  current honest state.
- Guardrails are part of the implementation, not optional follow-up cleanup.

## Latest verification for this checkpoint

- `cargo check -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner`
  passed.
- `cargo nextest list -p vx-runtime -E 'test(observer_evaluation_accepts) | test(observe_runs_a_real_command) | test(observe_crosses_the_output_tree) | test(the_executor_serves_run_outputs)'`
  selected the four observer helper/scratch tests.
- `cargo nextest run -p vx-runtime -E 'test(observer_evaluation_accepts) | test(observe_runs_a_real_command) | test(observe_crosses_the_output_tree) | test(the_executor_serves_run_outputs)' --no-fail-fast`:
  4/4 passed.
- `cargo nextest list -p vx-runtime -E 'binary(source_blind_guardrails) | binary(exec_on_strands) | binary(exec_output_tree)'`
  selected 14 tests.
- `cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails) | binary(exec_on_strands) | binary(exec_output_tree)' --no-fail-fast`:
  14/14 passed.
- `cargo nextest list -p vx-runner` selected
  `sandbox::tests::seatbelt_confines_writes_to_the_output_root`.
- `cargo nextest run -p vx-runner --no-fail-fast`: 1/1 passed.
- `cargo nextest run -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner --no-fail-fast`:
  173/173 passed.
- `cargo check -p vx-daemon -p vx-runner -p vx-runtime` passed after adding the
  daemon `"Observe"` route and removing a stale `Value::Closure` display arm.
- `cargo nextest list -p vx-daemon` selected two service tests.
- `cargo nextest run -p vx-daemon --no-fail-fast`: 2/2 passed.
- The forbidden-symbol scans over `vx-runtime-core`, `vx-runtime-driver`, and the
  deleted tree-walker affordance scan over `vx-runtime/src` returned no matches.

Important caveat: the green `exec_on_strands`, `exec_output_tree`, and
`observe_on_strands` tests still use `local_observer()`. They prove the M1 helper
did not break the existing compatibility path; they do not prove that runtime exec
has moved to the runner-backed service.

## Current next step

Finish M2/M3:

1. Teach the runtime/orchestrator test harness to obtain an observe client from a
   runner-backed service instead of `local_observer()`.
2. Repoint at least one observe-over-strands path to the runner-backed service.
3. Wire or explicitly quarantine `Producing` output serving for the runner-backed
   path.
4. If output-tree serving from CAS is not complete in the slice, document the
   exact unsupported result and leave tests red rather than rebuilding scratch
   output walking in `vx-runtime`.
