# eval3 slice 0 ABI

Status: planning draft.

This note is the ABI/object-layout companion to
[`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md).

Its job is not to settle every future `eval3` representation.
Its job is to make slice 0 concrete enough that "typed lazy refs" stops being a
vibe and becomes a machine shape.

## Scope

This note only covers slice 0:

- `U64`
- `Bool`
- tuples and simple structs
- ordinary functions
- closures with captured environment
- synchronous local thunk entry/update

It does not cover:

- `Tree`, `Blob`, `Path`, `exec`, `discover`, or observers
- keyed/shared solver nodes
- joined inputs
- suspension/resume
- worker publication
- general arrays, maps, or enums

## Hard constraints

Slice 0 should obey these constraints:

1. No universal runtime `Value` in the hot path.
2. Function values are not definition blobs.
3. Captures exist only when captures exist.
4. Ordinary function arguments remain lazy.
5. Local sharing has update/reuse semantics.
6. There is no interpreter loop hidden inside node entry.

If any proposed representation breaks one of those, it is outside slice 0.

## Runtime services available in slice 0

Compiled code in slice 0 should only need a tiny runtime surface:

- allocate closure environment objects
- allocate closure objects
- allocate thunk objects
- allocate product objects
- trap/fail
- optional debug hooks / counters

There is no scheduler, no keyed table, no async wakeup protocol, and no owner /
worker handshake in this slice.

## The ABI families

Slice 0 should distinguish three families of runtime things:

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

`Ref<T>` is the core lazy edge type for slice 0.

Conceptually, `Ref<T>` means:

- a thing that can satisfy observations of `T`

It does not mean:

- a boxed dynamic `Value`

### Semantic forms of `Ref<T>`

For slice 0, `Ref<T>` only needs to cover these semantic cases:

1. already-ready scalar
2. pointer to a thunk producing `T`
3. pointer to a product object of type `T`
4. pointer to a closure object when `T` is a callable type

The exact bit-level encoding is still open.

The likely practical direction is:

- inline/immediate for already-ready small scalars
- pointer/handle for heap-backed objects

The key invariant is:

- entering a `Ref<U64>` is a typed operation
- entering a `Ref<Pair<U64, U64>>` is a typed operation
- there is no generic `enter(Value)` fast path

### Observing a `Ref<T>`

Node entry is synchronous in slice 0.

Given:

- `Ref<T>`
- `Obs<T>`

the machine either:

- satisfies the observation immediately
- or enters compiled code attached to the ref's backing object
- possibly updates that object
- and returns the demanded typed result fragment

No async protocol is involved.

## Observations at the ABI boundary

Slice 0 should use the observation model from
[`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L106), but with one
important practical normalization.

### Scalars

For `U64` and `Bool`:

- `Obs<U64> = NeedValue`
- `Obs<Bool> = NeedValue`

### Products

At the semantic level, products can be discussed as field observations.

At the ABI level, slice 0 can normalize that into:

- demand enough to obtain the product shell
- then observe the selected field refs individually

So in practice:

- a thunk returning a tuple/struct may satisfy demand by producing a product
  object whose fields are themselves `Ref<FieldTy>`
- later field access enters only the selected field refs

This is simpler than a full per-field partial-product cache while still
preserving the important laziness:

- constructing the shell does not force sibling fields
- observing one field does not enter the others

## Callable representation

Callable values in slice 0 should come in two forms:

1. function item
2. closure value

### Function item

Non-capturing functions do not need a closure environment.

Conceptually:

- `FnItem<Sig> = CodeId`

This can be represented as a tagged immediate or some other thin callable
encoding.

### Closure value

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

That is all the closure value itself should carry.

No parameter metadata, source names, bytecode metadata, or activation state
belongs here.

### Environment object

Captured values live in a separate environment object.

Conceptually:

```text
EnvObj<Layout> {
  header,
  captures: [Ref<CaptureTy>; N],
}
```

Important points:

- captures are stored as typed refs, not eagerly forced values
- each lambda gets a fixed environment layout
- a closure without captures should not allocate an environment object

## Function entry ABI

Slice 0 should commit to one semantic rule:

- ordinary function parameters are passed as lazy refs

If `fn f(x: U64) -> U64` is called, the body of `f` receives:

- `x: Ref<U64>`

not:

- `x: u64`

The body may enter `x` under `NeedValue` if its logic demands that.

This is essential. If ordinary parameters arrive already strict, slice 0 has
already broken laziness.

### Suggested entry shape

Conceptually, a compiled function/closure entry should look like:

