# implementation plan

Status: active restart note.

This note exists so `eval3` work survives context loss and does not drift back
into toy-lowering work.

## Plain-language problem

`eval3` is not mainly a codegen problem.

`eval3` is mainly an analysis problem:

- start from real Vixen frontend output
- decide what stays lazy runtime state
- decide what can be compiled as a straight-line kernel
- only then lower the chosen kernels toward Kajit

The key decision for every authored expression is:

- does this become part of a compiled kernel, or
- does this become a runtime node with lazy identity?

That decision depends on:

- demanded observation
- strictness under that observation
- sharing
- escape
- closure capture and environment layout
- host/runtime boundaries
- later, solver/shared-node boundaries

If the work is not attacking those decisions, it is probably not moving `eval3`
forward.

## Current diagnosis

Two things are true at once:

1. Removing canonical-pattern matching from `vx-eval3-core` was locally correct.
2. Building lowering around `vx-eval3-oracle::Program` was the wrong seam.

The existing `vx-eval3-core <- vx-eval3-oracle::Program` path is prototype code.
It is not the architecture we should keep building around.

Why:

- the oracle/reference harness is a tiny semantic model for slice-0 examples
- it is not the real Vixen frontend
- the hard part of `eval3` is analysis on real authored Vixen
- lowering the oracle model skips that hard part

## Terms

Use these terms consistently.

### Frontend

The real parser / resolver / type information already in-tree.

This is the input side that `eval3` must reuse.

### Reference harness

The thing currently called the "oracle".

Its job is:

- run tiny canonical examples
- produce result + trace
- check semantic invariants

Its job is not:

- be the frontend for lowering
- become a second language implementation we optimize

### Analysis plan

The first real `eval3` artifact we still need to define.

It should answer, for a fixed frontend-derived program slice:

- what is demanded
- what is local strict work
- what is shared
- what escapes
- what becomes a runtime node
- what kernel entrypoints must exist
- what closure/env/frame layouts are needed

### Lazy core

An internal IR after analysis decisions have already been made.

It is not where we discover laziness.
It is where we encode already-decided laziness explicitly enough to lower to
Kajit.

## What to keep

Keep these:

- the active design note set under `docs/design/`
- the four canonical slice-0 examples
- the trace/event vocabulary from `vx-eval3-oracle`
- the idea that result assertions alone are not enough; we also need semantic
  trace assertions
- any `vx-eval3-core` IR pieces that still match the slice-0 notes after the
  frontend/analysis seam is fixed

## What to stop doing

Stop doing these:

- extending lowering from `vx-eval3-oracle::Program`
- treating the reference harness as the source language for the compiler
- spending effort on `vx-eval3-core` lowering coverage before the frontend
  analysis boundary is defined
- arguing about Kajit lowering details before the analysis result is explicit

## Four canonical examples

These are still worth keeping. They prove:

1. Program A: lazy field selection
   - observing `pair.0` does not enter `bomb()`
2. Program B: closure capture
   - closures have explicit environment identity
   - unused siblings stay unentered
3. Program C: local sharing
   - one shared local thunk enters once and is reused
4. Program D: higher-order local helper
   - calling through a closure value stays in the compiled model

These examples should survive as real fixtures and semantic trace checks, even
if the current oracle crate is later replaced or reduced.

## Immediate implementation direction

The next work should happen in this order.

### 1. Lock the testing story

Define the reference harness contract in plain language:

- input fixture
- demanded observation
- expected result
- expected semantic events / non-events

The current useful event vocabulary is:

- node allocated
- node entered
- node reused
- observation widened
- product field projected
- env allocated
- closure allocated
- trap entered

If we later replace the current oracle crate, this contract should survive.

### 2. Define the real frontend bridge

Write down the exact input artifact `eval3` analysis consumes from the existing
frontend.

This bridge must come from real Vixen frontend structures, not the oracle AST.

The bridge only needs enough information for slice-0 analysis:

- functions / closures
- bindings / local uses
- call sites
- projection / product construction
- static type information
- capture sets or enough info to derive them

### 3. Define the analysis output

Before more lowering code, define one explicit analysis data structure.

It should include at least:

- demanded observation per use site
- extracted node set
- compiled kernel set
- kernel parameters / result observation
- closure environment layouts
- thunk/frame layouts
- runtime-boundary markers

This is the real missing centerpiece.

### 4. Lower the analysis result, not raw expressions

Only after steps 1-3 should we lower into:

- slice-0 lazy core
- Kajit-facing HIR shape

At that point, lowering should be mostly mechanical.

## Short-term code policy

Until the frontend bridge exists:

- `vx-eval3-oracle` is a reference harness, not the compiler frontend
- `vx-eval3-core` is prototype-only
- no new feature work should be added to the current oracle-AST lowering path
  unless it directly supports extracting the restart boundary

## Concrete next tasks

These are the next tasks that should survive agent/context churn.

1. Replace "oracle as lowering input" with "reference harness only" in docs.
2. Write the frontend-bridge note:
   - what existing frontend artifacts exist
   - what extra derived data `eval3` needs
   - what the bridge output should look like
3. Write the analysis-plan note:
   - inputs
   - outputs
   - invariants
   - how Programs A-D exercise it
4. Only then decide whether `vx-eval3-core` should be refactored or replaced.

## Decision rule

When evaluating a proposed `eval3` task, ask:

"Does this make the frontend-to-analysis-to-kernel boundary clearer?"

If no, it is probably not the next task.
