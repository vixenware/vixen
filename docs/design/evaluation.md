# Vixen evaluation model

## Demand-driven dataflow

Vixen uses demand-driven dataflow evaluation.

The runtime calls `build`. It needs the return value. That's the demand.
Everything flows backward from there.

```vixen
fn build(src: Tree, args: Array[String], env: Map[String, String]) -> Tree {
  a = compile_crate_a(src);
  b = compile_crate_b(src);
  output = link(a, b);
  Tree.of({ "target": output })
}
```

To produce the return value, `Tree.of` needs `output`. To produce `output`,
the evaluator enters `link` — it discovers `link` needs `a` and `b`. To
produce `a`, it enters `compile_crate_a`, discovers an `exec` inside, and
gets a pending value back. Same for `b` via `compile_crate_b`. Those execs
run concurrently. Inside `link`, the evaluator discovers another `exec`
that needs the resolved values of `a` and `b` — that exec is the node
boundary that forces them. When both resolve, that exec runs, its result
flows through `Tree.of`, and `build` returns.

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

## Forcing

Almost nothing in the language forces a pending value. Arithmetic,
comparison, pattern matching, field access, function calls, string
interpolation — all of these can operate on pending values. They produce
new pending values that depend on their inputs. They are just more nodes
in the dataflow graph.

Two things force:

1. **Crossing the node boundary.** To serialize a value and ship it to
   another machine, the data must be resolved. This is the primary
   forcing point — and it's the reason the language was designed around
   value semantics, universal hashability, and serializability in the
   first place. Everything about the language exists so that values can
   cross this boundary.

2. **Materializing the final output.** At the very end, the runtime
   writes the output tree to disk. The data must be resolved to write
   bytes.

Everything else just builds up the graph.

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
outputs = crates.map(compile($));
```

This means `.map(f)` over a stream is concurrent — each application of
`f` is an independent computation. The stream is the source of work, and
the evaluator runs as many applications of `f` concurrently as there are
available elements and resources.

`.filter(pred)` applies `pred` to each element and yields those where it
returns true. Same as `.map` — no ordering guarantee, concurrent evaluation
of the predicate.

`.find(pred)` returns whichever matching element resolves first. If
multiple elements match, the result is non-deterministic — use `.find()`
when you expect exactly one match. The result is a pending value that
resolves when any matching element is found.

## Poisoned values

A node becomes poisoned in two ways:

- **`fail`** — the code explicitly poisons: `fail "message"`.
- **Stream termination** — a stream's producer dies (e.g., a process
  crashes). Pending values derived from that stream become poisoned.

These are the same mechanism. A poisoned value propagates its error
through the graph — any node that depends on a poisoned value also
becomes poisoned. The runtime attaches context automatically: what
function was being evaluated, what its inputs were, what demanded it.

If multiple inputs to a computation are poisoned, all errors are collected.
Error reporting is deterministic — the same set of errors regardless of
timing. Poisoning is not catchable.

## The evaluator enters functions

When a computation is demanded, the evaluator enters the function body.
It has to — there's no other way to discover whether the result is ready
or pending, or what the internal dependency structure looks like.

The evaluator does not treat functions as opaque nodes. It walks into them,
discovers their dataflow graph, and schedules their internal computations
the same way it schedules top-level ones. Demand propagates all the way
down.

## Memoization

When the evaluator demands a function call, it first checks whether that
call has already been computed with the same inputs. If so, it returns the
cached result without re-entering the function. The cache key is
**(function, arguments)**. Because all values are hashable and comparable,
any function call can be memoized transparently.

This is what makes incremental builds work — on a rebuild, most calls have
the same inputs as before and their results are reused.

Functions with `@ttl` annotations are cached provisionally — the runtime
revalidates them according to their policy. If the revalidated result is
the same as the cached result, downstream nodes are not re-evaluated
(early cutoff).
