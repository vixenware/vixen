# eval3 slice 0 observations

Status: planning draft.

This note is the observation-lattice companion to
[`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md) and
[`slice0-abi.md`](/Users/amos/bearcove/vixen/eval3/slice0-abi.md).

Its purpose is simple:

- make "typed demand" explicit for slice 0

Without this note, slice 0 still talks about laziness in English instead of in a
machine-checkable shape.

## Scope

This note only covers the slice-0 semantic subset:

- `U64`
- `Bool`
- tuples
- simple structs
- functions and closures
- `let`
- `if`
- call
- tuple/struct projection

It does not cover:

- arrays
- maps
- enums / `match`
- host/resource types
- keyed/shared solver nodes
- suspension/resume

## What an observation is

An observation answers:

- what information about a value is actually needed?

In slice 0:

- observations are typed
- observations form finite lattices
- evaluation is monotone with respect to observation strength

That means:

- stronger observation may require more work
- weaker already-satisfied observations should be reusable
- the machine should never need an untyped "give me the whole value" fallback

## Global shape

For every slice-0 type `T`, define:

- `Obs<T>`

Every `Obs<T>` includes a bottom element:

- `NoObs`

Meaning:

- no information about the value is required

For non-bottom observations, the intended order is:

- `a <= b` means `b` is at least as demanding as `a`

And:

- `join(a, b)` is the least observation satisfying both

## Scalar observations

For `U64` and `Bool`, the lattice is minimal.

### `U64`

```text
Obs<U64> = NoObs | NeedValue
```

Ordering:

- `NoObs <= NeedValue`

Join:

- `join(NoObs, x) = x`
- `join(NeedValue, NeedValue) = NeedValue`

### `Bool`

```text
Obs<Bool> = NoObs | NeedValue
```

Same ordering and join as `U64`.

This is enough for slice 0 because:

- arithmetic needs `NeedValue`
- `if` needs `NeedValue` on its condition

There is no weaker boolean observation in this slice.

## Callable observations

Functions and closures are values, but slice 0 does not structurally inspect
them.

It only needs to know enough to call them.

So:

```text
Obs<Fn(A...) -> R> = NoObs | NeedCallable
```

Meaning:

- `NeedCallable` asks for a callable value with code identity and optional
  environment identity
- it does not ask for any structural internals of the callable

This matters because:

- closure literals can satisfy `NeedCallable` by allocating a closure object
- non-capturing function items can satisfy `NeedCallable` without an env object

There is no deeper callable observation in slice 0.

## Product observations

Tuples and structs need structural observations.

The key idea is:

- a product observation is per-field
- each field carries its own child observation

So for a tuple `(A, B, C)`:

```text
Obs<(A, B, C)> =
  NoObs
| NeedProduct {
    0: Obs<A>?,
    1: Obs<B>?,
    2: Obs<C>?,
  }
```

Where:

- omitted / `None` field means no observation on that field
- present field carries an observation of the field type

The same idea applies to structs by field id/name instead of tuple index.

### Canonical form

For simplicity, slice 0 should normalize product observations so that:

- `NeedProduct` with all fields `None` collapses to `NoObs`

So every non-bottom product observation names at least one demanded field.

### Ordering

Product observation ordering is fieldwise.

For the same product type:

- `NoObs <= x`
- `NeedProduct(a) <= NeedProduct(b)` iff, for every field:
  - if `a` does not observe the field, it imposes no requirement
  - if `a` observes the field with `oa`, then `b` must observe that field with
    some `ob` such that `oa <= ob`

### Join

Product observation join is fieldwise.

For each field:

- absent joined with absent => absent
- present `oa` joined with absent => present `oa`
- present `oa` joined with present `ob` => present `join(oa, ob)`

Then normalize the all-absent case back to `NoObs`.

### Example

For:

```vx
let pair = ((1, 2), 3);
pair.0.1
```

the demanded observation is:

```text
Obs<((U64, U64), U64)> =
NeedProduct {
  0: NeedProduct {
    1: NeedValue,
  },
}
```

Field `1` of the outer pair is not demanded at all.

## Satisfaction rule

The semantic notion of satisfaction is:

- a value satisfies observation `o` if the demanded information described by `o`
  is justified and can be reused

For slice 0:

- scalar `NeedValue` is satisfied by the scalar itself
- callable `NeedCallable` is satisfied by a function item or closure object
- product observation is satisfied by a product shell whose demanded field refs
  can themselves satisfy the requested child observations

This matches the ABI normalization in
[`slice0-abi.md`](/Users/amos/bearcove/vixen/eval3/slice0-abi.md#L135):

- the shell may exist without forcing every field
- field refs are entered later under their own child observations

## Propagation rules for included expressions

Slice 0 should commit to the following semantic propagation rules.

### Literals

- scalar literal under `NeedValue` is immediately satisfied
- scalar literal under `NoObs` does no work

### Tuple / struct literal

A product literal under:

- `NoObs` does no work
- `NeedProduct { field_i: oi, ... }` propagates `oi` only to the demanded
  fields

Undemanded sibling fields are not entered.

### Projection

For:

```vx
e.field
```

under child observation `o_child`, propagate to `e`:

```text
NeedProduct { field: o_child }
```

Nothing else is demanded from the product.

### `if`

For:

```vx
if cond (then_branch) else (else_branch)
```

under result observation `o`:

1. `cond` is observed under `NeedValue`
2. once the boolean is known, only the selected branch is observed under `o`

The unselected branch is not entered.

### `let`

For:

```vx
let x = e1;
e2
```

under observation `o` on `e2`:

- demand propagates through `e2`
- `e1` is only demanded insofar as uses of `x` inside `e2` require it

This is what makes:

```vx
let x = bomb();
41
```

semantically lazy.

### Function / closure literal

A function item or closure literal under:

- `NoObs` does no work
- `NeedCallable` constructs or exposes the callable value

Closure creation may require allocating an environment object, but it does not
evaluate the closure body.

### Call

For:

```vx
f(arg0, arg1, ...)
```

under result observation `o_result`:

1. observe `f` under `NeedCallable`
2. do not force arguments to strict values
3. construct/enter the call result under `o_result`

The callee body receives its parameters as lazy refs.

This is one of the main semantic commitments of slice 0:

- ordinary function arguments remain lazy

### Sharing

If one local binding is used from multiple demand sites, the demanded
observation on that binding is the join of all those uses.

So:

```vx
let x = plus_one(n);
x + x
```

induces:

- first use demands `NeedValue`
- second use demands `NeedValue`
- joined demand is still `NeedValue`

The point is not that the lattice becomes interesting here.
The point is that the machine now has a formal reason to update/reuse the same
shared thunk instead of treating the uses as unrelated recomputations.

## Canonical examples revisited

### Program A

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L299):

```vx
fn bomb() -> U64 = trap("bomb")

fn main() -> U64 {
  let pair = (41 + 1, bomb());
  pair.0
}
```

Observation on `pair`:

```text
NeedProduct {
  0: NeedValue,
}
```

So `bomb()` remains under `NoObs`.

### Program B

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L316):

```vx
fn main(n: U64) -> U64 {
  let add_n = (x) => x + n;
  let pair = (add_n(1), add_n(2));
  pair.1
}
```

Observation on `pair`:

```text
NeedProduct {
  1: NeedValue,
}
```

So only the second call result is entered.

### Program C

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L332):

```vx
fn plus_one(n: U64) -> U64 {
  n + 1
}

fn main(n: U64) -> U64 {
  let x = plus_one(n);
  x + x
}
```

Observation on `x` from both use sites joins to:

```text
NeedValue
```

One shared local thunk can satisfy that once and be reused.

### Program D

From [`slice0.md`](/Users/amos/bearcove/vixen/eval3/slice0.md#L350):

```vx
fn apply(f: Fn(U64) -> U64, x: U64) -> U64 {
  f(x)
}

fn main(n: U64) -> U64 {
  let add_n = (x) => x + n;
  apply(add_n, 41)
}
```

The call to `apply` observes:

- `f` under `NeedCallable`
- `x` as `Ref<U64>`
- result under `NeedValue`

Inside `apply`, the nested call `f(x)` again observes:

- `f` under `NeedCallable`
- `x` remains a lazy ref
- result under `NeedValue`

## What this note settles

After this note, slice 0 should no longer be vague about:

- what observations exist for the included types
- what the partial order is
- how joined local demand works
- how projection avoids demanding siblings
- how function call preserves lazy arguments

## What remains open

Still open after this note:

- exact bit-level encoding for observation values
- whether product observations are stored inline, boxed, or table-indexed
- how much observation specialization is generated statically vs selected at
  runtime
- the future observation lattices for arrays, enums, host resources, and
  solver/shared nodes

Those are later design choices.

For slice 0, the important thing is that "typed observation" is now explicit.
