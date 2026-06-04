+++
title = "Types are values; checking is best-effort"

[extra]
type = "Doc"
tiers = ["2"]
tags = ["types", "type-checking", "exec", "grammar", "lsp"]
+++

**[superseded]** The motivating example here — "the grammar is a runtime value
the program *computes*, so types are values" — turned out wrong, and so did the
generalization. A command (a grammar) is a **constant**, not a forced runtime
value. Build code is written against a capability **spec**, and the concrete
tool is **injected** at dispatch — dependency injection. The compiler reads a
command by **const-evaluating `tool.command`**, which succeeds because the
command is provider-independent; there is no "type that is a runtime value," and
the "best-effort because we might lack a runner" framing is gone (the command is
static and offline-available). The settled model is the build spec —
`r[exec.command]`, `r[exec.validate.static]`/`r[exec.validate.runtime]`,
`r[ability.*]` — and [[The checker lives in the engine]]. This note is kept only for the
reasoning trail; **do not build on it.**

---

*(original below, for the record)*

This was the type-system stance behind [the exec primitive](/exec/) and
[Process as a first-class value](/process-as-first-class-value/). It started as
"how does `@[ ${cmd} … ]` find its grammar," and generalized.

## The forcing example

A CLI literal is typed by a **grammar** — the structure of a tool's command line
(which flags exist, which take values, which positions are inputs/outputs). The
grammar classifies the literal's arguments; that is a *typing* job.

But a grammar is not a compile-time fact. A `Command` is built by **probing** —
the capability runs the tool, reads its version, and *computes* the grammar that
applies (a tool's CLI can change between versions). So the thing that types the
command's arguments is a value the program **computes at runtime**.

A static type cannot hold a probed/computed grammar. A **value** can. And since
that value's job is to classify (to type) the arguments, the conclusion is forced:

> The thing that types the arguments is a runtime value. Therefore types are
> values.

`@[ ${self.cc} … ]` resolves its grammar by **projecting `self.cc`'s grammar
value** — an ordinary demand-driven projection — not by a static type lookup. The
old `@grammar[ … ]` import form (grammar known by import) is the special case
where that value happens to be statically available.

## General, not an exception

We considered scoping this to grammars and keeping the rest of the type system
static. We chose **general**. The reasoning:

- **Carving out an exception is more work than accepting the rule.** A static
  type system with one dynamic escape hatch has to special-case the hatch
  everywhere it touches (checking, errors, tooling). Accepting "a type may be a
  value" once, uniformly, is simpler than a branch that says "…except here."
- **Vixen is already dynamic.** "No static analysis in a JIT'd language — a
  snapshot of a moving picture"; imports resolve and packages download mid-run;
  exec outputs have runtime shape. A purely static type layer was already a
  fiction we kept patching. Types-as-values is honest about what the language is.
- **Not every case resolves aesthetically, and that's fine.** Some types are
  statically known (a struct's fields, an enum's variants, a function's
  signature) and check eagerly; some are runtime values (a grammar, an exec
  output's shape) and check when available. We don't force a clean static story
  onto the genuinely-dynamic parts.

The cost we accept: the checker can no longer be *certain* of everything purely
statically. That cost is paid only where a type is genuinely a runtime value —
elsewhere, static types still check eagerly with full guarantees.

## Best-effort checking

The checker's contract changes from "prove everything or error" to:

- Resolve statically what is statically resolvable. Eagerly-known types check
  exactly as before — full errors, full guarantees.
- Where a type is a runtime value the checker **cannot yet force**, it does **not
  error** — it records a *pending* check, resolved when the value becomes
  available (forcing it in the demand graph).
- It is **most complete when connected** (access to the daemon / runners, so it
  can force probes). Offline, or with no runners, some checks stay pending and
  some things go unchecked. **That is acceptable** — best-effort, strongest with
  access, never *wrong*, only sometimes *incomplete*.

## The LSP is a client of the live engine

A dynamically-discovered type and an IDE are not in tension once the LSP is a
**client of the running engine** rather than a re-implementation of the checker
(see the LSP → daemon-over-vox direction). To type a `@[ ${self.cc} … ]`, the LSP
asks the engine "what is `self.cc`'s grammar right now?" — and the engine has
already computed it as part of the build's demand graph.

**Hover-to-evaluate and grammar-completion are the same gesture**: force a value
in the live graph, show the result. A dynamically-discovered grammar is just
another forced value. Before the vox migration, the in-process LSP forces it
through `vx-runtime` directly — same thing, no hop. Offline, it shows what it can
and leaves the rest pending.

## First instance: the CLI-literal grammar resolver

The concrete realization, and the first thing built on this stance:

1. `Command` is a **value** bundling value-level `{ grammar, binary, fingerprint,
   exemptions }`. A capability *constructs* `Command` values (advertising a
   capability is defining its commands); `self.cc` is a field holding one.
2. `infer_cli_literal` (type-check of `@[ ${cmd} … ]`) **projects the grammar**
   from the leading hole's value, then runs the existing structural +
   per-hole-position validation against it.
3. When the grammar value is forceable in the snapshot, that validation is a
   compile-time check; when it isn't, it's a pending check — best-effort, by the
   rule above, with **no exception branch** for "this is the dynamic one."
