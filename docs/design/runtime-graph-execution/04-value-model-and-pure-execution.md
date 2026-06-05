# Runtime graph execution - value model and pure execution

This document is self-contained. It describes the value-model cutover and the
order for making pure Vixen programs run through graph execution.

## Complete task context

The current public `vx-runtime::eval` API still exposes a payload enum
(`Value::U64`, `Value::Struct`, `Value::Tuple`, etc.) plus API-edge thunks for
graph/native boundary fields. Expression-capturing thunks have been deleted, but
the public value shape is still transitional. The intended model is different: a
value in the orchestrator is a node reference; resolved payload is per-node memo
state. Cheap scalars live inside kernels or boundary materialization, not as
arbitrary values passed through an interpreter.

The graph/core/driver split exists and the source tree-walker is deleted. The
work here is to finish migrating runtime currency to node references and make
pure language features run through the graph driver without relying on public
`Value` as an internal currency.

## Value model target

Runtime graph value:

```text
GraphValue = NodeRef | RemoteHandle | BoundaryHandle, as needed
```

Node memo/output:

```text
NodeMemo {
  state: Unready | InProgress | Ready(NodeOutput) | Poison(...)
}
```

Node output payload:

```text
NodeOutput {
  Scalar words and bools for graph-visible resolved values,
  String/blob/tree handles,
  LazyData field node refs,
  Variant tag + payload node refs,
  Closure function + capture node refs,
  Boundary-produced handles,
}
```

Current progress:

- `vx-runtime-driver` stores an explicit per-node memo state:
  `InProgress`, `Ready(NodeOutput)`, or `Poison(CorePoison)`.
- Core still sees only a snapshot of ready node outputs through `ReadyInputs`;
  in-progress and poisoned memo entries are not presented as ready values.
- A demanded poison is memoized at the detonating node. Re-demanding that node
  returns the memoized poison instead of recomputing the node.
- Boundary requests clear the requesting node's `InProgress` state before
  returning to the graph executor, so legal boundary materialization can seed the
  node and resume the same demand.
- `Value` and `Thunk` remain public/API-edge shapes. They are not used as the
  driver memo currency.

Things to remove from execution:

- expression-capturing thunks (deleted in the demolition checkpoint)
- `ThunkState::Unforced { expr, env, body }` (deleted in the demolition
  checkpoint)
- `Value` as the orchestrator's primary currency
- helper methods that force a thunk to inspect it
- source-shaped pattern matching in execution

The public API may temporarily convert graph outputs into the old `Outcome` type
for tests, but execution must not be driven by the old value enum.

## Pure execution order

The order below is designed to restore the `.vx` suite one semantic slice at a
time without reintroducing fallback.

### 1. Constants, bomb, fail

Implement:

- typed constant outputs
- bomb node poison
- fail node poison, including demanded message/error payload

Vixen tests targeted:

- `poisons.vx`
- simple literal/assert tests

Acceptance:

- Demanding a bomb node poisons.
- An undemanded bomb is not memoized.
- Fail does not require source expressions.

### 2. Lazy products and projection

Implement:

- tuple/array/struct lazy data nodes
- field/index projection nodes
- known-constructor projection folding

Vixen tests targeted:

- `demand_basis.vx`
- `which_field_which_graph.vx`
- product tests

Acceptance:

- Lazy data construction does not demand fields.
- Projection demands only selected slot.
- Dead sibling bombs remain unmemoized.

### 3. Params, let aliases, and environments

Implement graph-level parameter binding:

- function params map to supplied argument node refs
- let bindings become graph aliases/node refs
- aliases do not evaluate the aliased node

Vixen tests targeted:

- `bindings.vx`
- simple call argument cases once calls exist

Acceptance:

- `let x = bomb; 42` does not demand `x`.
- `let q = p; q[0]` reaches `p[0]` through graph refs.

Current progress:

- Let aliases bind names to graph node refs.
- Public body arguments are seeded into the body graph. Scalar arguments become
  ready graph outputs; tuple/array/struct/variant arguments become graph outputs
  whose fields are external boundary nodes.
