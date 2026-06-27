# 004: Direct And Recursive Kernel Calls In Weavy

Status: Needs rewrite for the Weavy direction. The old `vx-jit` callable API is
gone; direct and recursive calls remain relevant, but should be modeled with
Weavy blocks, block calls, and continuations.

The kernel lowering path must eventually handle calls inside eager pure islands,
not just leaf expressions. Adding direct call support would let the Weavy-backed
kernel path execute:

- `fn f(x) ( g(x) + 1 )` — non-recursive cross-function calls
- `fn fib(n) ( if n == 0 (0) else (fib(n-1) + fib(n-2)) )` — recursion

## What's needed in the Weavy-backed path

- A graph/runtime rule for when a call can stay inside an eager kernel.
- Lowered Weavy block structure for non-recursive and recursive calls.
- A dense block/call representation that the interpreter and copy-and-patch JIT
  both consume.
- A native call/continuation story that follows Weavy's JIT surface instead of a
  Vixen-only callable registry.
- Oracle tests that compare graph execution, Weavy interpretation, and native
  execution for the same call-heavy kernel.

## Impact

This is required for call-heavy pure interiors to benefit from the same kernel
path as straight-line arithmetic/string work. Without it, the runtime can still
lower leaf kernels, but recursive and mutually recursive pure kernels remain
graph/interpreter-only.

## Status

Parked until the graph-runtime-to-Weavy lowering path exists.
