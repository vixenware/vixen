# 001: Slice 1a — structs

Add struct construction and field access to the evaluator.

```vixen
struct Point { x: U64, y: U64 }

fn main() -> U64 (
  let p = Point { x: 42, y: 99 };
  p.x
)
```

## What's needed

- Evaluator: new `Value::Struct { name, fields: HashMap<String, SharedThunk> }`
- Evaluator: handle `Expr::StructLiteral` — construct with lazy field thunks
- Evaluator: extend `Expr::Field` to resolve named fields (not just `_0`)
- Fixtures: struct construction, field access, dead struct field, nested structs

## Key test

```vixen
struct Pair { fst: U64, snd: U64 }
fn bomb() -> U64 ( trap("boom") )
fn main() -> U64 ( Pair { fst: 42, snd: bomb() }.fst )
```

Dead field on a struct — same honesty test as tuples.

## Parser support

Already exists: `StructDecl`, `StructLiteralExpr`, `FieldExpr` all in vx-parser.
