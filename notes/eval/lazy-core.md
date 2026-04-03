# lazy core

Status: planning draft.

This note defines the internal lazy core used by slice 0.

It sits between:

- the semantic planning notes in
  [`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md),
  [`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md),
  and
  [`slice0-abi.md`](/Users/amos/bearcove/vixen/docs/design/runtime-abi.md)
- and the Kajit-facing lowering plan in
  [`slice0-lowering.md`](/Users/amos/bearcove/vixen/docs/design/compiler-pipeline.md)

Concrete IR sketch:
[`slice0-lazy-core-ir.md`](/Users/amos/bearcove/vixen/docs/design/lazy-core-ir.md).

Its job is simple:

- make the kernel/node split explicit before HIR emission

Without this layer, the compiler either stays too source-shaped and vague, or it
falls straight into eager HIR and loses the lazy semantics.

## Purpose

The lazy core is not:

- surface Vixen syntax
- the final runtime ABI
- Kajit HIR
- RVSDG

It is a tiny internal compiler IR for the slice-0 subset whose whole purpose is
to make these things explicit:

- which values are strict
- which values are lazy refs
- where lazy identity is allocated
- where observations are propagated
- where compiled kernels begin and end

## Design constraints

The slice-0 lazy core must preserve the decisions already made elsewhere:

1. No universal runtime `Value` in the hot path.
2. Ordinary function arguments stay lazy.
3. Product fields stay lazy until observed.
4. Sharing is real and update/reuse is explicit.
5. The runtime boundary stays narrow.

If the core cannot express those directly, it is the wrong core.

## Position in the pipeline

The intended pipeline for slice 0 is:

```text
typed Vixen subset
  -> observation planning
  -> closure conversion / environment layout
  -> node extraction
  -> lazy core
  -> Kajit HIR
  -> RVSDG
  -> CFG-MIR
  -> machine code
```

The lazy core is the first place where the compiler should stop pretending that
"an expression" is the same thing as "a strict value".

## Core entities

The lazy core should distinguish four kinds of entities:

1. strict values
2. lazy refs
3. callable values
4. observations

### Strict values

Strict values are machine-ish typed values produced after sufficient demand.

Examples:

- `u64`
- `bool`
- pointer/handle to a product shell
- pointer/handle to a closure object

Strict values are what arithmetic and branch operations consume.

### Lazy refs

`Ref<T>` is the first-class lazy edge.

The core should never blur `Ref<T>` and `T`.

Examples:

- `Ref<U64>`
- `Ref<Pair<U64, U64>>`
- `Ref<Fn(U64) -> U64>`

Core ops must make entering a ref explicit.

### Callable values

Callable values are not the same thing as arbitrary refs.

Slice 0 needs two callable forms:

- function item
- closure value `{ code_id, env_id }`

The core may model them either as a dedicated `Callable<Sig>` type or as a
distinguished subset of `Ref<FnSig>`. The important thing is that callability is
explicit and typed.

### Observations

