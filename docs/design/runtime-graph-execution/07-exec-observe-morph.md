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

The former runtime-local process runner has now been removed. The remaining
exec/observe drift is the protocol/control-plane shape, not a process-spawning
service in `vx-runtime`:

- `vx-runtime::graph_exec` materializes a graph exec boundary and dispatches a
  minimal `ObserveRequest { program, args, observer }`.
- `vx-exec-protocol` owns that minimal protocol and the generated Vox
  Observe client/dispatcher. `vx-runtime::observe` now keeps only the
  executor-side observer-evaluation helper.
- `vx-runner` implements the runtime `Observe` service and owns process
  execution through `run_exec`; live CAS-VFS proofs are ignored by default
  because this host lacks the Vixen Management mount trigger.

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
- `vx-exec-protocol`: Vox protocol surface for exec/observe request and result
  payloads. It is intentionally narrower than `vx-services` so `vx-runtime` can
  construct demanded boundary requests without depending on all service
  contracts.
- `vx-services`: stable Vox protocol surface for daemon/runner/store/vfsd
  boundaries.
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
- `ScratchObserveService`, `scratch_dir`, `capture_outputs`, and
  `local_observer()` have been deleted from `vx-runtime`.

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
- This path currently sends no mounts or env because the runtime-local observe
  payload cannot represent them yet.
- `writable: [...]` roots now cross as `ObserveRequest.outputs`. They are
  extracted as graph node refs, demanded only when the exec boundary materializes,
  included in the exec cache key, and forwarded by `vx-runner` as
  `ExecRequest.outputs`.
- If an observer result is `WireValue::Tree`, the runner-backed path installs a
  CAS-backed `Producing` service over the flushed `ExecResult.outputs`; the
  orchestrator opens the `"Producing"` virtual connection on the same Vox session
  and demands individual paths from the returned write-set tree.
- The first CAS-backed producer resolves paths from the declared output-root
  namespace (`out/foo.o` for `writable: ["out"]`) by walking the flushed
  `DirectoryNode` tree and fetching small/large file chunks from `vx-store`.
- `RunnerAcceptor` and daemon-hosted runner sessions both route `"Producing"`.
- The old scratch-backed `crates/vx-runtime/tests/exec_output_tree.rs`
  integration file has been deleted. `exec_on_strands` no longer runs demanded
  exec/cache paths through `local_observer()`; its remaining test is the cold
  non-demand proof. Direct observe-on-strands, output-tree, and exec cache
  coverage has moved to runner-owned tests, with live-VFS variants ignored by
  default.

### Phase M3 - Move tests to the runner-backed path

Runtime exec/output tests that used to call `local_observer()` now use
runner-backed proof targets or have been deleted. Remaining important coverage is:

- any new `.vx` exec boundary test that registers an observe client must use the
  runner-backed path.
- ignored live-VFS proofs must stay visible and must fail at VFS boot when the
  host cannot provide the mount.

If a test cannot be moved in one slice, it must be marked as an honest missing
production feature by failing through an explicit unsupported boundary, not by
continuing to use the scratch process runner.

Current checkpoint:

- `vx-runner::tests::strand_observe_reaches_runner_backed_service` creates a
  runner-backed `ObserveClient`, registers it with `vx-runtime`'s strand
  scheduler, dispatches observe from a strand, and reaches the explicit
  no-CAS-VFS poison from `RunnerService`.
- This proves the strand/Observe seam can target runner code without adding a
  `vx-runtime -> vx-runner` dependency.
- `vx-runner::tests::strand_observe_uses_runner_backed_live_vfs_for_success_and_failure`
  is an ignored live-VFS proof target for the direct Observe bridge. It boots
  `vx_vfsd::boot_with_store`, runs `/bin/true` and `/bin/false` through
  `RunnerService::observe`, evaluates `(run) => run.ok`, and proves both the
  clean value and the gate poison cross back through the strand boss. The old
  `crates/vx-runtime/tests/observe_on_strands.rs` scratch-backed integration
  file has been deleted.
- `vx-runner::output_tree::tests::reads_declared_output_root_from_cas` stores a
  real Merkle output tree in `vx-store`, then serves `out/y.txt` through the
  runner CAS-backed `Producing` implementation.
- `vx-runner::tests::producing_vconn_serves_runner_output_tree_from_cas` proves
  `RunnerAcceptor` routes a `"Producing"` connection to that CAS-backed service.
