# execution model

Status: foundation note.

Current implementation status lives in
[`implementation-plan.md`](implementation-plan.md).

This note describes the evaluator Vixen is being rebuilt around.

It is not "the old evaluator, but faster". It is the current attempt to make
execution match the language we actually write in
[`zoo/`](/Users/amos/bearcove/vixen/zoo).

## Core claim

Vixen is demand-driven.

That is the primary semantic claim. Laziness follows from it, but
"demand-driven" is the stronger and more useful description for the evaluator.

This means:

- demand is typed and structural
- dead branches stay dead
- closures and captured `where` environments are ordinary code
- `Tree`, `Blob`, `Path`, `discover`, `exec`, observers, sandboxing, and
  verification are ordinary semantics
- keyed/shared search state, joins, assumptions, and conflicts belong in the
  runtime model

The evaluator has to make those semantics cheap enough to execute without
lying about them.

## Architectural bet

The current bet is:

- authored Vixen remains demand-driven
- the compiler extracts strict fragments from demanded local cones
- the runtime owns the parts that are genuinely shared, identity-bearing,
  suspended, effectful, or boundary-heavy
- JIT execution is one strategy for running those strict fragments inside the
  same evaluator/runtime model

So the machine is a mix of:

- JIT-compiled or interpreted strict fragments
- explicit runtime nodes and services

If ordinary local computation has to bounce through the runtime after every
small step, the design has failed.

## Runtime / fragment split

This split is still the center of the design.

A strict fragment is:

- local
- typed
- owned by one demanded cone
- able to do ordinary local reduction without generic runtime round-trips
- eligible for JIT execution when the evaluator can lower it cleanly

A runtime node or boundary is:

- shared
- identity-bearing
- resumable or suspended
- effectful or host-facing
- keyed or globally coordinated
- observed in a way that cannot be treated as one local strict region

The runtime exists to own graph identity, sharing, boundaries, and global
services. JIT code exists to execute local strict work inside that model.

## What the evaluator must honor

The evaluator must be designed for real authored Vixen, not a scalar toy
subset.

That means accounting for:

- build-shaped programs dominated by `Tree`, `Blob`, `Path`, `discover`, `exec`,
  and observer closures
- higher-order code dominated by closures, combinators, and captured
  environments
- shared search and keyed work that requires canonicalization, joins,
  assumptions, and structured conflicts
- provenance, verification, and sandbox-visible boundary behavior

The target is real `zoo` code, especially build-shaped programs. The target is
not just "a few scalar demos happen to lower cleanly".

## Testing is part of the architecture

The testing story is not secondary documentation. It is part of the evaluator
design, because the design is specifically trying to preserve semantic behavior
that is easy to lose during lowering.

See [`testing-strategy.md`](testing-strategy.md).

The short version is:

- final result alone is not enough
- entered and unentered work matters
- observation propagation matters
- sharing and reuse matter
- JIT parity must be semantic parity, not just output parity
- real `zoo` targets must eventually exercise boundary semantics honestly

## Document map

- [`implementation-plan.md`](implementation-plan.md): current roadmap and
  active implementation order
- [`testing-strategy.md`](testing-strategy.md): what the evaluator must prove
  and how it should be tested
- [`frontend-bridge.md`](frontend-bridge.md): input seam from the real frontend
  into evaluator analysis
- [`analysis-plan.md`](analysis-plan.md): analysis result expected between the
  frontend and lowering
- [`canonical-examples.md`](canonical-examples.md): frozen small examples that
  exercise demand, capture, sharing, and higher-order local code
- [`observations.md`](observations.md): slice-0 observation model
- [`runtime-abi.md`](runtime-abi.md): runtime object/calling-model direction for
  refs, thunks, closures, and products
- [`compiler-pipeline.md`](compiler-pipeline.md): analysis-before-lowering
  pipeline toward strict-fragment execution
- [`lazy-core.md`](lazy-core.md) and [`lazy-core-ir.md`](lazy-core-ir.md):
  older internal-layer sketches that still need naming and scope cleanup
- [`reference-harness.md`](reference-harness.md): semantic result + trace
  reference for canonical examples

## Historical lesson

The earlier evaluator proved important things:

- Vixen really is a demand-driven graph language
- keyed/shared work belongs in the runtime model
- joins, assumptions, and conflicts are real semantics
- parallel execution must fit the same model as single-worker execution

It also made the failure modes obvious:

- runtime identities were too large
- too much structure lived inside universal runtime values
- function refs conflated code identity, environment identity, and activation
- too much local churn was promoted into global coordinated state

The fix is not "compile harder". The fix is to get the runtime model and the
strict-fragment split right.
