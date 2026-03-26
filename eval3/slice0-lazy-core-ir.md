# eval3 slice 0 lazy core IR

Status: planning draft.

This note makes
[`slice0-lazy-core.md`](/Users/amos/bearcove/vixen/eval3/slice0-lazy-core.md)
concrete.

The previous note explained what the lazy core must do.
This note sketches what the core should actually look like as an internal IR.

The goal is not final syntax.
The goal is to make implementation choices obvious enough that we can stop
saying "some lazy core" and start saying "that IR".

## Shape

The slice-0 lazy core should be:

- typed
- statement-oriented
- ANF-ish
- explicit about strict vs lazy
- explicit about alloc / enter / update
- small enough to lower mechanically into Kajit HIR

It should not be:

- expression-tree based
- stack-machine based
- a bytecode
- a generic runtime eval format

## Why statement-oriented

For slice 0, the important boundaries are operational:

- allocate a thunk
- enter a ref under an observation
- branch on a strict bool
- update a thunk cache

Those are clearer as statements over named locals than as nested expressions.

That also matches Kajit HIR better.

## Top-level IR entities

The core IR should have four top-level entity kinds.

### 1. Kernel

A kernel is a compiled entry body.

Examples:

- top-level function entry
- closure entry
- thunk entry
- tiny dispatch helper

### 2. Layout

Compile-time layout metadata for:

- environments
- product shells
- thunk frames

These are not runtime values.
They are compiler identities used by alloc/load/store/project operations.

### 3. Constant / code id

The IR needs symbolic references to:

- kernel ids
- layouts
- string-like trap labels

### 4. Type

The IR should use semantic types, not backend ABI blobs.

Examples:

- `U64`
- `Bool`
- `Ref<U64>`
- `Obs<U64>`
- `Callable<(U64) -> U64>`
- `ProductHandle<PairU64U64>`
- `ThunkHandle<U64>`

## Kernel signature

Every kernel should have:

- a name / kernel id
- parameter list
- local declarations
- blocks

Recommended signature shape:

```text
kernel enter_plus_one(
  ctx: RuntimeCtx,
  self: ThunkHandle<U64>,
  arg0: Ref<U64>,
  obs: Obs<U64>,
) -> Never
```

Returning `Never` here means:

- entry kernels update `self` and tail into a return/update path
- they do not return a source-level value directly

That matches the thunk-update execution model better than pretending entry
kernels are ordinary pure functions.

For direct strict helpers, ordinary typed returns are fine.

## Local kinds

Locals should be explicitly typed and fall into a few kinds:

- strict local
- ref local
- callable local
- observation local
- handle local

This is mostly type-driven, but keeping the distinction visible helps debugging.

## Statement vocabulary

The IR should have a deliberately small statement set.

### Pure statements

```text
v = const_u64(42)
b = const_bool(true)
v = add_u64(x, y)
v = copy(x)
```

These only operate on strict values.

### Ref construction statements

```text
r = ready_u64(v)
r = ready_bool(b)
```

These construct immediate-ready refs without allocating a thunk.

### Allocation statements

```text
env = alloc_env EnvLayout42 { capture0: r0, capture1: r1 }
clos = alloc_closure kernel_add_n env
t = alloc_thunk thunk_plus_one { arg0: n_ref }
p = alloc_product PairLayout { field0: r0, field1: r1 }
```

These are the points where lazy identity becomes explicit.

### Observation / entry statements

```text
v = enter_u64(r, NeedValue)
b = enter_bool(r, NeedValue)
callable = need_callable(f_ref)
```

For products:

```text
p = enter_product_pair(pair_ref, NeedProduct { 0: NeedValue })
```

But for slice 0, product entry can usually be simplified to:

- obtain the shell
- project field refs
- enter child refs separately

So the preferred operational pattern is:

```text
field = project_pair_0(pair_ref)
v = enter_u64(field, NeedValue)
```

### Update statements

```text
mark_evaluating(self)
cache_ready_u64(self, v)
cache_ready_product(self, p)
redirect_updated(self, r)
```

These make sharing/update explicit.

### Control statements

```text
if b goto then_bb else else_bb
goto next_bb
trap "bomb"
```

### Terminators

Slice 0 only needs a few:

```text
return_strict(v)
return_ref(r)
done
```

For thunk entry kernels, `done` after cache/update is probably enough.

## Typed helper ops instead of mega-ops

The IR should prefer typed helper ops over generic dynamic ops.

Good:

```text
enter_u64(r, NeedValue)
project_pair_0(pair_ref)
cache_ready_u64(self, v)
```

Bad:

```text
eval(runtime_value, dynamic_mode_blob)
```

This is the main discipline that keeps the lazy core from degenerating into
"eval2 in a different syntax".

## Observations in the IR

The IR should model observations explicitly, but keep them simple.

For slice 0:

- scalar observations can be direct enum literals
- product observations can be constants or small literals attached to project /
  enter planning

Examples:

```text
NeedValue
NeedCallable
NeedProductPair { field0: NeedValue }
```

The IR does not need a fully generic runtime-observation construction language
yet.

## Blocks and control flow

The core IR should use ordinary basic blocks.

That matters for:

- `if`
- explicit handling of thunk state
- reuse vs first-entry branches

Example thunk-entry shape:

