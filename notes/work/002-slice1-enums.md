# 002: Slice 1b — enums and match

Add enum construction and pattern matching to the evaluator.

```vixen
enum Option {
  struct Some { val: U64 }
  struct None {}
}

fn unwrap_or(opt: Option, default: U64) -> U64 (
  match opt (
    Some { val } => val,
    None {} => default,
  )
)
```

## What's needed

- Evaluator: new `Value::Variant { enum_name, variant, fields: HashMap<String, SharedThunk> }`
- Evaluator: handle variant construction (looks like a struct literal with enum prefix)
- Evaluator: handle `Expr::Match` — evaluate scrutinee, match patterns, bind captures
- Evaluator: pattern matching logic — struct patterns, variant patterns, wildcards
- Fixtures: enum construction, match, nested match, dead branches in match

## Key test

```vixen
fn main() -> U64 (
  let opt = Some { val: 42 };
  match opt (
    Some { val } => val,
    None {} => bomb(),
  )
)
```

Match only enters the taken branch — `bomb()` never called.

## Parser support

Already exists: `EnumDecl`, `MatchExpr`, `MatchArm`, `VariantPattern`,
`StructPattern` all in vx-parser.