- Native/resolved external field thunks are materialized only when graph demand
  reaches their boundary nodes. Legacy expression thunks that leak into this path
  poison with an explicit graph-boundary error instead of evaluating source.
- Statically resolved direct calls expand at analysis time. Callee params bind
  to the caller's argument node refs, so unused arguments remain undemanded.
- Closure calls and named functions used as values expand when the analyzer can
  resolve the callable template. Closure construction stores capture node refs
  and does not demand them.
- Expanded direct and closure calls now leave a source-blind `Call` graph node
  around the expanded body root. Core demands only that body-root input, not the
  argument edges recorded for future memo keys.
- Callee expression memoization remains scoped per expansion; call memo output
  may share one expanded result only when the driver can compute a key before
  running the body.
- Direct and closure call nodes carry a code identity and share a per-run driver
  call memo when every argument and capture output is already resolved before the
  body runs. The memo is intentionally not used for unresolved arguments or
  captures.

### 4. Scalar kernels

Implement:

- kernel nodes with strict scalar inputs
- arithmetic
- boolean ops
- comparisons
- divide/remainder poison
- kernel interpreter path

Vixen tests targeted:

- `arithmetic.vx`
- comparison/control-flow tests

Acceptance:

- Kernel execution receives only ready strict inputs.
- Kernel nodes do not recursively force arbitrary graph nodes.

Current progress:

- Graph core executes arithmetic, comparisons, boolean ops, `if`, `assert`, and
  `assert_eq`.
- Graph core executes `String.len()` as `KernelOp::StrLen`.
- Graph core executes `match` for literal, wildcard, and binding patterns by
  demanding only the scrutinee and selected arm body.
- Graph core executes variant construction, variant projection, and variant-tag
  match selection. Analyzer binds variant payload fields as graph projection
  nodes inside selected arm bodies.
- The graph driver counts ready kernel nodes for the transitional public
  dispatch counter.
- `arithmetic.vx`, direct-call cases in `calls.vx`, and graph `if` demand tests
  pass through the source-entry graph path.
- Kernel failures become graph poison with source span when available.

### 5. If/control flow

Implement:

- graph/kernel control flow demand rule
- demand condition first
- demand selected branch only
- do not demand dead branch

Vixen tests targeted:

- `control_flow.vx`

Acceptance:

- `if true (42) else (bomb)` does not demand bomb.
- `if` inside scalar kernel short-circuits.

### 6. Equality

Implement:

- scalar equality
- string equality
- product/variant equality by demanding compared fields

Vixen tests targeted:

- assertions in most `.vx` files

Acceptance:

- Equality demands exactly the fields needed for comparison.
- Product equality does not force unrelated graph structure.

### 7. Variants and match

Implement:

- unit variant construction as value node, not param
- tuple/struct variant payload nodes
- match scrutinee demand
- tag selection
- pattern bindings as node refs
- selected arm body demand only

Vixen tests targeted:

- `variants.vx`
- `match.vx`
- error union tests later

Acceptance:

- `Off`/`Zero` are executable graph values.
- Unselected match arms are not demanded.
- Pattern-bound payload is lazy until read.

Current progress:

- Unit variants and payload variants are graph values, not params.
- Variant constructors carry the selected tag and field-name payloads.
- Variant payloads are stored as node refs and are only demanded through
  projection.
- Variant match selection is tag-driven in graph core.
- Variant payload bindings are graph projection aliases inside selected arms.

### 8. Function calls

Implement:

- function entry graph template or body graph instantiation
- argument node binding
- call result as demanded callee graph root
- per-call memo hook shape, even before persistent cache

Vixen tests targeted:

- `calls.vx`
- arithmetic through helper functions

Acceptance:

- Function arguments are not forced at call boundary unless demanded by callee.
- `pick_first((42, bomb))` does not demand bomb.

Current progress:

