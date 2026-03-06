# Vixen Program Sketches

This document tracks the active sketch direction for pressure-testing the
language surface against the kinds of programs Vixen actually needs to express.

The point is not to defend a specific syntax feature in isolation. The point is
to sketch realistic rule logic and see what feels natural, awkward, or missing.

The current active branch is:

- [vx-program-sketches-functional.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches-functional.md)
  explores a more expression-oriented direction with closures, collection
  combinators, and pipelines.

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
- pipelines are under active consideration
- error handling is being rethought away from Rust's `?`/`Result` surface
- the real pressure point is now less about methods and more about where we
  still want explicit reassignment, output emission, and typed diagnostics

That suggests the next syntax discussion should focus less on `self` and more
on:

- local mutation model
- output-emission model
- error/reporting model
