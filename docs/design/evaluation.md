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
- String interpolation of a scalar: `` `${x}` ``
- Arithmetic: `a + b`
- Passing to `exec` args (the data must be available to the executor)
- Materializing the final output tree to disk

Constructing a struct, accessing a field, calling a function, returning a
value — these are NOT inspection points. They do not force anything.

## Where pending values come from

The only source of pending values is `exec`. An `exec` returns a
`Stream[T]`. Operations on that stream (like `find`) produce pending
values — handles to data the executor hasn't produced yet.

Everything else in the language produces resolved values immediately
(assuming its inputs are resolved). `compile_lib(src)` where `src` is
resolved does not produce a pending value — unless its body contains an
`exec`, in which case the pending-ness flows out through the return value.

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

When a process in an `exec` fails unexpectedly, the stream terminates.
Any pending value that was waiting on that stream becomes **poisoned**.
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
