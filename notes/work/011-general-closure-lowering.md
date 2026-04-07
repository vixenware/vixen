# 011: General closure lowering

The exec_slice closure lowering only supports `(x) => x + captured` — a
single-param closure whose body is a binary add with one captured binding.

## What needs to change

- Zero-arg callable apply (fixture 11: `let f = (x) => x + n; f(5)` where
  `f` is used as a local closure, not via ApplyCallable)
- General closure bodies: function calls, nested closures, arbitrary
  expressions (fixtures 18, 21)
- Higher-order function parameters: passing a top-level function by name
  (fixture 16 — `apply(double, 21)` where `double` resolves to a body, not
  a local binding)

## Fixtures blocked

- 11_closure_capture — zero-arg callable
- 16_higher_order — MissingBindingForUse for fn passed as arg
- 18_compose — `(x) => g(f(x))` closure body
- 21_closure_returns_closure — `(x) => (y) => x + y`

## Depends on

007 (demand-driven runtime — done)

## Status

Not started.
