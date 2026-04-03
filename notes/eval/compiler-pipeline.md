# compiler pipeline

Status: planning draft.

This note is the lowering-plan companion to
[`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md),
[`slice0-abi.md`](/Users/amos/bearcove/vixen/docs/design/runtime-abi.md), and
[`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md),
[`slice0-lazy-core.md`](/Users/amos/bearcove/vixen/docs/design/lazy-core.md).

Its purpose is:

- explain how slice-0 Vixen should lower into Kajit-facing compiled kernels

This is where "typed lazy refs" becomes an actual compiler pipeline instead of a
semantic wish.

## Goal

Slice 0 lowering should prove that the following can coexist:

- lazy Vixen semantics
- typed observations
- explicit lazy identity where needed
- native compiled local execution through Kajit

The lowering must not secretly collapse into:

- a generic runtime evaluator
- a universal `Value` representation
- eager argument forcing

## Relationship to Kajit

Kajit already has the right broad architecture for this:

- HIR as the semantic/source-facing layer, per
  [`docs/hir-design.md`](/Users/amos/bearcove/kajit/docs/hir-design.md)
- RVSDG as the optimization layer
- CFG-MIR and machine code as the backend path

Kajit also already has a prototype Vixen-typed lowering path in
[`kajit-hir/src/lib.rs`](/Users/amos/bearcove/kajit/kajit-hir/src/lib.rs),
but that prototype is still fundamentally eager/direct:

- expressions lower as direct values
- call arguments are direct expression results
- there is no explicit `Ref<T>`
- there is no explicit `Obs<T>`
- there is no node extraction/update model

Slice 0 should reuse that direction where possible, but it needs a more explicit
lazy execution boundary.

## Lowering pipeline

For slice 0, the intended pipeline should be:

```text
typed Vixen subset
  -> demand planning
  -> closure conversion / environment layout
  -> node extraction
  -> slice-0 lazy core
  -> Kajit HIR
  -> RVSDG
  -> CFG-MIR
  -> machine code
```

The important addition relative to a direct eager lowering is:

- demand planning and node extraction happen before Kajit HIR emission

Kajit should compile the resulting kernels.
It should not be asked to infer laziness from already-eager lowered code.

## Slice-0 lazy core

Before lowering to Kajit HIR, slice 0 should pass through a tiny internal core
language whose terms already make laziness explicit.

This core does not need to be a public user-facing IR.
It exists to make the kernel/node split concrete.

### Core values

The core should distinguish:

- strict typed values
- `Ref<T>`
- `Obs<T>`
- callable values

### Core operations

The core only needs a small vocabulary:

- make literal
- make function item
- make closure `{ code_id, env }`
- allocate thunk
- allocate product
- select product field ref
- enter ref under observation
- update thunk result
- branch on strict bool
- direct arithmetic on strict scalars
- direct call to compiled entry kernel

This core is already enough to express the slice-0 canonical programs.

## Why demand planning comes first

If we lower ordinary Vixen expressions straight into direct HIR expressions, we
have already lost the semantics.

Example:

```vx
let pair = (41 + 1, bomb());
pair.0
```

An eager lowering would emit both tuple fields as direct computations.
That is wrong.

The lowering must first determine:

- the demanded observation on `pair` is only field `0`
- field `1` stays latent

So the pipeline needs a planning step that decides:

- which cones stay straight-line inside one kernel
- which subexpressions become independently enterable lazy nodes

Only then should Kajit HIR be emitted.

## Planning output

Demand planning for slice 0 should produce four things:

1. An observation on each expression use site.
2. A set of expressions extracted as nodes.
3. A set of kernel entrypoints to generate.
4. Concrete environment/frame layouts for closures and thunks.

This is the planning result that drives the rest of lowering.

## Node extraction rule

For slice 0, the extraction rule should stay conservative and local.

Extract a runtime node when an expression:

