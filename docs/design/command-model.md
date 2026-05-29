# Command model — every exec is a described, canonical command (draft)

Draft design material, **not normative spec**. Captures a decided direction
(full grammar; every exec understood) and frames the open questions under it.

## Thesis (decided)

The number-one job of a build system is running commands. So Vixen understands
*every* command it runs: each `exec` is described by a tool **grammar**,
constructed or parsed against it, and reduced to a **canonical** form.
Understanding the command is not decoration — it is what the rest of the system
is built on:

- **Caching.** The cache key is the *canonical* command, not the literal argv.
  `[cc, "-O2", "-Wall"]` and `[cc, "-Wall", "-O2"]` are one cache entry.
  Canonical alignment is a **correctness** requirement for a sound cache, not a
  nicety — it's the first reason this exists.
- **Linting.** The same pass that canonicalizes warns for free: `-O0`
  overridden by `-O2`, duplicate `-DFOO`, conflicting flags.
- **Observability / provenance.** "What actually ran" is structured data —
  inputs, outputs, flags-by-meaning — not an opaque string.

Settled: full grammar, full canonical formatting, **both directions** (parse an
argv → structure; construct → canonical argv).

## Power level: PEG-class

A flat flag table cannot describe a real CLI. It collapses on:

- **subcommands** — `cargo build …` vs `cargo test …`, different flag sets;
- **sub-argument languages** — `-Wl,--version-script=foo,--gc-sections` passes
  a comma-list *through* to the linker's own grammar (grammars nest);
- **mutually-exclusive groups, positional / order-significant sequences** —
  link order, `--` end-of-flags, `-framework Foo` consuming the next token;
- **spelling variants** — `-O2` / `-O 2`, `--target=x` / `--target x`.

That is sequence / ordered-choice / repetition / recursion — **PEG-class power**
(the power, not necessarily the syntax). On top of recognition, productions
carry **canonicalization semantics** so the grammar runs both directions: parse
argv ↔ emit canonical argv.

## The escape hatch is impure and punished

Some command will have no grammar (a one-off tool, something not yet described).
A raw, undescribed command is *allowed* — but it is an explicit **impurity
confession**: the author states this exec is opaque, the runtime cannot
canonicalize or understand it, and it forfeits what understanding buys (clean
caching, lint, structured provenance). It is deliberately onerous to reach for;
the expected path is always a described command. Exact spelling is an open
question, but the bar is "you have to work for it and admit what you're doing."

## Open questions

- **Grammar formalism.** PEG proper, or a recursive typed command *model* with
  ordered-choice semantics? How do canonicalization rules attach to productions
  — a small policy algebra (`last-wins` / `dedup-unordered` / `dedup-ordered` /
  `positional`) hung on grammar nodes?
- **`exec` API.** `exec` stops taking an opaque `Array[ExecInput]`. What does it
  take — a `Command` built against a `Tool` grammar? How does a package register
  a grammar, and how does a `Command` get built ergonomically?
- **Impurity spelling.** How is the raw escape hatch written and marked, and
  what exactly does it forfeit — uncacheable, cached-but-flagged, or gated
  behind a capability-like marker the way authority is?
- **Where grammars live.** Runtime supplies the mechanism (grammar types,
  parser, canonicalizer, linter); packages supply each tool's grammar
  (`@vixen/c` → cc/clang/msvc/ar, `@vixen/rust` → rustc). Bootstrapping order?
- **Spec touch-point.** `r[eval.memoization]` currently keys `exec` on `args`;
  it must key on the *canonical* command. Update once the model lands.

## Validation corpus: libpng

`old-zoo/libpng.vx` is the forcing function — `-I` / `-D` / `-O` / arch flags /
`-Wl,--version-script=` / positional objects / feature-probe snippets. The `cc`
grammar must canonicalize all of it. Build the `cc` grammar against these real
flags *first*, then generalize the formalism — don't design the cathedral in the
abstract.
