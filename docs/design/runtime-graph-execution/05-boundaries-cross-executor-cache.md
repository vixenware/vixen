# Runtime graph execution - boundaries, cross-executor runtime, and cache

This document is self-contained. It describes how to port boundary execution,
cross-executor values, and caching onto the graph runtime.

## Complete task context

Vixen's runtime is a demand graph. Pure computation runs in eager kernel islands.
Effects and external observations are boundary nodes. Boundary materialization is
one of the only legal origins of demand. It is not an excuse for arbitrary
internal `force`; the boundary node declares what must cross, and the driver
materializes exactly that payload.

The current runtime has exec, host operations, remote tree handles, observer
shipping, and strand tests wired through the old `vx-runtime::eval` path. Those
must move to graph driver semantics after pure graph execution is established.

## Legal demand origins

The driver should recognize only a small set of demand origins:

- root result demanded by CLI/API/test harness
- boundary payload materialization
- boundary-produced output materialization
- explicit test probe

Core does not create demand origins. It can only report `Need(nodes)` or
`Boundary(request)`.

## Boundary node categories

### Grammar import and tool values

Graph payloads needed:

- grammar package coordinate
- grammar name coordinate
- tool record layout: command coordinate + binary node

Execution:

- importing a grammar package returns a graph value representing available
  grammar coordinates.
- selecting `g.cc` is projection over that value.
- command validation should operate over structured command payloads, not source
  expressions.

### Where/capability injection

Graph payloads needed:

- capability spec identity
- structured predicate
- provider candidates or provider registry handle

Execution:

- demand on `Type where(predicate)` asks driver/control plane for matching
  provider.
- predicate evaluation may demand provider attributes.
- no provider match returns poison/unsupported according to current semantics.

### Host methods

Host methods are boundary or host-observation nodes:

- `host.which(name)`
- `host.exec(args)`

Demand:

- materialize method arguments required to cross host boundary.
- run host operation.
- memo output in node table.

Do not hide host methods inside source-level method dispatch.

Current progress:

- `host.which` and `host.exec` lower to typed graph `HostMethod` nodes.
- Core demands receiver/direct inputs, then reports the host method as a
  boundary.
- Runtime graph execution materializes the boundary by demanding argv/name
  payload nodes through `DemandRequest::BoundaryPayload`, running the host
  operation, and memoizing the graph output.
- `host.exec` returns a graph struct with `exit_code`, `stdout`, `stderr`, and
  `fingerprint` fields.
- `host.exec` has an in-process input-keyed L1. The key is computed before
  spawning from the materialized argv plus the executable's content fingerprint;
  the cached output is not part of the key.

### CLI invocation/backtick/exec

Graph payloads needed:

```text
ExecBoundaryPayload {
  tool: NodeId,
  args: Vec<CliArgPayload>,
  observer: ObserverPayload,
  mounts: Vec<NodeId>,
  env: Vec<NodeId>,
}
```

Demand profile:

- upfront inputs: tool identity, argv, env, mounts, observer code/captures
- progressive inputs: mounted tree bytes demanded by executor
- produced outputs: stdout/stderr streams, observer value, output tree

Execution:

- driver receives boundary request.
- driver materializes upfront payload.
- driver dispatches to executor.
- produced handles enter memo as graph outputs.

Current progress:

- Backtick command literals lower to graph `Boundary` nodes plus runtime-local
  exec metadata keyed by graph `NodeId`. The metadata carries CLI argument
  templates as text plus input node refs, and carries an explicit observer as a
  static `vx_ir::Bundle`.
- Bare invocations carry no observer bundle; the executor applies the default
  `run.ok.out` observer.
- Observer bundles have a stable `blake3:` content identity computed from the
  deterministic Facet JSON encoding of the packed `vx_ir::Bundle`. This is the
  observer-code component needed for an exec boundary cache key.
- Demand on the exec boundary materializes only request-shaping inputs: the
  leading tool node, its command coordinate, its `binary` field, and each
  demanded argv hole. Merely constructing an exec boundary or a lazy product
  containing one does not run the command.
- Successful backtick invocations now have an in-process exact L1 keyed by the
  current materialized request identity: command coordinate, binary string,
  pre-run executable content fingerprint, rendered argv, and observer identity.
  Bare binary names are resolved through `PATH` for fingerprinting. Cache hits
  seed the demanded graph node without calling `dispatch_observe`.
- Observe dispatch happens from graph boundary materialization through
  `dispatch_observe`, then the resulting graph node is seeded and the same demand
  loop resumes.
- A top-level data observer result becomes graph output structure. A top-level
  producing tree result becomes `NodeOutput::OutputTree` plus a runtime remote
  handle entry keyed by the exec node.
- `exec_on_strands` passes, including a cold-field tripwire where a failing exec
  in tuple slot 0 stays undemanded while slot 1 is selected, an exact-L1 test
  where two identical demanded backtick invocations dispatch once, a
  non-aliasing test where different observer bundles dispatch separately, and a
  binary-fingerprint test where a stable command path dispatches again after its
  executable content changes.
