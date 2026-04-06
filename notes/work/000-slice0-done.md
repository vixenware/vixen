# 000: Slice 0 — pure computation (DONE)

Literals, let bindings, binary ops, function calls, if/else, closures,
tuples with lazy fields, field projection, thunk caching.

28 datatest fixtures in `crates/vx-eval-tests/fixtures/slice0/`.

Key result: demand-driven evaluation proven — `(42, bomb())._0` returns 42
without entering `bomb()`.

Hybrid evaluator: strict functions compile via Kajit, lazy scaffolding
stays interpreted. Extended strict lowerer with Sub, Mul, Ne.

Limitation: Kajit can't compile function calls yet (only method calls),
so recursive functions still AST-walk.