- `vx-runner::tests::demanded_vixen_exec_reaches_runner_backed_observe` compiles
  a real `.vx` backtick invocation and proves a demanded exec reaches
  `RunnerService::observe` by receiving the runner-backed no-CAS-VFS poison.
- `vx-runner::tests::undemanded_vixen_exec_with_runner_backed_client_stays_cold`
  compiles the same language shape in an undemanded tuple slot and proves graph
  demand leaves it cold even though a runner-backed observe client is registered.
- These tests prove the runner-backed Observe seam and output-tree serving
  substrate without a live mounted VFS. They do not prove a demanded `.vx`
  backtick invocation running through live CAS-VFS yet.
- `vx-runner::tests::demanded_vixen_exec_runs_through_runner_backed_live_vfs_and_serves_output_tree`
  is an ignored live-VFS proof target. It compiles a real `.vx` backtick
  invocation, boots `vx_vfsd::boot_with_store`, builds a `RunnerService` with
  `with_vfsd`, runs `/bin/sh -c ...` with `writable: ["out"]`, uses the default
  `run.ok.out` observer, and demands `out.file("out/y.txt")` through the
  runner-backed `Producing` virtual connection. It is not run by default because
  it requires the host CAS-VFS mount; the default suite should show it as
  skipped, not silently replace it with scratch execution.
- `vx-runner::tests::explicit_stream_observer_returns_runner_backed_live_output_tree`
  is the ignored live-VFS proof for the explicit `(run) => run.stream.out`
  observer shape formerly covered by the scratch-backed `exec_output_tree`
  runtime test. It runs a real `.vx` backtick through `RunnerService::with_vfsd`
  and demands `out.file("out/y.txt")` through the returned runner-backed tree.
- `vx-runner::tests::bare_invocation_failure_poisons_through_runner_backed_live_vfs`
  is the ignored live-VFS proof for the default `run.ok.out` failure gate
  formerly covered by `exec_output_tree`; `/bin/sh -c "exit 7"` must poison
  through the runner-backed path.
- `vx-runner::tests::run_ok_observer_crosses_runner_backed_live_vfs_for_success_and_failure`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `exec_on_strands` `run.ok` success/failure tests. It runs real `.vx` backticks
  for `/bin/true` and `/bin/false` through runner-backed CAS-VFS and proves both
  the crossed `Exit` value and the gate poison.
- `vx-runner::tests::identical_backtick_exec_boundaries_share_runner_backed_l1_entry`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `exec_on_strands` identical-backtick L1 test. It counts actual `Observe` RPCs
  at the Vox service boundary while delegating to `RunnerService::with_vfsd`; two
  identical demanded invocations must dispatch observe once.
- `vx-runner::tests::backtick_exec_l2_cache_survives_fresh_runner_backed_graph_eval`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `exec_on_strands` disk-L2 test. It counts actual `Observe` RPCs at the Vox
  service boundary while sharing an explicit runtime L2 cache directory; the
  second fresh graph eval must reuse the persisted observer value and avoid a
  second dispatch.
- `vx-runner::tests::writable_roots_disambiguate_runner_backed_l1_entry`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `exec_on_strands` writable-root L1 test. It counts actual `Observe` RPCs at
  the Vox service boundary while delegating to `RunnerService::with_vfsd`; two
  otherwise-identical demanded invocations with different declared writable
  roots must dispatch observe twice.
- `vx-runner::tests::observer_identity_disambiguates_runner_backed_l1_entry`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `exec_on_strands` observer-bundle L1 test. It counts actual `Observe` RPCs at
  the Vox service boundary while delegating to `RunnerService::with_vfsd`; two
  otherwise-identical demanded invocations with different observer bundles must
  dispatch observe twice.
- `vx-runner::tests::declared_tool_fingerprint_disambiguates_runner_backed_l1_entry`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `exec_on_strands` declared-tool-fingerprint L1 test. It counts actual
  `Observe` RPCs at the Vox service boundary while delegating to
  `RunnerService::with_vfsd`; two otherwise-identical demanded invocations with
  different declared tool fingerprints must dispatch observe twice.
- `vx-runner::tests::executable_fingerprint_disambiguates_runner_backed_l1_entry`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `exec_on_strands` executable-fingerprint L1 test. It changes the host
  executable symlink between two demanded boundaries at the same path while
  counting actual `Observe` RPCs at the Vox service boundary; the second boundary
  must dispatch instead of reusing a path-keyed L1 entry.
