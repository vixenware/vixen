# Runtime and Execution Model

For language semantics, see [language.md](language.md). This document covers
how the runtime evaluates, memoizes, and executes Vixen programs.

## Architecture

The runtime has four components:

- **Orchestrator**: evaluates Vixen source, maintains memo tables, dispatches
  work. Lives near the source tree.
- **Store**: content-addressed storage for blobs and trees. BLAKE3 hashed.
  Shared across builds.
- **Executors**: run sandboxed `exec` computations. May be local or remote.
  Run Vixen code (monitors).
- **VFS**: mounts store-backed content into execution sandboxes. Executors
  don't bulk-copy inputs — they mount them on demand.

### Data flow

1. The user demands a top-level value: `vixen build my_crate`
2. The orchestrator evaluates the Vixen module, tracing the dependency subgraph
   for the demanded value
3. Pure computations (parsing, name resolution) run in the orchestrator
4. Effectful computations (`exec`, `acquire`) are dispatched to executors
5. Executors mount inputs from the store via VFS, run processes, commit outputs
   to the store
6. The orchestrator receives stream events from executors over RPC
7. Downstream computations consume those values and evaluation continues

### Executor code execution

Executors run Vixen code. The monitor function passed to `exec` executes on
the executor, not on the orchestrator. This means:

- stdout parsing happens where the process runs (no streaming raw stdout over
  RPC)
- `store.commit()` happens locally on the executor
- Yielded events are typed values sent to the orchestrator over RPC
- The executor needs a Vixen evaluator (it is already running a Vixen binary)

## Demand-driven evaluation

Evaluation is demand-driven. Nothing is computed until demanded.

- Computations start only when a demanded value depends on them
- Independent computations may run in parallel
- Computations that are no longer needed may be cancelled

Correctness is independent of scheduler order. The runtime may overlap,
reorder, or cancel work freely. The semantic result is the same regardless of
scheduling decisions.

There is no `force`, no `await`, no `realize` in the language. Demand
originates entirely from outside (CLI, API).

## Memoization

Every tracked computation is memoized. This includes:

- Host primitives: `exec`, `acquire`, `download`
- File/tree observations: `read_text`, `tree.entry`, `glob`
- Pure user functions (at named function boundaries)

A memo entry records:
- The computation's identity (function + argument identities)
- A dependency trace (what the computation observed)
- The output value

### Computation identity

A computation's identity includes:
- Function identity (a semantic hash of the function body)
- Argument identities (structural hash of each argument)
- For `exec`: argv, env, input identities, output schema, sandbox policy

If any part of the identity changes, the memo entry is invalid.

### Function identity

Each compiled function gets a semantic hash of its body. If the code changes,
the hash changes, and all memo entries for that function are invalidated.

Standard library changes also invalidate: if `std/rust.vx` changes, all
computations using functions from it get new identity hashes.

Runtime/language version is part of the identity. Upgrading Vixen itself
invalidates the cache.

## Dependency traces

A dependency trace records what a computation actually observed during
evaluation. It is the runtime's proof that a cached result is still valid.

### Structure

```
memo[compile_crate(foo, bar_meta, tc)]:
  trace:
    - (tc.rustc) -> blob(RUSTC_HASH_1)
    - (bar_meta) -> blob(BAR_RMETA_HASH_1)
    - (read(foo/src/lib.rs)) -> blob(SRC_HASH_1)
  output: stream [Metadata(FOO_RMETA_1), Library(FOO_RLIB_1)]
```

### Verification

On the next build, the runtime checks the trace top-down:

1. Is `tc.rustc` still `blob(RUSTC_HASH_1)`?
   - `tc.rustc` depends on `resolve_rust_toolchain` which depends on
     `resolve_channel`. Check those first (recursively).
   - If `resolve_channel(Stable)` still returns "1.80.0", and the toolchain
     tree is the same, then yes.
