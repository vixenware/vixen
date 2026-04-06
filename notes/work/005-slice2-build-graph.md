# 005: Slice 2 — build graph

The real vixen use case: `exec()`, `discover()`, sandboxed processes,
content-addressed caching, the VFS.

Far future. Depends on everything else being solid.

## What it looks like

```vixen
fn build() -> Tree (
  let src = discover(@src);
  let obj = exec(%{ command: "cc", args: ["-c", src] });
  exec(%{ command: "ld", args: [obj] })
)
```

## What's needed

- Runtime boundary handling in the evaluator
- VFS / sandbox integration
- Content-addressed store
- The full `ProgramEvaluator` path, not the slice 0 AST walker

## Status

Not started. This is the end goal but there's a lot of foundation
to lay first.
