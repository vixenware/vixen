+++
title = "Big Ideas"
insert_anchor_links = "heading"
weight = 10
+++

# Vixen Big Ideas

This page is an overview, not a normative rulebook. The goal is to explain
what Vixen is for and why the rest of the specification looks the way it does.

> v[ideas.overview]
>
> Vixen is a lazy, demand-driven language for build and execution
> orchestration. Its design centers on explicit values, explicit boundaries,
> deterministic results, and a runtime that can eventually analyze authored
> Vixen and compile straight-line kernels without changing language semantics.

## A build is just code

Vixen is not trying to be a pile of special-purpose build-file forms. A build
is an ordinary Vixen program that returns a `Tree`.

That has a few direct consequences:

- abstraction is ordinary language abstraction
- helpers are ordinary functions
- discovery logic is ordinary code
- build composition is ordinary value composition

The language should not fork into a "normal language" and a separate "build
DSL". The build surface is a library and runtime surface layered on top of the
language.

## The core execution model is lazy and demand-driven

Vixen is not a strict language with some caching bolted on afterward.

Evaluation is driven by demand. Work happens because some boundary demands a
result. Inside the orchestrator, language operations do not force values just
because syntax mentions them.

That is why the spec is careful about:

- pending values
- forcing boundaries
- branch demand
- memoization
- observer and executor boundaries

These are not optimization details. They are part of the model.

## Values stay as values

Vixen is built around value semantics.

- values are comparable
- values are hashable
- values are serializable
- closures are values
- build artifacts such as `Blob`, `Tree`, and `Path` are values at the
  language surface

This matters because caching, reuse, shipping work to executors, and stable
incremental behavior all depend on values having clear identity.

## Build systems need host interaction, but host interaction must stay explicit

Real builds need to:

- find tools
- run tools
- read and assemble filesystem content
- talk to remote or heterogeneous executors

Vixen does not pretend those concerns do not exist. Instead, it gives them
explicit surfaces:

- `discover` for executor-host observation
- `exec` for sandboxed process execution
- `Blob`, `Tree`, and `Path` for build artifacts and locations

The design goal is to make these interactions explicit and deterministic enough
to cache, replay, verify, and distribute.

## Determinism is a feature, not cleanup work

Vixen wants deterministic results by construction where possible.

That is why the spec leans on:

- total ordering across values
- canonical map ordering
- stable function identity
- explicit executor classes
- content-addressed blobs and trees

Determinism is not there to make tests pretty. It is there so the runtime can
reuse work, compare results, and move work across machines without becoming
guesswork.

## The system is meant to get smarter than a naive interpreter

The long-term goal is not "an interpreter, but faster." The goal is a system
that can analyze authored Vixen and decide what can be compiled into straight-
line kernels and what must remain lazy runtime structure.

That means the implementation has to care about questions like:

- what is actually demanded
- what must stay shareable
- what escapes
- what crosses executor or host boundaries
- what can be compiled without changing semantics

Those are design constraints on the implementation, not just backend details.

## The language, standard library, and runtime are separate on purpose

Vixen will stay easier to reason about if these layers stay distinct:

- the language defines syntax, values, functions, control flow, and types
- the standard library defines user-facing build and runtime surfaces
- the internals spec defines the accepted implementation model
- this ideas section explains why the system is shaped that way

If those layers blur together, the docs become hard to trust and the
implementation drifts.
