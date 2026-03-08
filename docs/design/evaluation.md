# Vixen evaluation model

Fundamental truths about how Vixen evaluates programs.

## Demand-driven dataflow

Vixen uses demand-driven dataflow evaluation. The computation forms a
dataflow graph — nodes are computations, edges are data dependencies.
But the graph is not built upfront and executed eagerly. It is discovered
lazily as evaluation proceeds.

Evaluation starts from what is demanded — the output `Tree` that the build
must produce. To evaluate a node, the evaluator looks at its dependencies
and evaluates those first. If a node has multiple independent dependencies,
they can be evaluated concurrently. If a node is never needed by anything
downstream of the demand, it is never evaluated.

A binding that is never read is never evaluated. A function that is never
called is never entered. Only the subgraph reachable from the demanded
output is ever computed.

## Values and pending-ness

Pending is a property of a **value**, not of a binding.

A value is either resolved (the data is available) or pending (the data
is not yet available). Any value of any type can be pending — scalars,
structs, arrays, all of them. A struct can exist with some fields resolved
and some fields pending — the struct itself is not "pending," it is a
struct. You can access its fields. If you access a resolved field, you
get the data. If you access a pending field, you get a pending value.

You can freely:

- Pass a pending value to a function.
- Store a pending value in a struct field.
- Put a pending value in an array.
- Return a pending value from a function.

None of these operations wait. They are just moving handles around.

## What forces a value

Every expression is just a node that takes inputs and produces a pending
value. There are no "inspection points" that force values mid-graph.
`a + b`, `match x { ... }`, `a == b` — these are all nodes like any
other. They depend on their inputs and produce a result. If nothing
demands the result, neither the expression nor its inputs are evaluated.

Demand flows backward from two boundaries:

- **Final output materialization** — the `Tree` returned by `build()` must
  be written to disk. This is the root of all demand.
- **The yield boundary** — when an observer yields a value back to the
  orchestrator, store handles in that value must resolve so the executor
  can be released.

Everything else is just demand propagating through the graph. `match x`
depends on `x` — when something downstream demands the match's result,
that demand flows into `x`, which flows into whatever produces `x`.

## Where pending values come from

Pending values come from **streams**. Any function that returns
`Stream[T]` and uses `yield` produces values over time. Operations on
a stream (like `find`) return pending values — handles to data that
hasn't been produced yet.

`exec` is one way to get a stream, but it's not special — it's a built-in
that happens to return `Stream[T]`. User code can create streams with
`yield` in any function that returns `Stream[T]`.

## The dataflow graph

Nodes are computations — function calls, `exec` invocations, expressions
that produce values. Edges are data dependencies between them. A node can
be evaluated when its inputs are available. "Available" does not mean fully
resolved — a struct with pending fields is available, an array with pending
elements is available. It just means the value exists.

Nodes with no dependencies between them can be evaluated concurrently.

## Poisoned values

When a stream's producer fails (e.g., a process crashes in an `exec`),
the stream terminates. Any pending value derived from that stream becomes
**poisoned**.
A poisoned value propagates its error at inspection points — the same
places where a pending value would force resolution.

If multiple inputs to a node are poisoned, the node collects all errors.
Error reporting is deterministic — the same set of errors regardless of
timing.

## Memoization

When the evaluator demands a node, it first checks whether that node has
already been computed with the same inputs. If so, it returns the cached
result without re-evaluating. This is what makes incremental builds work —
on a rebuild, most nodes have the same inputs as before and their results
are reused.

The cache key is **(function, arguments)**. Because all values are hashable
and comparable (universal value properties), any function call can be
memoized transparently.

Functions with `@ttl` annotations are cached provisionally — the runtime
revalidates them according to their policy. If the revalidated result is
the same as the cached result, downstream nodes are not re-evaluated
(early cutoff).
