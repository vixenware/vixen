# Vixen evaluation model

Fundamental truths about how Vixen evaluates programs.

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

A pending value must resolve when the actual data is needed. This happens
at **inspection points**:

- Comparing for equality: `a == b`
- Pattern matching: `match x { ... }`
- Arithmetic: `a + b`
- Materializing the final output tree to disk

Constructing a struct, accessing a field, calling a function, returning a
value, string interpolation — these are NOT inspection points. They do not
force anything. (String interpolation produces a structured value that
preserves provenance, not a plain string — so it doesn't need to resolve
its parts.)

## Where pending values come from

Pending values come from **streams**. Any function that returns
`Stream[T]` and uses `yield` produces values over time. Operations on
a stream (like `find`) return pending values — handles to data that
hasn't been produced yet.

`exec` is one way to get a stream, but it's not special — it's a built-in
that happens to return `Stream[T]`. User code can create streams with
`yield` in any function that returns `Stream[T]`.

## The dataflow graph

Every binding in a function body is a node. The edges are data
dependencies — which bindings each node references.

A node can be evaluated when its direct inputs are available. "Available"
means the binding exists and has been assigned — NOT that every value
reachable from it is fully resolved. A struct with pending fields is
available. An array with pending elements is available.

Nodes with no dependencies between them can be evaluated concurrently.
The runtime schedules them based on the dependency graph.

## Poisoned values

When a stream's producer fails (e.g., a process crashes in an `exec`),
the stream terminates. Any pending value derived from that stream becomes
**poisoned**.
A poisoned value propagates its error at inspection points — the same
places where a pending value would force resolution.

If multiple inputs to a node are poisoned, the node collects all errors.
Error reporting is deterministic — the same set of errors regardless of
timing.

## The yield boundary

Inside an observer, `commit(path)` returns a handle (a `Blob`). The
handle is lazy. The actual data movement to the content-addressable store
happens when the observer yields a value containing the handle back to
the orchestrator. This is the **materialization boundary** — the point
where store handles must resolve so the executor can be released.
