# Build Diagnostics

This note captures the intended shape of `build.vx` diagnostics.

The goal is not to improve the current plain-text error strings incrementally.
The goal is to make build failures render like real source diagnostics, with
syntax-highlighted snippets, labeled spans, and demand-chain context.

## Goals

1. Show the failing source expression, not just a low-level runtime string.
2. Show the resolved runtime facts that matter:
   - resolved path
   - relevant workspace root
   - OS / runtime error
3. Collapse duplicate causes aggressively.
4. Show demand-chain frames as source-backed context, not `invoke(..)` / `call(..)`.
5. Use the existing tree-sitter grammar for syntax highlighting rather than
   inventing another formatting path.

## Rendering Stack

Use:

1. `ariadne` for primary diagnostics:
   - file path
   - line numbers
   - labeled spans
   - notes / help text

2. `arborium` for syntax highlighting of source snippets:
   - same code slices rendered inside diagnostics
   - uses the existing Vixen tree-sitter grammar

The end result should feel like a source diagnostic first, and a runtime error
second.

## Primary Diagnostic Shape

The first report should be the actual failing expression.

Example:

```text
error: build input not found
  --> zoo/lua.vx:37:23
   |
34 |   let class = executor_class("default");
35 |   let src_tree =
36 |     Tree(
37 |       ws / @zoo/lua-src
   |            ^^^^^^^^^^^^ this path does not exist in the current workspace
38 |     );
39 |
40 |   let toolchain =
   |
   = resolved path: /Users/amos/bearcove/vixen/zoo/zoo/lua-src
   = workspace root: /Users/amos/bearcove/vixen/zoo
   = os error: No such file or directory
```

Properties:

1. The highlighted span is the relevant subexpression, not just the whole line.
2. The resolved path is always shown.
3. The workspace root is shown only when it is actually relevant to the failing
   path construction.
4. Duplicate identical causes should be collapsed before rendering.

## Demand-Chain Context

The current demand chain output:

```text
demand chain:
  -> invoke(..)
  -> call(..)
```

is not acceptable as a finished product.

Instead, each demand-chain frame should be rendered as another source-backed
snippet when source is available.

Example:

```text
while evaluating build entry
  --> zoo/lua.vx:36:5
   |
33 |   let cflags = ["-Wall", "-O2", "-std=c99"];
34 |   let class = executor_class("default");
35 |   let src_tree =
36 |     Tree(
   |     ^^^^ needed to construct the source tree for this build
37 |       ws / @zoo/lua-src
38 |     );

required by `compile`
  --> zoo/lua.vx:64:15
   |
61 |   fn compile(file: Path) -> Tree (
62 |     cexec [
63 |       cc,
64 |       "-c",
   |               ^^^^^ compile reads source files from `src_tree`
65 |       ..cflags,
66 |       "-o",

required by `objects`
  --> zoo/lua.vx:91:18
   |
88 |   fn link(objs: Tree) -> Tree (
89 |     cexec [cc, "-o", @out/bin/lua, ..ldflags, ..objs, ..syslibs]
90 |   );
91 |   let objects = (all_lib_sources.map(compile)).merge();
   |                  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ source compilation depends on `src_tree`
```

The exact labels will vary by failure kind, but the structure should remain:

1. primary source failure
2. source-backed demand-chain frames underneath
3. runtime facts as notes, not as the whole diagnostic

## Hints

Hints should be conditional, not automatic.

Examples of acceptable hints:

1. A path built from `ws` resolves to an obviously doubled workspace path.
2. A `Tree(...)` call points outside the current workspace unexpectedly.
3. A common build pattern is missing a generated or acquired input.

Hints should not appear by default when the error is already self-explanatory.

## Relevant Context To Surface

Depending on the failure kind, useful structured notes include:

1. resolved path
2. workspace root
3. mount path inside an `exec`
4. observer expression
5. executor class / discover context
6. OS error text
7. process exit code / stderr summary

These are notes under the source diagnostic, not replacements for it.

## Expected Supporting Work

To make this possible, the evaluator / lowering / runtime need to preserve
enough source information to map runtime failures back to source spans.

That likely means:

1. carrying source spans through lowering for relevant expressions
2. attaching source-bearing frames to demand-chain entries
3. rendering those frames via `ariadne`
4. syntax-highlighting code slices via `arborium`

## Non-Goals

1. Do not settle for better ad hoc string concatenation.
2. Do not show only runtime stack frames without source.
3. Do not print hints unconditionally.
4. Do not duplicate the same underlying cause multiple times.

## Product Standard

The finished product should feel like:

1. a compiler diagnostic with real source context
2. plus runtime facts
3. plus a source-backed explanation of why evaluation reached that point

It should not feel like a debug dump from the evaluator.
