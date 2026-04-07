# 016: Capture Pruning For Synthesized Thunk Bodies (`AllocThunk`)

Status: Done (implemented in 014 commit `bf5731ff`, dedicated test added separately).

## Goal

Stop `synthesize_thunk_body` from over-capturing every in-scope binding by default. Captures for extracted thunk bodies should be pruned to only the bindings actually referenced by the thunk body after lowering, matching the closure pruning approach added in work item 009.

## Motivation

Even if runtime forcing is demand-driven, “capture everything” makes:

- thunk allocation heavier
- later remapping / specialization harder
- correctness reasoning around “what escapes” fuzzier

Closures already have a capture-pruning pass based on `collect_referenced_slots`. Thunks should use the same mechanism.

## Work

- Apply the same “lower then prune” algorithm used in `synthesize_closure_body` to `synthesize_thunk_body`:
- Provisionally assign capture param slots for all candidate parent bindings.
- Lower the thunk body.
- Compute `used_slots` from ops + result slot.
- Keep only used capture slots, remap slots, and rebuild `params` and `captures`.
- Ensure the returned `captures: Vec<Operand>` matches the pruned `params` ordering.
- Add a unit test that exercises:
- a lazy product field thunk that mentions only one of several bindings in scope
- the produced `AllocThunk.captures.len()` is minimal (or at least smaller than total bindings).

## Acceptance

- All existing tests stay green.
- New test asserts pruning behavior (counts, or exact capture binding identity if we expose it).

## Notes

This item is purposely about “capture shape”, not about changing the runtime representation of captures to `Ref<T>` (that is a later, larger shift).
