+++
title = "Diagnostics"
weight = 7
+++

vix's diagnostic philosophy in one line: **observability instead of
annotation.** Where other languages ask you to mark things (`inline`,
`tailcall`, uniqueness annotations) to get behavior, vix infers the behavior
and *tells you* when inference didn't go the way a hot path wants.

The two performance diagnostics you'll actually meet:

**"self-call is a demand boundary (not tail position) — not looped."** You
wrote recursion the compiler couldn't turn into a jump. For tree recursion
this is correct and ignorable; for an intended loop it names the exact call
site to restructure. See [Loops](@/vix/loops.md).

**Reuse declined.** An aggregate update copied instead of mutating, because an
alias existed at that point. Again: semantically fine, always — but if it's
inside your hot loop, this diagnostic is the difference between profiling for
an afternoon and reading one line. See [Molten values](@/vix/molten.md).

Both share a principle: the *fast path is inferred, the fallback is safe, and
falling back is visible on request*. Silent performance cliffs are treated as
bugs in the language, not costs of doing business.

## Checking

The checker verifies what it can before the machine runs: schema agreement at
calls and constructions, pattern coverage over known shapes, field access
against declarations. It is deliberately conservative and young — its
error-message quality is an active workstream, and the working standard is
Rust-grade: an error should name the place, the expectation, the reality, and
when possible the fix.

## Runtime failures

When build logic fails at runtime — a coercion from dynamic data doesn't fit,
an external process exits nonzero, a fetch hash mismatches — the failure is a
*value in the trace* like everything else: attributable to the demand that
produced it, with its inputs' identities attached. The debugging move is the
same as every other question about a build: read the receipts.
