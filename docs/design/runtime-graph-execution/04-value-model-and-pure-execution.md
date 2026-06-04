# Runtime graph execution - value model and pure execution

This document is self-contained. It describes the value-model cutover and the
order for making pure Vixen programs run through graph execution.

## Complete task context

The current `vx-runtime::eval` uses a payload enum (`Value::U64`, `Value::Struct`,
`Value::Tuple`, etc.) plus thunks that can capture expressions. That shape invites
internal forcing and expression fallback. The intended model is different: a
value in the orchestrator is a node reference; resolved payload is per-node memo
state. Cheap scalars live inside kernels or boundary materialization, not as
arbitrary values passed through a tree-walker.

The graph/core/driver split exists. The tree-walker still exists. The work here
is to migrate runtime currency to node references and make pure language features
run through the graph driver.

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

Things to remove from execution:

- expression-capturing thunks
- `ThunkState::Unforced { expr, env, body }`
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

