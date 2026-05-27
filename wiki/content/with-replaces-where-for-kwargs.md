+++
title = "with replaces where for kwargs syntax"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["language", "syntax"]
+++

## Context

Vixen function signatures separate positional arguments from named arguments with defaults. The block following the positional list declares named bindings:

```
fn exec[T](
  args: Array[ExecInput]
) <KEYWORD> {
  env: Map[String, ExecInput] = %{},
  observer: Fn(Process, Tree) #sandbox -> T,
} -> T;
```

The keyword used today is `where`. Real-world readers find it confusing — Rust uses `where` for generic bounds (`fn foo<T>() where T: Clone`), and the reuse of the keyword with a different meaning in Vixen creates a constant pump of "wait, what is this `where` doing?" cognitive overhead.

## Options considered

1. **Keep `where`.** Document the difference; lean on LSP hover to clarify role. Cost: indefinite low-grade reader confusion.
2. **`with { ... }`** — reads as natural English ("exec args, with these named bindings").
3. **`opts { ... }`** — short, explicit; reads as "options."
4. **Trailing destructured struct in the parameter list** — no new keyword, brace inside the paren list. Adds parser ambiguity; visually busy.
5. **`named { ... }`** — maximally self-explanatory; verbose, slightly schoolmarmish.
6. **`@ { ... }`** — symbolic. Conflict: `@` is already heavily used for paths in Vixen.

## Decision

Replace `where` with `with`.

```
fn exec[T](
  args: Array[ExecInput]
) with {
  env: Map[String, ExecInput] = %{},
  observer: Fn(Process, Tree) #sandbox -> T,
} -> T;
```

## Consequences

- **Reading sentence-wise becomes natural.** "exec args, with env and observer." No conflict with Rust readers' expectations.
- **One keyword change in the parser and across the stdlib.** Mechanical migration: `where` → `with` everywhere it appears in function signatures. `core.vx` and `build.vx` get updated; existing tests pick up the new syntax.
- **`with` is now reserved** as a keyword and cannot be used as an identifier. Loss is small — `with` rarely shows up as a Vixen identifier in the existing code surveys.
- **No remaining conflict with Rust precedent.** Rust has no `with` syntax in function signatures; Vixen's usage is unambiguous.

## Related

- [[Rust/Vixen FFI shape]] — the stdlib that uses this syntax.
- [[Domain logic lives in Vixen, not in the runtime]] — Vixen packages will adopt the new syntax as it ships.
