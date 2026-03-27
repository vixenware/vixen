# eval3 analysis plan

Status: planning draft.

This note defines the first artifact that matters for `eval3` implementation.

The compiler should not lower raw expressions directly into lazy core or Kajit.
It should first produce an analysis result that explains:

- what is demanded
- what is local strict work
- what needs lazy identity
- what becomes a compiled kernel

## Input

The input to this phase is the frontend bridge described in
[`frontend-bridge.md`](/Users/amos/bearcove/vixen/docs/design/frontend-bridge.md).

For slice 0, that means the analysis can assume access to:

- typed function and closure bodies
- stable expression IDs
- binding sites and use sites
- call sites
- projection and product-construction sites
- enough lexical information to derive captures

## Output

The output should be one explicit analysis data structure.

For each analyzed body, it should answer:

### Observation

- what observation is demanded at the body result
- what observation is demanded at each use site
- where observations are narrowed, forwarded, or widened

### Local strict cones

- which expression regions can stay inside one compiled kernel
- which scalar/control-flow fragments are safe to evaluate straight-line under a
  known observation

### Extracted nodes

- which expressions require lazy identity
- why each extracted node exists

Every extracted node should carry a reason such as:

- shared across uses
- escapes as lazy result
- captured by closure environment
- becomes a field of a lazy product
- crosses a host/runtime boundary
- later: crosses a solver/shared-node boundary

### Kernel entrypoints

- which compiled entry kernels must exist
- their parameters
- their result observation
- what local region of code each kernel owns

### Layouts

- closure environment layouts
- thunk/frame layouts
- any product layouts that analysis must commit to before lazy-core lowering

## Invariants

The analysis result must make these facts explicit.

1. Every extracted node has a reason.
2. Every kernel has a well-defined demanded result observation.
3. Sibling lazy work is not accidentally pulled into a kernel without demand.
4. Shared work is not duplicated silently.
5. Closure capture is derived from real bindings, not guessed later in lowering.
6. The lazy core should not need to rediscover any of the above.

## What analysis is allowed to postpone

This phase does not need to settle:

- exact runtime object layout bytes
- exact helper-call names in lazy core
- exact Kajit HIR instruction selection
- backend ABI details

Those belong to later stages.

## How the four canonical programs exercise the analysis

### Program A

```vx
fn bomb() -> U64 = trap("bomb")

fn main() -> U64 {
  let pair = (41 + 1, bomb());
  pair.0
}
```

The analysis must conclude:

- the result demand on `pair` is only field `0`
- field `1` remains latent
- `41 + 1` can stay in local strict work
- `bomb()` must not be pulled into the demanded cone

### Program B

```vx
fn main(n: U64) -> U64 {
  let add_n = (x) => x + n;
  let pair = (add_n(1), add_n(2));
  pair.1
}
```

The analysis must conclude:

- `add_n` captures `n`
- that capture requires a closure environment layout
- only the second sibling call is on the demanded path
- the first sibling call remains latent

### Program C

```vx
fn plus_one(n: U64) -> U64 {
  n + 1
}

fn main(n: U64) -> U64 {
  let x = plus_one(n);
  x + x
}
```

The analysis must conclude:

- `x` is shared
- `x` therefore needs lazy identity
- the call result should be extracted once and reused
- `plus_one`'s local arithmetic can still become compiled kernel work

### Program D

```vx
fn apply(f: Fn(U64) -> U64, x: U64) -> U64 {
  f(x)
}

fn main(n: U64) -> U64 {
  let add_n = (x) => x + n;
  apply(add_n, 41)
}
```

The analysis must conclude:

- `add_n` captures `n`
- `apply` receives a callable value, not an erased interpreter object
- callability demand is explicit
- argument passing stays lazy by ref

## Why this is the centerpiece

If this artifact is missing, one of two bad things happens:

1. laziness is rediscovered ad hoc during lowering
2. the compiler quietly becomes eager and only preserves semantics in tests by
   accident

The analysis plan is the thing that prevents both failures.

## Suggested implementation order

1. define the Rust data structures for the analysis result
2. build the smallest possible frontend-bridge adapter for Programs A-D
3. populate the analysis result for those four programs
4. check the result with structural assertions
5. only then lower that analysis result into lazy core IR
