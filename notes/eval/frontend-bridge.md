# frontend bridge

Status: planning draft.

This note defines the seam the evaluator should use on the input side.

The goal is simple:

- reuse the real Vixen frontend
- do evaluator analysis there
- stop treating the reference harness as the source language for lowering

## Problem statement

The evaluator needs an input artifact that is:

- derived from real authored Vixen
- typed enough for early analysis
- explicit enough about bindings, calls, closures, and products
- independent of old runtime execution assumptions

Today we have two useful sources:

1. frontend data from parsing, resolution, and type checking
2. older compiled internal program representations

The bridge should be built from the first set, while borrowing ideas from the
second only when that avoids duplicated parsing or binding work.

## What the bridge must provide

The frontend bridge does not need to solve code generation.
It only needs to present real frontend information in a form analysis can use.

For the first slice, the bridge output should provide:

- function definitions
- closure literals
- binding sites and use sites
- call sites
- projection and product-construction sites
- static types for expressions and parameters
- enough lexical information to derive capture sets
- stable ids for expressions, bindings, functions, and closure sites

It should not yet decide:

- what becomes a runtime node
- what becomes a compiled strict fragment
- exact internal execution-layer operations
- JIT lowering details

Those belong to later analysis and lowering stages.

## Proposed bridge output

The bridge output should be a frontend-derived evaluator program model.

Plainly:

- not the raw CST
- not the reference-harness AST
- not an old bytecode/execution plan

It should be a typed expression tree or graph with stable ids and resolved
bindings.

## First consumer of the bridge

The first thing consuming this bridge should be the analysis pass.

That pass should compute:

- demanded observations
- strict local cones
- extracted nodes
- strict-fragment entrypoints
- closure environment layouts
- frame/thunk layouts

Only after that should anything lower into the internal execution layer or a
JIT-facing strict-fragment IR.

## Non-goals

This bridge should not:

- reuse the reference-harness AST
- smuggle in old scheduling/runtime behavior
- prematurely choose object layouts
- prematurely encode JIT helper calls