2. Is `bar_meta` still `blob(BAR_RMETA_HASH_1)`?
   - Check bar's trace recursively. If bar's inputs are unchanged, bar's
     output is unchanged. Yes.
3. Is `read(foo/src/lib.rs)` still `blob(SRC_HASH_1)`?
   - Compare the file's current content hash. If the file changed, no.

If all trace entries match, the cached output is valid. No recomputation.

If any entry doesn't match, the computation re-evaluates.

### Early cutoff

After re-evaluation, the runtime compares the new output to the previous
output. If they are equal (same structural hash), downstream computations do
NOT re-evaluate. The trace is updated (to reflect current input values), but
the output identity is preserved.

This is the critical optimization. Without early cutoff, any change propagates
to every transitive dependent. With early cutoff, changes stop propagating at
the first computation whose output is unchanged.

Concrete scenarios where early cutoff fires:
- Reformatting a TOML file: bytes change, parsed AST is identical
- Adding a private function to a dependency: rlib changes, rmeta is identical
- Regenerating a config file that happens to produce the same content

### Stream memoization

When a computation produces a stream, the memo entry records the full sequence
of stream elements.

When a computation consumes stream elements (via `find`, `filter`, `map`),
each per-element operation is memoized by the element's value identity. This
means:

- If a stream is re-evaluated and produces the same elements, all downstream
  per-element computations are cache hits
- If a stream produces a new element where an old one was, only that element's
  downstream is recomputed
- If elements appear in a different order but have the same values,
  per-element processing is still cached (value identity, not position)

Collection operations (`collect_tree`, `collect_list`) depend on all elements.
Their identity is the structural hash of the collected result.

## Sandboxing

Sandboxing is mandatory for soundness. If a process can observe undeclared
state, memoization becomes unsound — the cached result may not match what a
re-execution would produce.

An `exec` sandbox provides:
- Declared input trees/files mounted via VFS
- Declared environment variables (and nothing else)
- No network access
- No access to the host filesystem outside mounted inputs
- No ambient state (no home directory, no user config, no locale)

The VFS mount is not an optimization — it is how the runtime makes
content-addressed values appear as files without copying them.

## Store lifecycle

### Content-addressed storage

Every blob is stored by its BLAKE3 hash. Every tree is stored by its merkle
root. Identical content has identical identity, regardless of provenance.

### Leases and roots

The store distinguishes:

- **Leased**: in-flight work has produced content that is not yet part of a
  committed result. The lease keeps it alive temporarily.
- **Published**: content is referenced by a committed memo entry or a durable
  root. It is reachable and retained.

When an executor commits outputs during `exec`, those blobs are leased. When
the exec succeeds and the memo entry is written, the memo entry becomes a
durable root that keeps the outputs reachable.

If exec fails or is cancelled, leased objects expire and are eventually
collected.

### GC

Garbage collection operates over:
- Durable roots (memo entries, user pins)
- Active execution leases
- Reachability from roots

Everything unreachable is collectible.

## Failure

### Fail-fast

When a computation fails, the runtime reports immediately. It does not wait
for sibling computations to also fail. If multiple failures are already
available, tie-breaking is deterministic (stable source/dependency order).

### Failure caching

Failures from hermetic computations are cacheable:
- Parse failures from fixed bytes
- Exec failures with hermetic inputs

Failures from transient operations are not cached:
- Network failures from `download`

### Cancellation

If a demanded value no longer needs a computation (e.g. a sibling failed and
the demand root is now failed), in-flight work may be cancelled. No partial
result is published.

### Crash recovery

The minimum guarantee: any published semantic value is durably
reconstructible from the store. The system does not preserve every provisional
artifact after crashes. It does not publish references to data that is not
durable.

## Local and remote execution

Local and remote execution have the same semantics. The differences are
operational:

- Remote execution requires content transport (blobs shipped to the executor)
- Local execution may short-circuit transport (mount from local store)
- Both use the same sandbox, VFS, and input declaration rules

If local execution can see state that remote execution cannot, the model is
broken.