- `exec_boundaries.vx` passes through `lang_suite`: a real backtick exec in an
  undemanded tuple slot stays cold even though the source entry has no observe
  client available.

### Observer execution

Observer payload should be graph/executable code, not source callback:

- observer graph/function id
- captured node refs
- executor-local values such as `Run`, `Process`, `Tree`, `Host`

Executor links the same graph evaluator semantics. It binds executor-local
values as graph nodes and drives observer root demand.

Current progress:

- `ObserveService` still receives an observer bundle, but evaluating that bundle
  calls `eval_body_wire`, which now enters graph body execution.
- `Run` is bound as a graph external struct argument. Demanding `run.ok` or
  `run.stream` materializes native boundary fields only when reached.
- Graph observer results cross to `WireValue` through graph/native boundary
  thunks. The old evaluator's `to_wire` method has been deleted; the public
  observer path uses graph body execution plus API-edge graph/native crossing.
- The executor-local `run.stream` value now matches the stdlib `Live` layout:
  `stdout`, `stderr`, then `out`. The stream fields poison explicitly if
  demanded before streaming lands; `out` crosses as `WireValue::Tree`.

## Cross-executor remote values

Remote tree/stream values are graph outputs that may be pending because another
host has not produced a part.

Driver behavior:

- demanding a produced local part returns it immediately.
- demanding an unproduced part parks the strand.
- producer resolution wakes parked demand.
- producer completion without part poisons.

Graph shape:

```text
RemotePartNode {
  handle: RemoteHandle,
  key: PartKey
}
```

The boss/driver owns scheduling and parking. Core reports the need; it does not
block.

Current progress:

- `tree.file(path)` lowers to a graph boundary with runtime-local metadata.
- A remote `Tree` supplied as a public body argument or returned as a top-level
  observe result seeds the destination graph node as `NodeOutput::OutputTree`
  and records its `HandleId` in the runtime boundary context.
- Demand on the `.file(path)` boundary demands the receiver node and path node,
  then calls `force_remote(handle, PartKey::TreePath(path))`. Only that path's
  bytes cross; absent parts poison through the graph boundary.
- `remote_tree_via_eval` and `exec_output_tree` pass through graph execution.

## Strand integration

The strand scheduler is the driver for asynchronous progress. The graph driver
should produce step results that the strand scheduler can interpret:

- `NeedLocal(nodes)`
- `NeedBoundary(payload)`
- `NeedRemotePart(handle, key)`
- `Ready(output)`
- `Poison`
- `Unsupported`

No code inside core should wait on remote I/O. Waiting/parking belongs to driver
and strand scheduler.

## Cache roadmap

Caching must be added in this order.

### 1. Per-run node memo

Already conceptually present in driver. Every node has:

- unready
- running/in-progress
- ready output
- poison

This is Tier 1 in-memory memo for one run.

Current progress:

- `vx-runtime-driver` now represents this tier as explicit per-node memo state:
  `InProgress`, `Ready(NodeOutput)`, or `Poison(CorePoison)`.
- Driver tests assert that a demanded bomb memoizes poison, and that a boundary
  request does not leave its node stuck `InProgress` before boundary
  materialization seeds the result.

### 2. Function-call memo

Key:

```text
function code identity + resolved argument values
```

Rules:

- key computed before running body
- only resolved values participate
- no read-set in key

Current progress:

- Direct and closure calls have a per-run driver memo keyed by code identity plus
  argument and capture outputs that are already resolved before the body runs.
- Unresolved argument or capture nodes deliberately do not participate: the
  driver skips the call memo key instead of demanding them.

### 3. Boundary L1 input-keyed cache

Exec key:

```text
observer code identity
observer captures
executable content fingerprint
argv
env
mount identities
resolved toolchain/capability identities
```

Rules:

- key computed before running
- output not part of key
- executor selector string not part of key
- capability/toolchain identities are part of inputs

Current progress:

- `host.exec` has a local in-process L1 keyed by argv plus the executable
  fingerprint. `host_exec_uses_input_keyed_l1_cache` proves two identical graph
  host boundaries demand both outputs but spawn only once.
- `host.exec` also has an opt-in local disk L2 rooted at `VX_RUNTIME_L2_DIR` or
  an explicit runtime cache directory. Successful `HostExec` records persist by
  the same input key and can be reused by a fresh graph evaluation.
- Observer bundles now have a content identity suitable for the observer-code
  part of a backtick exec key.
- Function calls now leave a source-blind graph `Call` node around the expanded
  body root. Direct and closure calls have a per-run memo keyed by code identity
  plus already-ready argument and capture outputs; unresolved arguments and
  captures are not demanded to build keys.
- Backtick exec has a local in-process exact L1 for successful observer values
  and output-tree handles. The current key covers the command coordinate, binary
  string, pre-run executable content fingerprint, rendered argv, and observer
  identity for the materialized request. Successful observer data values can
  persist to the opt-in local disk L2 by that key. Output-tree handles remain
  L1-only because scheduler handle ids are per run. Full env/mount/toolchain
  identities, CAS-backed L2, function-output L2, and read-set provenance are
  still unimplemented.

