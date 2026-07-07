+++
title = "vix — the language"
+++

vix is the language vixen's build logic is written in. It exists because build
systems keep making the same trade badly: either the build language is a real
language (and every build script reinvents caching, wrongly), or it's a
configuration format (and real logic gets exiled to plugins in some other
language, where the build system can't see what it reads).

vix refuses the trade. It is a real language — functions, values, pattern
matching, recursion — whose *evaluation model* is the build system's semantics:

- Calling a function is **demanding** a value; results are memoized by the
  content of their inputs.
- Every computation's **read-set is tracked** as it runs; incremental
  re-evaluation is exact, not approximate.
- Values **freeze** into a content-addressed store when they cross computation
  boundaries, and stay **molten** (fast, mutable, identity-free) inside them.
- Everything the machine does is **observable**: demand traces, per-event
  receipts, profiler-friendly execution.

The consequence: naive vix code has the properties that take heroic effort
elsewhere. The dependency resolver ([rodin](@/constellation/rodin.md)) is
ordinary-looking functional code; memoization, warm restarts, and incremental
re-resolution come from the machine, not from resolver-specific cleverness.

## How it runs

vix does not interpret an AST. Programs lower to [weavy](@/constellation/weavy.md),
the shared execution substrate, which runs them through either a portable
interpreter or a JIT — same semantics, chosen at runtime. Hot loops are real
loops (tail calls compile to jumps), aggregate updates mutate in place when
provably invisible, and the JIT emits real debug info, so a debugger can step
through build logic like any other program.

## What vix is not

vix is not a general-purpose application language and doesn't want to be. There
is no I/O free-for-all — effects like fetching and process execution are
explicit primitives with tracked observations, because "what did this build
read from the world" *is the product*. Parsers, network protocols, and
cryptography belong in Rust behind capabilities; logic whose incrementality
matters belongs in vix. The boundary is precisely: **would you want this
computation's interior to be cached and invalidated exactly? Then it's vix.**