- Direct calls and known closure calls instantiate the body graph and wrap the
  result in `NodeKind::Call { function, code_identity, arg_count, capture_count }`.
- `vx-runtime-core` executes a call node by demanding only its expanded body-root
  input. Additional argument inputs are retained as graph identity/memo-hook
  material, not as strict dependencies.
- `vx-runtime-driver` has a per-run memo for direct and closure calls keyed by
  function entry, code identity, and already-ready argument/capture outputs. It
  does not demand unresolved arguments or captures to build a key.
- The analyzer now emits source-derived direct/closure code identities from
  structured body material plus transitive identities for referenced function
  items (`blake3:<hash>` for functions, `closure:blake3:<hash>` for closures),
  not from names alone or source spans. Mutually recursive functions share one
  SCC fingerprint; the call node's function entry disambiguates memo keys.
- When an L2 cache directory is configured, graph execution can persist
  self-contained function outputs (`U64`, `Bool`, `String`, `Unit`) under the
  call key. Outputs containing graph-local node refs remain per-run only.

### 9. Closures and captures

Implement:

- closure output payload: function id/template + capture node refs
- call closure through graph driver
- captures remain node refs

Vixen tests targeted:

- `closures.vx`

Acceptance:

- Closure construction does not force captures.
- Closure in dead data slot is not evaluated.
- Captured local used by demanded call resolves through graph refs.

### 10. Strings and pure methods

Implement:

- string constants
- interpolation as kernel/pure graph node where possible
- string `len`, `slice` as pure methods/nodes
- concat

Vixen tests targeted:

- `strings.vx`
- failure message tests

Acceptance:

- String ops do not require expression fallback.
- Methods are structured graph payloads.

### 11. Failure context and `?`

Implement:

- graph poison propagation with source span
- demand path/call chain if required
- `?` as graph-level catch/reify
- `Failure { err, context }` values as graph outputs

Current implemented slice:

- `CorePoison` carries the demanded graph node, typed graph `err` output, source
  span, and graph failure context.
- `NodeKind::Fallible` and `NodeKind::CaughtFailure` reify `expr?` as
  `Ok(value)` or `Err(Failure { err, context })`.
- The graph driver catches poison from the fallible inner demand and memoizes the
  caught-failure node.
- Projection over a caught failure exposes `err` and `context` fields.
- `fail f` over a caught failure re-raises the original `err` and context.
- `graph_exec` converts graph failure outputs into the old public `Value` and
  `Outcome` API while that adapter still exists.

Current API-edge adapter:

- `Outcome::Clean(Value)` remains as the public compatibility shape, but it is
  now constructed from a sealed resolved-node handle after root demand.
- Public tuple/array/struct/variant fields are graph/native boundary thunks.
  Forcing one issues `DemandRequest::PublicValueField` through an authorized
  `PublicValueDemand` token minted only from node IDs present in that resolved
  output.
- Native graph-boundary thunks are no longer publicly constructible:
  `Thunk::native` is crate-private, and the guardrail suite rejects a public
  native thunk constructor. Runtime-owned adapters can still build native fields
  for executor-local `Run` and graph boundary crossing, but external callers
  cannot smuggle arbitrary Rust callbacks into public values.
- Executor-local output trees use a graph `OutputTree` marker and cross to
  `WireValue::Tree` during observer wire crossing.

Vixen tests targeted:

- `failures.vx`
- `error_union.vx`

Acceptance:

- Catching a poison reifies the same error/context.
- Re-raising preserves context.

## Acceptance for pure execution phase

Pure graph execution is complete when the non-boundary `.vx` tests pass through
graph driver with no expression fallback:

- `arithmetic.vx`
- `bindings.vx`
- `calls.vx`
- `closures.vx`
- `control_flow.vx`
- `demand_basis.vx`
- `match.vx`
- `poisons.vx`
- `products.vx`
- `strings.vx`
- `variants.vx`
- pure parts of `failures.vx` and `error_union.vx`

If a test is still red, it must be red as graph unsupported/poison behavior, not
because the old evaluator was invoked.
