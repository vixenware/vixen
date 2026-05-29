# Self-hosting tests & benchmarks (draft)

Draft, non-normative. Decision: Vixen tests and benchmarks are written **in
Vixen**, not in Rust. One tiny generic Rust harness discovers `.vx` test files
and runs each `#[test]` function through the evaluator; every case — and the
assertions — live in `.vx`. This stops ~80% of the Rust being fixtures and
assertions about internal types (`RtValue`, `Thunk`) that churn.

## A test passes iff it evaluates without poisoning

- `#[test] fn name() ( … )` — no-arg; the runner demands its result. Clean
  evaluation = pass; any poison = fail (the Diagnostic is the report).
- `#[test(poisons)] fn name() ( … )` — inverted: the test is *expected* to
  poison (error-path test). Detonation = pass.
- `#[bench] fn name() ( … )` — times forcing the body, and **must run cold**:
  caching makes a warm run instant, so benchmarks bypass the cache to measure
  compute. (Distinct from `#[test]`, which only cares about the value.)

## 💣 — the bomb literal

`💣` is a value of the bottom type (fits any position) that **detonates
(poisons) the instant it is forced**. Drop it into every position demand-driven
evaluation must never reach; if the evaluator over-forces, the bomb goes off and
the test fails loudly — and you can *see* it sitting in the road not taken. The
canonical demand-driven tripwire; replaces `fn boom() -> U64 ( fail "boom" )`.

## Assertions are themselves Vixen

```vixen
fn assert(cond: Bool) -> Bool (
    if cond ( true ) else ( fail "assertion failed" )
)

fn assert_eq[T](got: T, want: T) -> Bool (
    if got == want ( true ) else ( fail "not equal" )
)
```

`fail "msg"` is juxtaposition application — no parens. A generic `assert_eq[T]`
exercises universal `==`.

## Examples (corrected surface)

```vixen
struct Pair { a: U64, b: U64 }
struct Two  { fast: U64, slow: U64 }

// demand-driven basis: the bomb must not go off
#[test]
fn unused_tuple_field_never_runs() (
    let pair = (42, 💣);
    assert_eq(pair[0], 42)            // tuple field = positional [index], not ._0
)

#[test]
fn unused_struct_field_never_runs() (
    let s = Pair { a: 42, b: 💣 };
    assert_eq(s.a, 42)                // named field = declared member, .a
)

// which field you demand IS the graph
#[test]
fn demanding_fast_skips_slow() (
    let r = Two { fast: 7, slow: 💣 };
    assert_eq(r.fast, 7)
)

// poison + recovery (forces the error surface)
#[test(poisons)]
fn divide_by_zero_detonates() (
    divide(1, 0)
)

#[test]
fn divide_by_zero_recovers_to_err() (
    assert(divide?(1, 0) is Err)      // ? is a NAME suffix: divide? is the Result-returning variant
)
```

## Open / carried for the next pass

- **`?` → `Result` shape.** `f?` lifts `f`'s poison into a `Result`; the `Err`
  is inspectable (`is Err`). How structured the error payload is — undecided.
  Poison is **not** a return value (not "empty tuple is an error"): it is the
  orthogonal failure channel. `divide : Fn(U64,U64) -> U64` poisons on `/0`;
  `divide?` lifts that into `Result`.
- **`?` is strictly a name suffix** (`divide?`, `tree.file?`, `parse_u64?`),
  never on an arbitrary expression. (Reconciles with `r[error.option]`.)
- **Tuple fields are `[index]`** (`pair[0]`), not `._0`. Refines the `.`-vs-`[ ]`
  rule (named member → `.`, positional → `[ ]`). Recorded in `query-surface.md`.
- **Negative-test mechanism:** `#[test(poisons)]` for whole-test failure; `f?` +
  `is Err` for asserting a specific error mid-computation.
- **Harness:** one generic Rust test globs `tests/lang/*.vx`, prepends the
  assert prelude, runs each `#[test]` fn via `eval_source`, pass = a non-poison
  result. Groups A (demand basis) and E (which-field-which-graph) run against
  today's evaluator; C (composite `==`) and D (streams + `Stream→Array`
  coercion) define the next increment; B (poison/`Result`) can't be written
  until `?`→`Result` + `#[test(poisons)]` are committed.