- is shared across multiple uses
- escapes as a lazy subresult
- becomes a captured closure environment value
- is returned as a field of a lazy product
- otherwise needs stable lazy identity

Keep an expression inside the current kernel when it is:

- local
- demanded under one known observation
- non-escaping
- not shared independently

This is not the final optimizer.
It is just enough to separate:

- local strict work
- lazy reusable identity

## Generated artifact kinds

Slice 0 lowering should generate a small number of compiled artifact kinds.

### 1. Function-item entry kernels

For top-level/non-capturing functions.

Conceptually:

```text
enter_fn_f(ctx, self_thunk, arg0: Ref<A0>, ..., obs: Obs<R>)
```

### 2. Closure entry kernels

For capturing functions.

Conceptually:

```text
enter_closure_f(ctx, self_thunk, env: EnvObj*, arg0: Ref<A0>, ..., obs: Obs<R>)
```

The code body may be shared with the non-capturing form when possible.

### 3. Thunk entry kernels

For extracted lazy computations.

Conceptually:

```text
enter_thunk_codeid(ctx, self_thunk, obs)
```

Where the thunk frame contains the stored refs needed for re-entry.

### 4. Optional tiny dispatch shims

Some operations are inherently dynamic even in slice 0:

- enter an arbitrary `Ref<T>`
- call an arbitrary callable value

At first, these may be implemented as tiny typed dispatch shims:

- `enter_ref_u64`
- `enter_ref_pair_u64_u64`
- `call_callable_sig_X`

These shims are acceptable only if they are:

- thin tag/shape dispatch
- not generic evaluation loops
- immediately tailing into compiled entry kernels or returning ready values

They are dispatch glue, not the evaluator.

## Kajit HIR shape

Slice 0 should target Kajit HIR, not raw RVSDG.

That fits the HIR direction in
[`docs/hir-design.md`](/Users/amos/bearcove/kajit/docs/hir-design.md):

- explicit state and locals
- ordinary control flow
- semantic structs/functions/calls

### HIR-level semantic types needed

Slice 0 lowering likely needs semantic HIR types for:

- `Ref<T>`
- `Obs<T>`
- closure object layouts
- environment object layouts
- thunk object layouts
- product object layouts
- tiny runtime context type

The important point is that these are semantic HIR types, not backend ABI blobs.

### HIR-level function shapes

Generated HIR functions should fall into a few patterns:

- `enter_*` functions for thunk/function entry
- tiny helper functions for direct strict computation
- optional typed dispatch helpers

HIR should model:

- locals
- field access
- calls
- `if`
- struct construction
- typed runtime helper calls

This is a good fit for Kajit's HIR direction.

## RVSDG/state-domain expectations

Slice 0 should keep RVSDG state modest.

The likely first requirement is:

- one runtime/heap mutation domain for allocation and object mutation

Possibly:

- reuse Kajit's existing generic memory domain
- or introduce one named `slice0_heap` domain that lowers to memory-backed
  object operations

What slice 0 should not need:

- cursor/output-style ambient domains
- a scheduler domain
- keyed/shared solver domains

The important thing is that alloc/update/trap boundaries are explicit, while
ordinary local arithmetic and control flow stay pure where possible.

## Lowering rules by source form

Slice 0 should commit to specific lowering rules for each included source form.

### Literals

Lower scalar literals as:

- ready strict values

If a lazy ref is needed, they may be wrapped in an immediate-ready `Ref<T>` form
without allocating a thunk.

### `let`

Lower:

```vx
let x = e1;
e2
```

by:

- first planning uses of `x`
- then either:
  - inline `e1` into the local kernel if it stays local and singly demanded
  - or extract one thunk for `x` if it needs identity/sharing

`let` is where most local-sharing decisions happen.

### Tuple / struct literal

Lower a demanded lazy product by:

- allocating or materializing one product shell
- storing field refs, not forced field values

Demanded field refs may be:

- immediate-ready scalar refs
- extracted thunks
- other product refs

### Projection