### 4. L2 content-addressed storage

Persist boundary/function outputs by content identity. This is the storage layer,
not a reason to change cache key semantics.

Current slice:

- `host.exec` persists successful `HostExec` records in a local disk L2 when a
  runtime cache directory is provided or `VX_RUNTIME_L2_DIR` is set.
- Backtick exec persists successful observer data values in the same local disk
  L2. It deliberately does not persist output-tree handles yet.
- Cache records are encoded with `facet-json`, and file paths are derived from a
  blake3 hash of the Facet JSON cache key.
- This is not the final CAS-backed L2 and it does not record read-set witnesses.

### 5. Read-set provenance

Read-set is discovered after running. It supports reuse/rerun avoidance; it is
never used as the primary key. If an implementation tries to key by read-set, it
must already have run the command and is therefore wrong.

Coarse misses can still become precise hits. The pre-run request key may include a
broad mounted tree identity because the runtime cannot know the exact include set,
response-file reads, or generated input paths before dispatch. When that coarse
identity changes, lookup moves to a candidate-validation path:

1. Narrow candidates by stable request shape: observer identity, resolved command
   shape, env, mount roles, capability/toolchain identities, and impurity marks.
2. Validate one candidate's recorded read-set witness against the current input
   handles. The witness names exactly the paths, byte ranges, or subtrees that
   candidate read and the content identities observed there.
3. Reuse the candidate only if its witness still matches. Otherwise try the next
   candidate within budget, or run the command.

Do not form the union of every file read by every prior candidate. That turns a
precision mechanism into a broad rehash. Candidate validation must be grouped and
bounded:

- group records with the same read-set shape so a witness is checked at most once
  per lookup.
- use batched tree/Merkle queries, changed-path summaries, and subtree witnesses
  instead of per-file filesystem reads whenever possible.
- if the candidate set remains huge, prefer recent/cheap candidates and then miss.
  Rerunning is sound; unbounded validation is not.
- if two validated candidates with the same request shape and same current read
  bytes disagree on output identity, treat that as impurity/corruption or an
  incomplete witness. Do not choose arbitrarily.

## Current boundary/cross-executor checkpoint

Verified green:

- `cargo nextest run -p vx-runtime -E 'binary(host_probe)'`: 6/6 passed,
  including the input-keyed `host.exec` L1 cache test, bare executable-name
  fingerprint resolution, and disk-L2 reuse across fresh graph evaluations.
- `cargo nextest run -p vx-runtime -E 'binary(bundle)' --no-fail-fast`: 3/3
  passed, including stable, content-sensitive observer bundle identity.
- `cargo nextest run -p vx-runtime -E 'test(observe_runs_a_real_command_and_returns_the_observer_value) | test(observe_crosses_the_output_tree_as_a_handle_marker) | binary(observe_on_strands)' --no-fail-fast`: 4/4 passed.
- `cargo nextest run -p vx-runtime -E 'binary(exec_on_strands)' --no-fail-fast`:
  7/7 passed, including cold exec non-demand, exact backtick exec L1 reuse,
  observer-identity non-aliasing, executable-fingerprint non-aliasing, and
  disk-L2 reuse across fresh graph evaluations.
- `cargo nextest run -p vx-runtime -E 'binary(exec_output_tree)' --no-fail-fast`:
  3/3 passed.
- `cargo nextest run -p vx-runtime -E 'binary(remote_tree_via_eval)' --no-fail-fast`:
  3/3 passed.
- `cargo nextest run -p vx-runtime -E 'binary(cc_probe) | binary(hello_world)' --no-fail-fast`:
  2/2 passed.
- In the full `vx-runtime` package run, observe, observe-on-strands, exec,
  output-tree, remote-tree eval, grammar import/toolchain, and
  requested-toolchain capability tests now pass through graph execution.

Newly green boundary payloads:

- Grammar-package imports lower to graph lazy records with grammar-coordinate
  graph values.
- `where` capability requests lower to graph boundary nodes with typed
  capability metadata and materialize by selecting an advertised provider.
- Per-run node memo is explicit driver state for in-progress, ready, and poisoned
  nodes.
- Direct and closure calls have a local per-run call memo keyed by code identity
  plus already-ready argument and capture outputs.
- `host.exec` has a local input-keyed L1 cache.
- `host.exec` has an opt-in local disk L2 for successful results.
- Backtick exec has a local exact L1 cache for the current materialized request
  identity, including a pre-run executable content fingerprint.
- Backtick exec has an opt-in local disk L2 for successful observer data values.

No memo path demands unresolved arguments or captures to build a key. CAS-backed
L2 storage, output-tree-handle persistence, function-output persistence, and
read-set provenance have not been implemented in this checkpoint.

## Acceptance criteria

Boundary/cross-executor phase is complete when:

- exec tests pass through graph driver.
- observer evaluation runs graph payloads, not source callback evaluation.
- remote tree tests pass through graph driver/strand semantics.
- host `which`/`exec` are boundary nodes or structured host nodes.
- boundary payload materialization is explicit in driver.
- cache keys are computed before running.
- read-set is recorded after running and not included in key.
