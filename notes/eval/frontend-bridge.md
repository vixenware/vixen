# eval3 frontend bridge

Status: planning draft.

This note defines the seam `eval3` should use on the input side.

The goal is simple:

- reuse the real Vixen frontend
- do `eval3` analysis there
- stop treating the reference harness as the source language for lowering

## Problem statement

`eval3` needs an input artifact that is:

- derived from real authored Vixen
- typed enough for slice-0 analysis
- explicit enough about bindings, calls, closures, and products
- independent of `eval2` runtime semantics

Today we have two things, and neither is exactly that:

1. frontend data from `vx-parser` / `vx-resolve`
2. `eval2`'s compiled internal program representation

We should build the `eval3` bridge from the first set, while borrowing ideas
from the second only where that reduces duplicated parsing/binding work.

## Existing frontend artifacts

### Parser

`vx-parser` gives us CST/AST access rooted at `SourceFile`.

This is the real surface syntax of Vixen.

### Resolution and scope

`vx-resolve` already gives us:

- `NameResolution`
- `ProgramScopes`
- definition lookup
- namespace/member resolution

See [vx-resolve/lib.rs](/Users/amos/bearcove/vixen/crates/vx-resolve/src/lib.rs).

### Types

`vx-resolve` also gives us:

- `TypeCollection`
- expression typing results in `TypeCheckResult`

This is not yet a lowering-ready typed IR, but it is already the right source
of truth for frontend-derived types.

### eval2 compiled program

`vx-eval2` already compiles parsed Vixen into an internal executable program
representation with:

- compiled functions
- expression kinds
- expression execution plans

See [program.rs](/Users/amos/bearcove/vixen/crates/vx-eval2/src/program.rs).

This proves there is already a real in-tree seam from frontend syntax into an
internal program model.

It is still not the `eval3` bridge directly, because it already carries `eval2`
execution assumptions.

## What the bridge must provide

The `eval3` bridge does not need to solve code generation.
It only needs to present real frontend information in a form analysis can use.

For slice 0, the bridge output should provide:

- function definitions
- closure literals
- binding sites and use sites
- call sites
- projection and product-construction sites
- static types for expressions and parameters
- enough lexical information to derive capture sets
- stable IDs for expressions, bindings, functions, and closure sites

It should not yet decide:

- what becomes a runtime node
- what becomes a compiled strict fragment
- exact lazy-core operations
- Kajit lowering details

Those belong to later analysis/lowering stages.

## Proposed bridge output

The bridge output should be a frontend-derived, eval3-specific program model.

Plainly:

- not the raw CST
- not the reference harness AST
- not `eval2` bytecode/execution plans

It should be a typed expression graph or tree with stable IDs and resolved
bindings.

For each function or closure body, we need records like:

- body ID
- parameter list with types
- expression tree with stable expression IDs
- binding-use links
- call/callee structure
- field keys for projections
- source spans for diagnostics/debugging

## First consumer of the bridge

The first thing consuming this bridge should be the `eval3` analysis pass.

That pass should compute:

- demanded observations
- strict local cones
- extracted nodes
- strict-fragment entrypoints
- closure environment layouts
- frame/thunk layouts

Only after that should anything lower into lazy core or Kajit-facing IR.

## Non-goals

This bridge should not:

- reuse the reference harness AST
- smuggle in `eval2` scheduling/runtime behavior
- prematurely choose object layouts
- prematurely encode Kajit helper calls

## Immediate follow-up

Once this seam is accepted, the next note should define the analysis output in
the same concrete way:

- bridge input
- analysis output
- invariants
- how Programs A-D exercise it
