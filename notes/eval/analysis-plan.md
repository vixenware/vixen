# analysis plan

Status: planning draft.

This note defines the first artifact that matters for evaluator implementation.

The compiler should not lower raw expressions directly into the internal
execution layer or the JIT backend. It should first produce an analysis result
that explains:

- what is demanded
- what is local strict work
- what needs lazy identity
- what becomes a compiled strict fragment

## Input

The input to this phase is the frontend bridge described in
[`frontend-bridge.md`](frontend-bridge.md).

For the first slice, that means the analysis can assume access to:

- typed function and closure bodies
- stable expression ids
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

- which expression regions can stay inside one compiled strict fragment
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
- later, crosses a keyed/shared-search boundary

### Strict-fragment entrypoints

- which compiled entry strict fragments must exist
- their parameters
- their result observation
- what local region of code each strict fragment owns

### Layouts

- closure environment layouts
- thunk/frame layouts
- any product layouts that analysis must commit to before lowering

## Invariants

The analysis result must make these facts explicit:

1. Every extracted node has a reason.
2. Every strict fragment has a well-defined demanded result observation.
3. Sibling lazy work is not accidentally pulled into a strict fragment without
   demand.
4. Shared work is not duplicated silently.
5. Closure capture is derived from real bindings, not guessed later.
6. The internal execution layer does not need to rediscover any of the above.

## What analysis is allowed to postpone

This phase does not need to settle:

- exact runtime object-layout bytes
- exact helper-call names in the internal execution layer
- exact JIT IR instruction selection
- backend ABI details

It may also postpone the full shared-search implementation, but not the shape
needed to admit it later.

## Why this is the centerpiece

If this artifact is missing, one of two bad things happens:

1. laziness is rediscovered ad hoc during lowering
2. the compiler quietly becomes eager and only preserves semantics in tests by
   accident

The analysis plan is the thing that prevents both failures.