- `vx-runner::tests::hello_world_uses_runner_backed_live_vfs_and_where_injection`
  is the ignored live-VFS proof formerly covered by the scratch-backed
  `hello_world` runtime integration. It exercises `where` provider injection, a
  demanded backtick using the injected C compiler, declared `writable: ["out"]`,
  and `out.file("out/hello.o")` through the runner-backed CAS-VFS path.
- The old `crates/vx-runtime/tests/exec_output_tree.rs` file has been deleted.
  The old `crates/vx-runtime/tests/hello_world.rs` file has also been deleted.
  The old runtime-local scratch observe helper has been deleted as well; its
  process-running tests live as runner-owned proofs.

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

Current checkpoint:

- `ScratchObserveService`, `scratch_dir`, `capture_outputs`, and
  `local_observer()` are absent from `vx-runtime`.
- The focused observer selector now has only
  `observer_evaluation_accepts_a_completed_process_without_spawning`, proving
  the runtime-owned helper still evaluates observer bundles over executor-supplied
  run facts without spawning.

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

Current checkpoint:

- `crates/vx-exec-protocol` exists as the narrower boundary crate for the
  exec/observe wire surface.
- `ExecCommand`, `ObserveRequest`, `WireValue`, `Observed`, and the generated
  `ObserveClient`/`ObserveDispatcher` now come from `vx-exec-protocol`.
- `vx-runtime::observe` no longer declares the Vox service contract or wire
  value/result enums. It owns only `CompletedRun` and `evaluate_observer`.
- `vx-runner`, `vx-daemon`, and the strand scheduler use the protocol crate for
  Observe service/client/dispatcher types.
- `source_blind_guardrails` now scans `vx-runtime/src` for the deleted protocol
  declarations as runtime exec ownership affordances.

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

- `cargo check -p vx-exec-protocol -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner -p vx-daemon`
  passed.
- `cargo nextest list -p vx-runtime -E 'test(observer_evaluation_accepts) | test(observe_runs_a_real_command) | test(observe_crosses_the_output_tree) | test(the_executor_serves_run_outputs)'`
  selected only `observer_evaluation_accepts_a_completed_process_without_spawning`;
  the three scratch process/Producing tests have been deleted.
- `cargo nextest run -p vx-runtime -E 'test(observer_evaluation_accepts) | test(observe_runs_a_real_command) | test(observe_crosses_the_output_tree) | test(the_executor_serves_run_outputs)' --no-fail-fast`:
  1/1 passed.
- `cargo nextest list -p vx-runtime -E 'binary(source_blind_guardrails) | binary(exec_on_strands)'`
  selected 4 tests.
- `cargo nextest run -p vx-runtime -E 'binary(source_blind_guardrails) | binary(exec_on_strands)' --no-fail-fast`:
  4/4 passed.
- The raw protocol-ownership scan
  `rg -n 'pub struct ExecCommand|pub struct ObserveRequest|pub enum WireValue|pub enum Observed|pub trait Observe|ScratchObserveService|local_observer|fn scratch_dir|fn capture_outputs|tokio::process::Command|pub struct ObserveService|struct ObserveService' crates/vx-runtime/src`
  returned no matches.
- `cargo nextest run -p vx-runtime --no-fail-fast`: 140/140 passed.
- `cargo nextest list -p vx-runner --run-ignored all` selected
  `output_tree::tests::reads_declared_output_root_from_cas`,
  `sandbox::tests::seatbelt_confines_writes_to_the_output_root`,
  `tests::backtick_exec_l2_cache_survives_fresh_runner_backed_graph_eval`,
  `tests::bare_invocation_failure_poisons_through_runner_backed_live_vfs`,
  `tests::declared_tool_fingerprint_disambiguates_runner_backed_l1_entry`,
  `tests::demanded_vixen_exec_reaches_runner_backed_observe`,
  `tests::demanded_vixen_exec_runs_through_runner_backed_live_vfs_and_serves_output_tree`,
  `tests::executable_fingerprint_disambiguates_runner_backed_l1_entry`,
  `tests::explicit_stream_observer_returns_runner_backed_live_output_tree`,
  `tests::hello_world_uses_runner_backed_live_vfs_and_where_injection`,
  `tests::identical_backtick_exec_boundaries_share_runner_backed_l1_entry`,
  `tests::observer_identity_disambiguates_runner_backed_l1_entry`,
  `tests::producing_vconn_serves_runner_output_tree_from_cas`,
  `tests::run_ok_observer_crosses_runner_backed_live_vfs_for_success_and_failure`,
  `tests::strand_observe_reaches_runner_backed_service`,
  `tests::strand_observe_uses_runner_backed_live_vfs_for_success_and_failure`,
  `tests::undemanded_vixen_exec_with_runner_backed_client_stays_cold`,
  `tests::writable_roots_disambiguate_runner_backed_l1_entry`.