```text
enter_f(
  ctx,
  self_thunk,
  env,
  arg0: Ref<A0>,
  arg1: Ref<A1>,
  ...,
  obs: Obs<R>,
) -> void
```

Where:

- `ctx` is the tiny slice-0 runtime context
- `self_thunk` is the destination thunk being satisfied/updated
- `env` is null or an environment pointer
- args are typed refs
- `obs` is the requested observation of the result

The important design choices are:

- entry code is compiled code, not interpreter bytecode
- arguments are lazy refs
- result satisfaction happens by updating the destination thunk

Slice 0 does not need suspension, so entry code does not need resumable frame
objects.

## Thunk layout

Thunk objects are the runtime identity for lazy local computations.

Conceptually:

```text
ThunkObj<T, FrameLayout> {
  header,
  code: CodeId,
  state: ThunkState,
  obs_state: ObsState<T>,
  frame: FrameLayout,
  cache: ResultCache<T>,
}
```

Where:

- `FrameLayout` is generated per thunk/code shape
- `frame` stores the captured argument refs and any lifted environment refs
- `cache` stores the best result currently justified for `T`

### Thunk state

Slice 0 only needs:

- `Unevaluated`
- `Evaluating`
- `Ready`
- optional `Updated`

`Evaluating` is needed to catch accidental self-reentry or recursive forcing.

### Why frame layout is code-specific

Slice 0 has no suspension.

So the thunk does not need to store a general activation stack.
It only needs to store whatever lazy inputs are necessary to re-enter the code:

- argument refs
- captured env refs

Locals that exist only during one synchronous enter live on the native stack.

That is important because it keeps slice 0 from smuggling eval2-style activation
blobs into every runtime object.

## Result cache shape

Slice 0 should keep the result cache typed.

### Scalar results

For scalar `T`, the cache can hold:

- the satisfied scalar value

Example:

```text
ResultCache<U64> = u64
```

### Product results

For tuple/struct `T`, the cache can hold:

- a pointer/handle to a product object of type `T`

The product object contains field refs, not eagerly realized field values.

This means:

- returning a pair does not imply entering both fields
- a function returning a struct can be "ready" once the shell exists
- later field observations still enter the selected field refs lazily

## Product object layout

Tuple and struct results should use typed product objects.

Conceptually:

```text
ProductObj<Layout> {
  header,
  fields: [Ref<FieldTy>; N],
}
```

Important points:

- fields are stored as refs
- field access is just selecting the appropriate typed ref
- sibling fields remain untouched until observed

This is the slice-0 substitute for a more sophisticated future structured
observation cache.

## Update / reuse rule

Slice 0 must have real local sharing semantics.

That means:

1. a shared thunk may be entered under demand
2. the thunk caches the justified result
3. later demands reuse that cached result
4. the original body is not blindly rerun

An implementation may realize this either by:

- keeping the thunk and marking it `Ready`
- rewriting it to an explicit indirection/updated object

Both are fine for slice 0.

The semantic rule is the important part:

- local identity survives long enough to make sharing real

## Worked mapping to the canonical programs

### Program A: lazy field selection

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L299):

- `pair` is a product object with two field refs
- field `0` may be an immediate ready scalar ref or a tiny thunk
- field `1` is a thunk for `bomb()`
- reading `pair.0` selects field `0` and never enters field `1`

### Program B: closure capture

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L316):

- `add_n` is a closure object
- its environment stores `n` as `Ref<U64>`
- `add_n(1)` and `add_n(2)` allocate result thunks for the two call results
- observing `pair.1` only enters the second call result

### Program C: local sharing

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L332):

- `x` is one thunk for `plus_one(n)`
- the first use of `x` enters that thunk and updates it
- the second use of `x` reuses the updated result

### Program D: higher-order local helper

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L350):

- `apply` receives `f` as a typed callable value
- `x` arrives as `Ref<U64>`
- calling `f(x)` stays in compiled code except for normal lazy thunk entry

## What is still open after this note

This note should settle the slice-0 ABI direction, but not every detail.

Still open:

- exact bit-level encoding of `Ref<T>`
- exact object header format
- whether function items and closures share one tagged callable representation
- whether `Updated` is a distinct object or a thunk state
- how much debug type/layout info lives in headers vs side tables

Those are implementation choices.

What should no longer be open after this note:

- whether slice 0 uses a universal runtime `Value`
- whether ordinary function parameters are lazy
- whether closures carry giant definition blobs
- whether products store refs or forced values
- whether sharing is semantically real
