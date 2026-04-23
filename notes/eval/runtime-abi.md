# runtime ABI

Status: planning draft.

This note is the ABI/object-layout companion to
[`canonical-examples.md`](canonical-examples.md).

Its job is not to settle every future evaluator representation.
Its job is to make the first small slice concrete enough that "typed lazy refs"
stops being a vibe and becomes a machine shape.

## Scope

This note only covers the first small slice:

- `U64`
- `Bool`
- tuples and simple structs
- ordinary functions
- closures with captured environments
- synchronous local thunk entry/update

It does not cover:

- `Tree`, `Blob`, `Path`, `discover`, `exec`, or observers
- keyed/shared solver nodes
- suspension/resume
- arrays, maps, or enums

## Hard constraints

The first slice should obey these constraints:

1. No universal runtime `Value` in the hot path.
2. Function values are not definition blobs.
3. Captures exist only when captures exist.
4. Ordinary function arguments remain lazy.
5. Local sharing has update/reuse semantics.
6. There is no interpreter loop hidden inside node entry.

If a proposed representation breaks one of those, it is outside the slice.

## ABI families

The ABI should distinguish three families of runtime things:

- strict values
- lazy refs
- callable values

They are related, but they are not the same representation.

## Strict values

Strict values remain typed machine-level values.

Examples:

- `u64`
- `bool`
- typed pointer/handle to a product object
- typed pointer/handle to a closure object

The important point is that strict execution does not pass through a dynamic
boxed value representation.

## `Ref<T>`

`Ref<T>` is the core lazy edge type.

Conceptually, `Ref<T>` means:

- a thing that can satisfy observations of `T`

It does not mean:

- a boxed dynamic `Value`

For the first slice, `Ref<T>` only needs to cover:

1. already-ready scalar
2. pointer to a thunk producing `T`
3. pointer to a product object of type `T`
4. pointer to a closure object when `T` is callable

The exact bit-level encoding is still open.

The key invariant is:

- entering a `Ref<U64>` is a typed operation
- entering a `Ref<Pair<U64, U64>>` is a typed operation
- there is no generic `enter(Value)` fast path

## Observations at the ABI boundary

The ABI should use the observation model from [`observations.md`](observations.md).

For the first slice:

- `Obs<U64> = NeedValue`
- `Obs<Bool> = NeedValue`

For products, the practical rule can be:

- demand enough to obtain the product shell
- then observe selected field refs individually

That preserves the important laziness:

- constructing the shell does not force sibling fields
- observing one field does not enter the others

## Callable representation

Callable values should come in two forms:

1. function item
2. closure value

Non-capturing functions do not need a closure environment.

Capturing functions need:

- code identity
- environment identity

Conceptually:

```text
ClosureObj<Sig> {
  header,
  code: CodeId,
  env: EnvObjId,
}
```

Captured values live in a separate environment object.

Conceptually:

```text
EnvObj<Layout> {
  header,
  captures: [Ref<CaptureTy>; N],
}
```

Important points:

- captures are stored as refs, not eagerly forced values
- each lambda gets a fixed environment layout
- a closure without captures should not allocate an environment object

## Function entry rule

The ABI should commit to one semantic rule:

- ordinary function parameters are passed as lazy refs

If `fn f(x: U64) -> U64` is called, the body of `f` receives `x: Ref<U64>`,
not `x: u64`.

That is essential. If ordinary parameters arrive already strict, the evaluator
has already broken demand-driven semantics.

## Runtime services

Compiled code in the first slice should only need a small runtime surface:

- allocate closure environment objects
- allocate closure objects
- allocate thunk objects
- allocate product objects
- enter/update typed refs
- trap/fail

There is no scheduler, keyed table, async wakeup protocol, or owner/worker
handshake in this slice.

## Success criteria

The ABI direction is good if it gives us:

- typed hot-path values
- explicit lazy refs
- explicit callable and environment identity
- update/reuse semantics for local sharing
- one runtime model that a JIT backend can target without semantic shortcuts
