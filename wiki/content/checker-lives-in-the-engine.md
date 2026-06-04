+++
title = "Checking is forcing; the checker lives in the engine"

[extra]
type = "Doc"
tiers = ["2"]
tags = ["architecture", "type-checking", "runtime", "lsp", "incremental", "streams"]
created = "2026-06-04"
+++

**[settled]** Where the type checker lives, recorded so it isn't re-litigated.
The structural companion to [[Types are values]] (the semantics) and
[The demand-driven runtime](/runtime-semantics/) (the engine). Distilled from the
session that derived it — including the wrong turns in [What this rejects](#what-this-rejects).

## The thesis

**Checking and evaluation are one process, so the checker lives in the engine —
the same crate as the evaluator, not a lower layer that calls it or that it calls.**

[[Types are values]] settles the *semantics*: a check is forcing the assertion a
program makes and emitting a diagnostic if it doesn't hold; type-checking is the
subset of forcing that validates; resolve statically what is statically
resolvable, force what isn't, record *pending* otherwise. This note settles the
*structure* that follows: if checking is forcing, the checker must sit where
forcing lives.

Concretely: `vx-parser` and the IR data types are the pure front-end below
(lex → parse → lower to IR). Everything that **forces** — checking, evaluation,
and eventually dynamic import / name resolution — is the **engine**
(`vx-runtime`). The type checker (`type_check/`, `cli.rs`) belongs in the engine,
beside the evaluator.

## Why — the forces, all future-facing

The grammar-forcing CLI resolver ([[Commands & grammars are values]]) is only the
*first* check that forces. Every direction the system is heading makes the
unification load-bearing, not incidental:

- **Interleaving.** Checking one `@[ ${cmd} … ]` weaves static (structural lints
  on literal tokens) and dynamic (force the grammar value) together, sharing state
  — the hole types, the diagnostic list — *within one expression*. A crate
  boundary through the middle of that forces a two-pass handshake ferrying state
  across. As more checks become value-forcing (the explicit trajectory of
  types-are-values), the boundary cuts through more of them. A fault line that
  keeps moving is the worst kind.

- **Incrementality and caching, for free.** Per-unit diagnostic caching (the
  [continuous cross-target validation](/executor-abilities/#continuous-cross-target-validation-the-lsp-killer-feature)
  feature) is just: diagnostics are forced values in the demand graph, reused by
  read-set (`r[eval.readset]`) like any other forced value. That falls out only if
  checking *is* forcing-in-the-graph. A separate checker grows its own, worse,
  incrementality story.

- **Remote checking.** Cross-target validation runs checks on *remote* executors —
  the same `exec` / observe machinery that already ships observer closures to
  runners ([[Executor abilities]]). The checker must run *where exec runs*: in the
  engine. A lower-crate checker cannot ship itself to an executor; the engine
  already ships closures and pulls results back.

- **Streaming.** Diagnostics are `Stream[Diagnostic]` that update as you type
  ([Streams](/streams-first-primitive/)) — an engine output (forcing producing a
  stream), not a batch pass's return value.

- **One gesture.** Hover, completion, and type-checking are the same act — *force a
  value in the live graph and inspect it*. The LSP is a **client of the live
  engine, not a re-implementation of the checker** ([[Types are values]]). One
  engine knows how to force; the LSP queries it.

## What this rejects

The session reached the thesis by walking into the alternatives. Recorded so they
are not re-walked:

- **The static-trace cheat.** Re-implementing a dumb slice of the evaluator inside
  the checker (follow a binding to a struct literal; give up on a `match` or a
  call). It conflates two different *pending*s — "genuinely needs a runner
  (offline)" vs. "my tracer gave up" — and so marks perfectly forceable pure
  values as pending because the approximation is weak. The checker must force with
  the *real* evaluator.

- **The keyhole `Forcer` trait** (checker stays in `vx-ir`, calls *down* into the
  engine to force, through an injected trait). A callback only makes sense if the
  IR *drives* checking and reaches down to force — backwards. The **engine
  drives** the live check; nothing calls *up*. The trait is ceremony around a
  boundary that should not exist.

- **The two-pass split** (static checks in `vx-ir`, a separate live pass in the
  engine, diagnostics merged). It splits the per-expression interleaving above
  into two passes ferrying shared state, and the split line only grows as more
  checks force.

- **Blast-radius worries about merging.** "It drags the runtime into lean IR
  consumers" is fiction — it is one binary, the tooling *is* `vx`, and it already
  links the runtime. There is no consumer of the checker-without-the-engine to
  protect.

## Migration

Incremental, not a big bang. The CLI-literal grammar resolver is the first checker
piece that needs forcing, so it moves first: bring `infer_cli_literal` (and the
pure `command-model` validators it calls) into the engine, where it can force the
leading hole's grammar value against the live graph. That sets the pattern — the
pure analyses are library functions; the checker that *orchestrates* them
(deciding force / resolve-statically / record-pending) is engine code. The rest of
`type_check/` follows as it earns forcing. The exact crate cut — merge `vx-ir` into
`vx-runtime`, or lift `type_check/` across with IR + lowering staying put — is
taken against the real dependency graph, not guessed.

## Related

- [[Types are values]] — the semantic stance this is the structural consequence of.
- [The demand-driven runtime](/runtime-semantics/) — the engine the checker joins.
- [[Commands & grammars are values]] — the grammar-forcing resolver, the first check to move.
- [[Executor abilities]] — continuous cross-target validation: the feature needing remote + streaming checks.
- [Streams as the first Tier-2 primitive](/streams-first-primitive/) — `Stream[Diagnostic]`.