The core should carry explicit `Obs<T>` values as defined in
[`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md).

At minimum:

- `NeedValue`
- `NeedCallable`
- `NeedProduct { ... }`

This matters because the whole point of the core is to make "enter under this
observation" explicit before Kajit lowering.

## Core program structure

The lazy core should be organized around generated entry kernels plus a small
set of helper bodies.

Recommended shape:

- one core function per top-level function item entry
- one core function per closure body entry
- one core function per extracted thunk body
- optional small typed dispatch helpers

Each core function should have:

- typed parameters
- typed locals
- explicit ref entry operations
- explicit allocation/update operations
- ordinary control flow

This is intentionally close to what Kajit HIR wants, but with slice-0-specific
lazy concepts still preserved.

## Core type families

The core does not need a huge type system.

It needs only enough to keep strict/lazy distinctions honest.

### Base strict types

- `U64`
- `Bool`
- `ProductHandle<ProductTy>`
- `ClosureHandle<Sig>`
- `EnvHandle<EnvTy>`
- `ThunkHandle<ThunkTy>`

### Lazy types

- `Ref<T>`
- `Obs<T>`
- `Callable<Sig>` if modeled separately

### Aggregate type metadata

The core may also refer to compiler-known layouts:

- product layouts
- environment layouts
- thunk frame layouts

These are compile-time identities, not runtime mega-values.

## Core operations

The core vocabulary should be small and explicit.

### Pure strict ops

- `const`
- `add_u64`
- `eq_bool`
- `select` / branch condition materialization
- local variable moves

These only consume strict values.

### Allocation ops

- `alloc_env(layout, captures...) -> EnvHandle`
- `alloc_closure(code_id, env?) -> ClosureHandle`
- `alloc_thunk(code_id, frame...) -> Ref<T>`
- `alloc_product(layout, field_refs...) -> Ref<ProductTy>`

These are the points where lazy identity becomes concrete.

### Observation ops

- `enter(ref, obs) -> observed`
- `join_obs(a, b) -> obs`
- `subobs_product(obs, field) -> obs_field`

`enter` is the centerpiece of the core.

### Product ops

- `project_ref(product_ref, field) -> Ref<FieldTy>`
- optional `project_handle(product_handle, field) -> Ref<FieldTy>`

The important property is:

- projection selects a field ref
- it does not implicitly enter sibling fields

### Callable ops

- `need_callable(ref_or_callable) -> Callable<Sig>`
- `call(callable, args_as_refs..., obs_result, self_thunk?)`

The call operation must preserve lazy arguments.

### Update ops

- `thunk_state(thunk)`
- `mark_evaluating(thunk)`
- `cache_ready_scalar(thunk, value)`
- `cache_ready_product(thunk, product_handle)`
- optional `redirect_updated(thunk, ref)`

Whether these stay as explicit core ops or lower quickly into runtime helpers is
an implementation choice. The important thing is that update/reuse is explicit
before Kajit lowering.

### Control ops

- `if strict_bool then ... else ...`
- `trap`
- `return`

Slice 0 does not need loops or suspension.

## Core function signatures

The core should make entrypoint shape explicit.

### Function item entry

Conceptually:

```text
enter_fn_f(
  ctx,
  self_thunk,
  arg0: Ref<A0>,
  arg1: Ref<A1>,
  ...,
  obs: Obs<R>,
)
```

### Closure entry

Conceptually:

```text
enter_closure_f(
  ctx,
  self_thunk,
  env: EnvHandle<EnvTy>,
  arg0: Ref<A0>,
  ...,
  obs: Obs<R>,
)
```

### Extracted thunk entry

Conceptually:

```text
enter_thunk_k(
  ctx,
  self_thunk,
  obs: Obs<T>,
)
```

In this case, the thunk frame already contains the stored ref inputs.

## Normal forms the core should enforce

The point of the core is not just to have more IR. It is to make certain bad
states impossible.

The core should enforce these normal forms:

1. Arithmetic only consumes strict scalars, never raw refs.
2. Branches only consume strict bools, never raw refs.
3. Product construction stores field refs, not forced field values by default.
4. Calls receive argument refs, not strict argument values.
5. Every lazy identity allocation is explicit.
6. Every ref entry is explicit.
7. Sharing/update is explicit at the thunk level.

If any lowering step wants to violate one of these, it should need an explicit
transformation rule.

## Example lowering into the core

### Program A

From [`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md#L299):

```vx
fn bomb() -> U64 = trap("bomb")

fn main() -> U64 {
  let pair = (41 + 1, bomb());
  pair.0
}
```

Core shape:

1. compute strict `42`
2. wrap `42` as ready `Ref<U64>`
3. allocate thunk ref for `bomb`
4. allocate product ref for `(field0, field1)`
5. project field `0`
6. enter field `0` under `NeedValue`

Notably absent:

- any enter on field `1`

### Program C

From [`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md#L332):

```vx
fn plus_one(n: U64) -> U64 {
  n + 1
}

fn main(n: U64) -> U64 {
  let x = plus_one(n);
  x + x
}
```

Core shape:

1. allocate one thunk ref `x`
2. enter `x` under `NeedValue`
3. enter `x` under `NeedValue` again
4. second enter observes ready/update state and reuses cached result
5. strict `add_u64` consumes the two strict results

The point is that reuse is visible directly in the core, not hidden behind
source-level substitution.

## How the core should lower to Kajit HIR

The lazy core should lower almost mechanically into Kajit HIR:

- core locals -> HIR locals
- core strict ops -> HIR expressions/statements
- core control ops -> HIR `if` / returns
- core allocation/update/enter ops -> typed runtime/helper callables
- core product/env/thunk layouts -> HIR semantic struct types

This is why the core should stay small.
If the core is compact and explicit, HIR lowering becomes straightforward.

Kajit HIR should not need to rediscover:

- where laziness exists
- where sharing exists
- where a thunk must be entered

Those decisions belong in the lazy core already.

## Relationship to the oracle

The oracle in
[`slice0-oracle.md`](/Users/amos/bearcove/vixen/docs/design/reference-harness.md)
should validate the same semantic model as the lazy core.

The easiest long-term shape is:

- oracle interprets programs directly against the slice-0 semantics
- compiler lowers the same programs through the lazy core
- traces/results are compared at the semantic event level

That way the lazy core is not just a codegen device.
It is also the bridge between semantic planning and executable validation.

## What the lazy core should not contain

To keep slice 0 honest, the core should not contain:

- host/resource operations
- keyed/shared solver operations
- suspension/resume
- parallel ownership/publication
- generic dynamic eval ops
- a mega-op that means "ask runtime to evaluate this expression"

If such an op appears, slice 0 has already drifted away from its purpose.

## Open questions

The lazy core still leaves some implementation questions open:

- whether `Callable<Sig>` is separate from `Ref<FnSig>` or just a validated form
- whether ready scalar refs are explicit core constructors or an optimization
- how much observation specialization is encoded in the core vs deferred to
  helper generation
- whether product shells are always allocated or can sometimes be stack-shaped
  inside one kernel before escaping

Those are legitimate open questions.

What should not remain open after this note is the compiler boundary:

- the source language is too high-level
- Kajit HIR is too eager/direct by default
- the lazy core is the explicit middle layer that carries slice-0 laziness
