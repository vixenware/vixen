# 000: VIR Data Structures (vx-vir crate)

Status: Not started.

## Goal

Define the Vixen Intermediate Representation — the typed, scoped, identified IR
that replaces the bridge and becomes the single source of truth between parsing
and lowering.

Separate crate: `vx-vir`. No logic, just types. Thorough code comments on every
type, field, and variant.

## Core Types

```
Body {
    id: BodyId,
    owner: BodyOwner,         // Fn / Closure / Thunk
    parent: Option<BodyId>,   // lexical parent for capture analysis
    name: Option<String>,
    params: Vec<BindingId>,
    root: ExprId,             // body expression
    source: SourceSpan,       // for diagnostics only
}

Expr {
    id: ExprId,
    kind: ExprKind,
    source: SourceSpan,       // metadata, never a lookup key
}

ExprKind — variants map ~1:1 to language constructs:
    Literal { kind: LiteralKind, value: LiteralValue }
    NameRef { name: String, resolved: Option<DefTarget> }
    Binary { op: BinOp, lhs: ExprId, rhs: ExprId }
    Call { callee: ExprId, args: Vec<ExprId>, type_args: Vec<Ty> }
    Field { receiver: ExprId, field: String }
    Closure { body: BodyId }
    Block { stmts: Vec<StmtId>, tail: Option<ExprId> }
    If { condition: ExprId, then: ExprId, else_: Option<ExprId> }
    Match { scrutinee: ExprId, arms: Vec<MatchArm> }
    Tuple { elements: Vec<ExprId> }
    Array { elements: Vec<ExprId> }
    StructLiteral { name: String, fields: Vec<(String, ExprId)> }
    BuiltinCall { builtin: Builtin, args: Vec<ExprId> }
    RuntimeBoundary { kind: RuntimeBoundaryKind, ... }
    ... etc

Binding {
    id: BindingId,
    name: String,
    kind: BindingKind,        // Parameter / Let / Implicit
    body: BodyId,             // owning scope
    init: Option<ExprId>,     // for let bindings
    source: SourceSpan,
}
```

## IDs

Arena-indexed monotonic u32. Allocated during CST -> VIR walk. Not hashes.
- ExprId(u32)
- BodyId(u32)
- BindingId(u32)
- StmtId(u32)

## Open Questions

- How do multi-source programs work? Per-source arenas or global?
- Where do type annotations live? On bindings? On expressions?
- Do we need a PatternId for match patterns?
- How does the import graph integrate?

## Depends On

Research from 007 (KernelOp/slots/multi-file) should inform multi-source design.
