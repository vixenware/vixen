# 004: vx-jit direct function calls

Currently blocked: vx-jit is in flux.

The strict lowerer can only compile leaf functions (no calls to other
functions). Adding direct call support would let vx-jit compile:

- `fn f(x) ( g(x) + 1 )` — non-recursive cross-function calls
- `fn fib(n) ( if n == 0 (0) else (fib(n-1) + fib(n-2)) )` — recursion

## What's needed in vx-jit

- `VixenCallableRef::Self` or similar for recursive calls
- Register the compiled function's own address as a callable
- Or: compile all functions in a module together (mutual recursion)

## Impact

Would make `fib(10)` go from 0.4s to microseconds. Currently the
evaluator falls back to AST-walking for any function containing calls.

## Status

Parked until vx-jit stabilizes.
