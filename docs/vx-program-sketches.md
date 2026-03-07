# Vixen Program Sketches

This document tracks the active sketch direction for pressure-testing the
language surface against the kinds of programs Vixen actually needs to express.

The point is not to defend a specific syntax feature in isolation. The point is
to sketch realistic rule logic and see what feels natural, awkward, or missing.

The current active branch is:

- [vx-program-sketches-functional.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches-functional.md)
  explores a more expression-oriented direction with closures, collection
  combinators, `match`-centric control flow, and default error propagation.

Older sketch branches were intentionally removed once the design moved away from
them.

Current constraints for these sketches:

- no Kajit-backed semantics yet
- no new interpreter design yet
- method syntax allowed
- no user-defined methods assumed
- no references assumed
- closures now belong in the active exploration
- no recursion
- functional transforms now look closer to the actual shape of Vixen rule code
- method chaining is preferred over a pipeline operator, for now
- error handling is being rethought away from Rust's `?`/`Result` surface
- bounded `for` remains in the language for imperative multi-step code
- `if` remains in the language for simple guards
- the real pressure point is now less about methods and more about where we
  still want explicit writeback, output emission, and typed diagnostics

That suggests the next syntax discussion should focus less on `self` and more
on:

- local writeback model
- output-emission model
- error/reporting model