```text
entry:
  state = thunk_state(self)
  if state_is_ready(state) goto reuse else eval

reuse:
  v = load_cached_u64(self)
  cache_ready_u64(self, v)
  done

eval:
  mark_evaluating(self)
  n = enter_u64(arg0, NeedValue)
  one = const_u64(1)
  out = add_u64(n, one)
  cache_ready_u64(self, out)
  done
```

This is close to executable shape already, which is exactly what we want.

## Canonical IR examples

### Program A

Source:

```vx
fn bomb() -> U64 = trap("bomb")

fn main() -> U64 {
  let pair = (41 + 1, bomb());
  pair.0
}
```

Suggested lazy-core shape:

```text
kernel enter_main(ctx, self, obs):
entry:
  forty_one = const_u64(41)
  one = const_u64(1)
  forty_two = add_u64(forty_one, one)
  field0 = ready_u64(forty_two)
  field1 = alloc_thunk thunk_bomb {}
  pair = alloc_product PairU64U64 { field0: field0, field1: field1 }
  selected = project_pair_0(pair)
  out = enter_u64(selected, NeedValue)
  cache_ready_u64(self, out)
  done

kernel thunk_bomb(ctx, self, obs):
entry:
  trap "bomb"
```

Important property:

- `thunk_bomb` exists
- `thunk_bomb` is never entered on the successful path

### Program C

Source:

```vx
fn plus_one(n: U64) -> U64 {
  n + 1
}

fn main(n: U64) -> U64 {
  let x = plus_one(n);
  x + x
}
```

Suggested lazy-core shape:

```text
kernel enter_plus_one(ctx, self, n_ref, obs):
entry:
  state = thunk_state(self)
  if state_is_ready(state) goto reuse else eval

reuse:
  out = load_cached_u64(self)
  cache_ready_u64(self, out)
  done

eval:
  mark_evaluating(self)
  n = enter_u64(n_ref, NeedValue)
  one = const_u64(1)
  out = add_u64(n, one)
  cache_ready_u64(self, out)
  done

kernel enter_main(ctx, self, n_ref, obs):
entry:
  x = alloc_thunk enter_plus_one { n_ref: n_ref }
  lhs = enter_u64(x, NeedValue)
  rhs = enter_u64(x, NeedValue)
  sum = add_u64(lhs, rhs)
  cache_ready_u64(self, sum)
  done
```

Important property:

- both uses of `x` target the same thunk identity

### Program D

Source:

```vx
fn apply(f: Fn(U64) -> U64, x: U64) -> U64 {
  f(x)
}

fn main(n: U64) -> U64 {
  let add_n = (x) => x + n;
  apply(add_n, 41)
}
```

Suggested lazy-core shape:

```text
kernel enter_add_n(ctx, self, env, x_ref, obs):
entry:
  n_ref = env_get_capture_0(env)
  x = enter_u64(x_ref, NeedValue)
  n = enter_u64(n_ref, NeedValue)
  out = add_u64(x, n)
  cache_ready_u64(self, out)
  done

kernel enter_apply(ctx, self, f_ref, x_ref, obs):
entry:
  callable = need_callable(f_ref)
  result = alloc_thunk call_result { callable: callable, arg0: x_ref, obs: NeedValue }
  out = enter_u64(result, NeedValue)
  cache_ready_u64(self, out)
  done

kernel call_result(ctx, self, callable, arg0, obs):
entry:
  call_callable_u64_to_u64(callable, self, arg0, obs)
  done
```

This is enough to show:

- closure env is explicit
- callability is explicit
- arguments remain refs

## Minimum runtime helper surface exposed to the IR

The lazy core should rely on a tiny helper surface.

Recommended first helpers:

- `alloc_env`
- `alloc_closure`
- `alloc_thunk`
- `alloc_product`
- `thunk_state`
- `state_is_ready`
- `mark_evaluating`
- `load_cached_*`
- `cache_ready_*`
- `project_*`
- `need_callable`
- `call_callable_*`
- `trap`

That is already enough for slice 0.

If the helper list grows rapidly, the core is probably leaking semantics back
into the runtime.

## Lowering rules from source planning into this IR

The compiler step into the lazy core should follow these rules:

1. Every shared/escaping/captured computation already chosen by node extraction
   becomes an explicit `alloc_thunk` or `alloc_product`.
2. Every strict local cone becomes straight-line strict statements.
3. Every projection becomes `project_*` plus an explicit `enter_*` only where
   demanded.
4. Every function/closure body becomes an `enter_*` kernel signature.
5. Every call keeps arguments as refs.

This means the lazy core is where planning decisions become executable shape.

## Lowering rules from this IR into Kajit HIR

The next lowering step should be mostly mechanical:

- kernel params -> HIR params
- locals -> HIR locals
- strict ops -> HIR arithmetic / control
- alloc/update/project/enter -> HIR calls to typed helper callables
- blocks -> HIR blocks / `if`

If the HIR lowering needs to rediscover laziness, the core is too vague.

## What is still intentionally open

This note still does not settle:

- exact textual syntax
- exact SSA vs mutable-local form
- exact product-observation literal encoding
- whether `need_callable` is always explicit or sometimes folded into call sites
- whether `done` is a real terminator or sugar for a runtime return/update path

Those are implementation details.

What should now be concrete is the IR shape:

- statement-oriented
- typed
- explicit refs
- explicit observations
- explicit alloc / enter / update
- directly lowerable into Kajit HIR