- `cargo nextest list -p vx-runner --run-ignored only` selected the ignored
  live-VFS proof targets:
  `tests::backtick_exec_l2_cache_survives_fresh_runner_backed_graph_eval`,
  `tests::bare_invocation_failure_poisons_through_runner_backed_live_vfs`,
  `tests::declared_tool_fingerprint_disambiguates_runner_backed_l1_entry`,
  `tests::demanded_vixen_exec_runs_through_runner_backed_live_vfs_and_serves_output_tree`,
  `tests::executable_fingerprint_disambiguates_runner_backed_l1_entry`,
  `tests::explicit_stream_observer_returns_runner_backed_live_output_tree`,
  `tests::hello_world_uses_runner_backed_live_vfs_and_where_injection`,
  `tests::identical_backtick_exec_boundaries_share_runner_backed_l1_entry`,
  `tests::observer_identity_disambiguates_runner_backed_l1_entry`,
  `tests::run_ok_observer_crosses_runner_backed_live_vfs_for_success_and_failure`,
  `tests::strand_observe_uses_runner_backed_live_vfs_for_success_and_failure`,
  and `tests::writable_roots_disambiguate_runner_backed_l1_entry`.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::identical_backtick_exec_boundaries_share_runner_backed_l1_entry)' --no-fail-fast`:
  failed at live CAS-VFS boot because this host does not have the Vixen
  Management mount trigger installed (`/Library/Application Support/Vixen/management/queue/mount`
  was missing). This is the expected environmental boundary for ignored
  live-VFS proofs; do not replace it with scratch execution.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::writable_roots_disambiguate_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same live CAS-VFS boot boundary on this host, before any scratch
  fallback could run.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::observer_identity_disambiguates_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same live CAS-VFS boot boundary on this host, before any scratch
  fallback could run.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::declared_tool_fingerprint_disambiguates_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same live CAS-VFS boot boundary on this host, before any scratch
  fallback could run.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::executable_fingerprint_disambiguates_runner_backed_l1_entry)' --no-fail-fast`:
  failed at the same live CAS-VFS boot boundary on this host, before any scratch
  fallback could run.
- `cargo nextest run -p vx-runner --run-ignored only -E 'test(=tests::backtick_exec_l2_cache_survives_fresh_runner_backed_graph_eval)' --no-fail-fast`:
  failed at the same live CAS-VFS boot boundary on this host, before any scratch
  fallback could run.
- `cargo nextest run -p vx-runner --no-fail-fast`: 6/6 passed, 12 skipped
  (the live-VFS proof targets).
- `cargo nextest run -p vx-exec-protocol -p vx-services -p vx-runtime-graph -p vx-runtime-core -p vx-runtime-driver -p vx-runtime -p vx-runner --no-fail-fast`:
  172/172 passed, 12 skipped.
- `cargo nextest run -p vx-daemon --no-fail-fast`: 2/2 passed.
- The forbidden-symbol scans over `vx-runtime-core`, `vx-runtime-driver`, the
  deleted tree-walker affordance scan over `vx-runtime/src`, and the
  runtime-owned exec/observe protocol scan over `vx-runtime/src` returned no
  matches.

Important caveat: the green default suite still skips live CAS-VFS proofs. The
scratch-backed `exec_output_tree`, `hello_world`, demanded `exec_on_strands`,
and runtime-local observe process-runner shapes now live as runner-owned proof
targets. On this host the ignored live proofs fail at VFS boot before any
fallback could run.

## Current next step

Finish M6:

1. On a host with the Vixen CAS-VFS mount, run the ignored live proof with
   `cargo nextest run -p vx-runner --run-ignored only --no-fail-fast` and record
   the result. If the host cannot supply CAS-VFS, the proof should fail at the
   explicit VFS boot/runner-backed boundary, never by switching to scratch
   execution.
2. Move `BoundaryL2Cache`/input-key lookup toward the daemon/orchestrator
   cache/index and `vx-store`, instead of letting `vx-runtime` remain the cache
   owner.
