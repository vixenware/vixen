# Vixen evaluation model

## Demand-driven dataflow

Vixen uses demand-driven dataflow evaluation.

The runtime calls `build`. It needs the return value. That's the demand.
Everything flows backward from there.

```vixen
fn build(src: Tree, args: Array[String], env: Map[String, String]) -> Result[Tree, BuildError] {
  a = compile_crate_a(src);
  b = compile_crate_b(src);
  output = link(a, b);
  Tree.of({ "target": output })
}
```

To produce the return value, `Tree.of` needs `output`. To produce `output`,
`link` needs `a` and `b`. To produce `a`, `compile_crate_a` needs `src`.
Same for `b`. `src` is already resolved — it was passed in. So the evaluator
enters both `compile_crate_a` and `compile_crate_b`, discovers they each
do an `exec`, gets pending values back for `a` and `b`. Those execs run
concurrently. `link` has two pending inputs — it doesn't get entered until
both resolve. When they do, `link` runs, `Tree.of` runs, `build` returns.

## Where concurrency comes from

There are no concurrency primitives in Vixen. No `spawn`, no `async`, no
`await`, no channels. Concurrency comes from the structure of expressions.

Any expression with multiple independent sub-expressions creates
concurrency. The evaluator demands all sub-expressions, discovers they
are independent, and evaluates them concurrently:

- Function calls: `link(a, b)` — both arguments concurrently.
- Array literals: `[f(x), g(y), h(z)]` — all elements concurrently.
- Struct literals: `Foo { a: f(x), b: g(y) }` — all fields concurrently.
- Tuple literals: `(f(x), g(y))` — all elements concurrently.
- Map literals: `{ "k1": f(x), "k2": g(y) }` — all values concurrently.

The structure of the code is the concurrency model. No annotations needed.

## Pending values

Pending is a property of a **value**, not of a binding or a type.

A value is either resolved (the data is available) or pending (the data
is not yet available). Any value of any type can be pending. A struct can
exist with some fields resolved and some fields pending — the struct itself
is not "pending," it is a struct. You can access its fields. If you access
a resolved field, you get the data. If you access a pending field, you get
a pending value.

You can freely:

- Pass a pending value to a function.
- Store a pending value in a struct field.
- Put a pending value in an array.
- Return a pending value from a function.

None of these operations wait. They are just moving handles around.

## Inspection points

A pending value must resolve when the actual data is needed. This happens
at **inspection points**:

- Comparing for equality: `a == b`
- Pattern matching: `match x { ... }`
- Arithmetic: `a + b`
- Materializing the final output tree to disk

These are NOT inspection points — they do not force anything:

- Constructing a struct, array, tuple, or map
- Accessing a field
- Calling a function
- Returning a value
- String interpolation (produces a structured value preserving provenance,
  not a plain string)

## Streams

A stream is a value that produces other values over time. A stream has a
`.next()` method that returns `Option[T]` — the next value when it becomes
available, or `None` when the stream is exhausted.

Each yielded value is a separate node in the dataflow graph. It becomes
available when its producer yields it.

Streams come from any function that returns `Stream[T]` and uses `yield`.
`exec` is one way to get a stream — it's a built-in that returns
`Stream[T]` — but streams are a general concept.

## Stream combinators

Stream combinators return new streams. The key property: **stream
combinators do not impose ordering.**

`.map(f)` applies `f` to each element of the input stream and yields the
results. But results are yielded as they become available, not in input
order. If element 3 resolves before element 1, the output stream yields
element 3 first.

```vixen
// compile_crate returns a stream of artifacts
// .map applies build_artifact to each one
// results arrive as each build completes, not in source order
outputs = crates.map((c) => compile(c));
```

This means `.map(f)` over a stream is concurrent — each application of
`f` is an independent computation. The stream is the source of work, and
the evaluator runs as many applications of `f` concurrently as there are
available elements and resources.

`.filter(pred)` applies `pred` to each element and yields those where it
returns true. Same as `.map` — no ordering guarantee, concurrent evaluation
of the predicate.

`.find(pred)` returns the first element where `pred` returns true. "First"
here means first in stream order (the order the producer yields), not
first to be evaluated. The result is a pending value that resolves when a
matching element is found.

## Poisoned values

When a stream's producer fails (e.g., a process crashes in an `exec`),
the stream terminates. Any pending value derived from that stream becomes
**poisoned**.

A poisoned value propagates its error at inspection points — the same
places where a pending value would force resolution.

If multiple inputs to a computation are poisoned, all errors are collected.
Error reporting is deterministic — the same set of errors regardless of
timing.

## The evaluator enters functions

When a computation is demanded, the evaluator enters the function body.
It has to — there's no other way to discover whether the result is ready
or pending, or what the internal dependency structure looks like.

The evaluator does not treat functions as opaque nodes. It walks into them,
discovers their dataflow graph, and schedules their internal computations
the same way it schedules top-level ones. Demand propagates all the way
down.
