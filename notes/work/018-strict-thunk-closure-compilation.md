# 018: Strict Compilation For Extracted Thunks + Closure Bodies (Partial Compilation In Mixed Programs)

Status: Not started.

## Goal

Enable “compile some parts through Kajit and evaluate the rest” in the most incremental way: strict-compile the synthesized bodies (thunk bodies and closure bodies) even when the top-level body must remain interpreted due to boundaries or other non-strict ops.

## Motivation

Right now, strict compilation tends to be “all or nothing per top-level body”. That blocks the main promise of strict fragments: most real code is mixed, especially build-shaped programs where boundaries exist but lots of local computation is still pure and scalar.

The extracted thunk bodies are the natural unit of partial compilation because:

- they already exist as separate `ExecutableBody` units
- they represent “demanded cones” (or at least extracted identity points)
- they are often scalar/pure, making them good strict fragment candidates

## Work

- Prerequisite: 017 (KernelOp-based strict lowering).
- Ensure `lower::lower_executable_program` attempts strict compilation for:
- the entry body
- any additional bodies in the program map, including synthesized body IDs (>= 10000)
- Extend whatever runtime/evaluator path consumes `KernelKind::Strict` so it can execute strict kernels for those bodies.
- Add a regression test fixture where:
- the top-level body contains a runtime boundary (so it must stay interpreted)
- but a sibling extracted thunk is pure scalar and gets strict-compiled

## Acceptance

- Existing tests remain green.
- Demonstrate in logs or a unit test assertion that:
- at least one synthesized body is marked `KernelKind::Strict { .. }`
- the surrounding program still runs and produces the same result

## Notes

This item is not yet “real eval3 lazy core”. It is a practical stepping stone that aligns the implementation with the strict-fragment execution strategy described in `notes/eval/compiler-pipeline.md`.
