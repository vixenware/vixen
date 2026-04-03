# canonical examples

Status: planning draft.

This note turns "slice 0" from
[`execution-model.md`](/Users/amos/bearcove/vixen/docs/design/execution-model.md) into a concrete
proof target.

Companion note:
[`slice0-abi.md`](/Users/amos/bearcove/vixen/docs/design/runtime-abi.md),
[`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md),
[`slice0-lowering.md`](/Users/amos/bearcove/vixen/docs/design/compiler-pipeline.md),
[`slice0-lazy-core.md`](/Users/amos/bearcove/vixen/docs/design/lazy-core.md),
[`slice0-oracle.md`](/Users/amos/bearcove/vixen/docs/design/reference-harness.md).

Slice 0 is intentionally below current zoo build/solver semantics.

That is not because those semantics do not matter.
It is because slice 0 exists to prove the execution model in the smallest
possible setting before `Tree`, `exec`, `discover`, `keyed`, joined inputs, and
branch-local search are added on top.

If slice 0 cannot be made precise, the later slices will be hand-waving.

## Goal

Slice 0 should prove one thing:

> lazy, typed Vixen semantics can reduce mostly through compiled native kernels
> without collapsing into a universal runtime `Value` interpreter.

More concretely, slice 0 should establish:

- lazy construction stays lazy
- typed structural demand is real
- local demand propagation can stay inside compiled code
- closure capture does not require a giant boxed function value
- local sharing updates and reuses results instead of recomputing blindly

## Non-goals

Slice 0 does not try to prove:

- host/resource semantics like `Tree`, `Blob`, `Path`, `exec`, `discover`, or
  observers
- solver semantics like `keyed`, joined inputs, `Conflict`, branch assumptions,
  or search frontier publication
- suspension/resume
- parallel publication or stealing
- open-ended container demand such as arbitrary array indexing

Those belong to slice 1 and slice 2.

## Exact semantic subset

Slice 0 should freeze a tiny authored subset instead of speaking in examples.

### Types

Included:

- `U64`
- `Bool`
- tuples of slice-0 types
- simple structs with named fields of slice-0 types
- first-class closures and function values

Excluded:

- arrays
- maps
- enums / `match`
- traits / methods / namespaces
- host-backed resource types
- keyed or join-specific value types

### Expressions

Included:

- literals
- local `let`
- `if`
- tuple literals
- struct literals
- tuple/struct projection
- ordinary function call
- closure literals with captured environment
- local helper calls expressed through ordinary functions/closures

Excluded:

- recursion
- loops
- partial application syntax as a surface feature
- effectful primitives
- implicit runtime services

### Combinator surface

Slice 0 should still include one higher-order shape, because closures are not an
optional add-on in Vixen.

The recommended limit is:

- higher-order helper functions written in the slice itself
- closures passed as ordinary values
- no prelude-scale collection library yet

That is enough to prove that local higher-order code can stay compiled and typed
without dragging in full build-style collection semantics.

## First observation lattice

Slice 0 needs an explicit observation model.

The point is not to solve the final lattice. The point is to stop speaking
vaguely about "forcing".

### Scalars

For `U64` and `Bool`, the only slice-0 observation is:

- `NeedValue`

There is no weaker scalar observation in this slice.

### Tuples and structs

For tuples and structs, observation is structural.

Suggested shape:

- `NeedFields(mask)`

Where `mask` is a finite field set known from the type layout.

Examples:

- observing `pair.0` creates `NeedFields({0})`
- observing `pair.0 + 1` still only demands `{0}` from `pair`
- observing the whole pair creates `NeedFields({0, 1})`

For named structs, the same idea applies by field id instead of tuple index.

### Functions and closures

Functions are not structurally observed in slice 0.

They are only:

- passed around
- called

So the relevant notion is:

- a call node receives an observation on its result type

Calling `f(x)` does not mean "eagerly evaluate `f`".
It means "construct a lazy result of the callee's return type, to be entered
under some later observation".

### What this buys

This is enough to state the core lazy behavior precisely:

- building a tuple does not imply observing every field
- demanding one field should not enter the others
- demanding a call result does not necessarily imply demanding a full
  higher-level container returned by the call

## Minimal runtime entity set

Slice 0 should have a tiny set of runtime entities.

It does not need global solver nodes yet.

### 1. Closure object

Needed when a function value captures locals.

Conceptually:

- `code_id`
- `env_id`

No definition metadata, bytecode metadata, or activation state rides inside the
closure value itself.

### 2. Thunk node

Represents a lazy local computation with a typed result.

Conceptually:

- node header
- entry code pointer / code id
- environment / captured refs
- current state
- memoized observation state

State is at least:

- `Unevaluated`
- `Evaluating`
- `Ready`
- `Updated` / indirection

### 3. Product object

Represents a tuple or struct shell whose fields are themselves refs.

This matters because:

- tuple/struct construction is lazy
- field demand should be able to enter only the demanded field refs

The product shell is not a giant universal value.
It is a typed runtime object with fixed field layout.

### 4. Optional explicit indirection

If the update story is cleaner with a distinct indirection/updated object,
include it in slice 0.

If not, fold it into thunk state.

The important thing is semantic:

- once a local shared thunk has been entered and satisfied, later users should
  reuse the result rather than recompute the original body

## Typed ABI sketch

Slice 0 needs a small ABI story even if the exact representation changes later.

### Strict values

Strict values stay typed:

- `u64`
- `bool`
- typed pointers/handles to closure and product objects

Slice 0 should not route ordinary compiled execution through a universal Rust
`Value` enum.

### `Ref<T>`

Every lazy edge in slice 0 is represented as `Ref<T>`.

The representation can still evolve, but the slice should commit to the model:

- `Ref<T>` is not "erased any-value"
- `Ref<T>` points to or embeds something that will satisfy observations of `T`
- entering a `Ref<T>` is a typed operation

The most likely practical representation is:

- immediate inline form for small already-ready scalars when possible
- pointer/handle form for heap-backed products, closures, and thunks

Whether `Ref<T>` ends up being one word or two words is less important than the
semantic rule that it stays typed and non-erased.

### Node entry

Slice 0 can stay synchronous.

So node entry can be modeled as:

- given `Ref<T>` and `Obs<T>`
- either satisfy the observation immediately
- or enter compiled node code
- possibly update the node
- return the demanded typed result fragment

No scheduler, no async protocol, no owner/worker round-trip.

## Kernel extraction rule

Slice 0 needs one concrete rule for when the compiler keeps code inside a kernel
vs allocating a thunk/product object.

Recommended planning rule:

- if a cone is local, non-escaping, and demanded in one known observation
  context, compile it straight-line inside the current kernel
- allocate a thunk when a computation is shared, captured, returned as a lazy
  subresult, or otherwise needs independent identity
- allocate a product object when a tuple/struct escapes as a lazy structured
  value whose fields may be observed independently

This rule is deliberately local and conservative.

The point of slice 0 is not to invent a perfect global optimizer.
The point is to prove the machine can distinguish:

- local demanded work
- lazy reusable identity

## Canonical slice-0 programs

Slice 0 should be planned against named programs, not vibes.

### Program A: lazy field selection

```vx
fn bomb() -> U64 = trap("bomb")

fn main() -> U64 {
  let pair = (41 + 1, bomb());
  pair.0
}
```

What this proves:

- tuple construction is lazy
- observing field `0` does not enter field `1`
- local arithmetic can stay compiled

### Program B: closure capture

```vx
fn main(n: U64) -> U64 {
  let add_n = (x) => x + n;
  let pair = (add_n(1), add_n(2));
  pair.1
}
```

What this proves:

- closure environments are real
- closure values are not giant boxed definition blobs
- unused lazy siblings stay unentered

### Program C: local sharing

```vx
fn plus_one(n: U64) -> U64 {
  n + 1
}

fn main(n: U64) -> U64 {
  let x = plus_one(n);
  x + x
}
```

What this proves:

- a shared local thunk can be entered once and updated
- the second use sees the updated result instead of blindly re-running the body

### Program D: higher-order local helper

```vx
fn apply(f: Fn(U64) -> U64, x: U64) -> U64 {
  f(x)
}

fn main(n: U64) -> U64 {
  let add_n = (x) => x + n;
  apply(add_n, 41)
}
```

What this proves:

- higher-order local code works in the typed lazy model
- calling through a closure value still stays in compiled code except at normal
  lazy node entry points

## Success criteria

Slice 0 is successful if all of the following are true:

1. Program A never enters `bomb()`.
2. Program B proves closure capture without a mega-function-value object.
3. Program C computes `plus_one(n)` once under sharing, not twice.
4. Program D proves that higher-order local code does not collapse into erased
   runtime evaluation.
5. The compiled hot path is:
   - typed values
   - typed refs
   - compiled node entry/update
   - no generic eval interpreter loop
6. The implementation can explain, for every node allocation in the slice, why
   that node exists semantically.

## Deliverables for slice-0 planning

Planning is complete when these artifacts exist:

1. This note, with the subset frozen.
2. A tiny observation-lattice note for the included types.
3. A tiny ABI/object-layout note for `Ref<T>`, closures, products, and thunks.
4. A tiny reference runner or oracle model for the four canonical programs.
5. A lowering sketch from typed Vixen subset into Kajit-facing IR/kernels.

If any of those are missing, slice 0 is still underplanned.

## What stays deferred

These belong to later slices and should not leak into slice 0 planning:

- `Tree`, `Blob`, `Path`, `TreeEntry`
- `exec`
- `discover`
- observer callbacks
- `keyed(...)`
- joined monotone inputs
- structured `Conflict`
- branch-local assumptions
- pending/resume
- parallel publication / stealing

That is not because they are unimportant.
It is because slice 0 should prove the execution model before those larger
semantic surfaces are added.