Lower projection as:

- demand enough to obtain the product shell
- select the field ref
- enter that field ref only if the parent expression actually needs the field's
  value

Projection should never implicitly force sibling fields.

### `if`

Lower `if` by:

- entering the condition under `NeedValue`
- branching in ordinary HIR control flow
- lowering only the selected branch under the original result observation

This is straight-line compiled control flow once the condition is known.

### Closure literal

Lower a closure literal by:

- computing its capture set
- building an environment layout
- allocating an env object if captures exist
- producing a typed callable value `{ code_id, env }`

The closure body itself becomes a compiled entry kernel.

### Call

Lower a call under result observation `obs_r` by:

1. obtain the callee under `NeedCallable`
2. lower arguments as lazy refs, not strict values
3. allocate a result thunk if the call result needs lazy identity
4. otherwise enter the callee directly into the current demanded context

The important distinction is:

- calling is not the same thing as eagerly computing the full result

### Sharing

When one binding is used from multiple sites:

- compute the join of demanded observations
- create one shared thunk/result identity
- update and reuse it

This is where the planning result from
[`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md)
feeds directly into code generation.

## Worked lowering sketches

The point here is not final syntax. It is the execution shape.

### Program A

From [`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md#L299):

```vx
fn bomb() -> U64 = trap("bomb")

fn main() -> U64 {
  let pair = (41 + 1, bomb());
  pair.0
}
```

Planning result:

- `pair` needs lazy product identity
- field `0` is demanded under `NeedValue`
- field `1` stays latent

Lowered shape:

1. allocate product shell for `pair`
2. field `0` becomes an immediate-ready `Ref<U64>` holding `42`
3. field `1` becomes a thunk for `bomb`
4. projection selects field `0`
5. no code path enters field `1`

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

Planning result:

- `x` is shared
- joined observation on `x` is `NeedValue`
- `x` therefore becomes one thunk with reusable identity

Lowered shape:

1. allocate one thunk for `plus_one(n)`
2. first use of `x` enters that thunk under `NeedValue`
3. thunk entry enters `n` under `NeedValue`, computes `n + 1`, updates cache
4. second use of `x` reuses the cached result
5. outer `+` is ordinary strict compiled arithmetic

## Interaction with the existing Kajit Vixen prototype

Kajit's existing `VixenTypedExpr` / `VixenTypedFunction` path in
[`kajit-hir/src/lib.rs`](/Users/amos/bearcove/kajit/kajit-hir/src/lib.rs#L244)
is a useful seed, but slice 0 needs at least four extensions before it matches
this plan:

1. typed `Ref<T>` and `Obs<T>` in the frontend model
2. explicit closure literals / environments
3. explicit node extraction before HIR emission
4. explicit runtime helper calls for alloc/update/dispatch

Without those, the prototype path is still closer to "typed eager lowering" than
to a lazy execution model.

## Deliverables for lowering planning

This note completes the planning only when it is paired with:

1. the semantic subset in
   [`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md)
2. the object/ABI note in
   [`slice0-abi.md`](/Users/amos/bearcove/vixen/docs/design/runtime-abi.md)
3. the observation note in
   [`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md)

After that, the remaining work is implementation planning, not conceptual
guesswork:

- define the slice-0 lazy core precisely
- decide the first HIR type encodings for `Ref<T>` and `Obs<T>`
- pick the tiny runtime helper surface
- build the four canonical programs as a reference lowering corpus

## What this note intentionally does not settle

This note does not yet choose:

- the exact shape of the internal lazy core IR
- the exact HIR syntax/extensions for `Ref<T>` and `Obs<T>`
- the exact dispatch strategy for callable/ref entry
- the exact object-header format
- how much specialization happens ahead of time vs on demand

Those are still open.

What should no longer be open after this note is the shape of the lowering
problem:

- plan demand first
- extract lazy identity explicitly
- compile kernels through Kajit
- keep runtime work narrow and explicit
