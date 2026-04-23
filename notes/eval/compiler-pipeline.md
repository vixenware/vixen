# compiler pipeline

Status: planning draft.

This note is the lowering-plan companion to:

- [`canonical-examples.md`](canonical-examples.md)
- [`observations.md`](observations.md)
- [`runtime-abi.md`](runtime-abi.md)
- [`frontend-bridge.md`](frontend-bridge.md)
- [`analysis-plan.md`](analysis-plan.md)

Its purpose is to explain how the evaluator should lower a small typed Vixen
subset into runtime nodes plus JIT-friendly strict fragments.

## Goal

The pipeline should prove that the following can coexist:

- demand-driven Vixen semantics
- typed observations
- explicit lazy identity where needed
- compiled local execution for strict fragments

The lowering must not secretly collapse into:

- a generic runtime evaluator
- a universal `Value` representation
- eager argument forcing

## Pipeline shape

For the first slice, the intended pipeline is:

```text
typed frontend program
  -> frontend bridge
  -> analysis result
  -> internal execution layer
  -> strict-fragment IR
  -> JIT backend
```

The important point is that demand planning and node extraction happen before
backend IR emission.

The backend should compile the resulting strict fragments. It should not be
asked to infer laziness from already-eager lowered code.

## Why analysis comes first

If we lower ordinary Vixen expressions straight into direct backend expressions,
we have already lost the semantics.

Example:

```vx
let pair = (41 + 1, bomb());
pair.0
```

An eager lowering would emit both tuple fields as direct computations.
That is wrong.

The pipeline must first determine:

- the demanded observation on `pair` only needs field `0`
- field `1` stays latent
- `41 + 1` can stay local strict work
- `bomb()` must not be pulled into the demanded cone

Only after that should strict-fragment IR be emitted.

## Planning output

For the first slice, planning should produce four things:

1. Observation on each expression use site
2. A set of expressions extracted as nodes
3. A set of strict-fragment entrypoints to generate
4. Concrete environment/frame layouts for closures and thunks

This is the artifact that drives the rest of lowering.

## Internal execution layer

Before lowering to the backend, the evaluator should pass through a small
internal layer whose terms already make the split explicit.

That layer does not need to be a public user-facing IR.
It exists to make the strict-fragment/runtime-node split concrete.

It should distinguish:

- strict typed values
- `Ref<T>`
- `Obs<T>`
- callable values
- runtime-node allocation/entry/update

It should be explicit enough that lowering into a JIT-facing strict-fragment IR
is mostly mechanical.

## Node extraction rule

For the first slice, extract a runtime node when an expression:

- is shared across multiple uses
- escapes as a lazy subresult
- becomes a captured closure environment value
- becomes a field of a lazy product
- crosses a host/runtime boundary
- otherwise needs stable lazy identity

Keep an expression inside the current strict fragment when it is:

- local
- demanded under one known observation
- non-escaping
- not shared independently

## Generated artifact kinds

The first slice only needs a small number of compiled artifact kinds:

- function-item entry fragments
- closure entry fragments
- thunk entry fragments
- small typed dispatch shims where a callable or ref shape is dynamic

Those shims are acceptable only if they are thin dispatch, not hidden evaluator
loops.

## Backend rule

The backend is an implementation detail of strict-fragment execution.

The notes should say `JIT`, not a particular backend name, because the semantic
requirements are:

- typed values
- typed refs
- explicit observations
- explicit boundaries
- no eager shortcutting during lowering

## Success criteria

The pipeline is successful for the first slice if:

1. analysis happens before lowering
2. canonical examples lower into strict fragments plus explicit nodes
3. no generic eval loop appears in the hot path
4. ordinary local computation can run in one shot inside a fragment
5. runtime re-entry only happens at real node/boundary points
