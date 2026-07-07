+++
title = "Diagnostics"
weight = 11

[extra]
updated = "2026-07-08"
+++

vix's diagnostic philosophy in one line: **observability instead of
annotation.** Where other languages ask you to mark things (`inline`,
`tailcall`, uniqueness annotations) to get a particular behavior, vix infers
the behavior from your code as written and *tells you* when the inference
didn't go the way a hot path wants — rather than requiring you to have
predicted that ahead of time.

## The two performance diagnostics you'll actually meet

**"self-call is a demand boundary (not tail position) — not looped."** You
wrote recursion the compiler couldn't turn into a backward jump. For tree
recursion this is correct and safely ignorable — memoization is what you
want there. For an intended loop, it names the exact call site to
restructure into tail position. See [Loops](/vix/loops/).

**Reuse declined.** An aggregate update copied instead of mutating in place,
because an alias existed at that point (or a value had already been read
more than once this execution — see the honest edges in [Molten
values](/vix/molten/)). Semantically fine, always — but if it's inside your
hot loop, this diagnostic is the difference between profiling for an
afternoon and reading one line telling you exactly where and why.

Both diagnostics share a principle: **the fast path is inferred, the
fallback is always safe, and falling back is visible on request.** A silent
performance cliff is treated as a bug in the language's observability, not an
accepted cost of doing business.

## Checking, before the machine runs

The checker verifies what it can before evaluation starts: schema agreement
at calls and constructions, pattern coverage over known variant sets, field
access against declarations. It is deliberately conservative and still
young — error-message quality is an active workstream, and the working
standard is Rust-grade: an error should name the place, the expectation, the
reality, and, when possible, the fix. See [Pattern matching](/vix/patterns/)
for what "exhaustive" means concretely.

## Runtime failures are values in the trace

When build logic fails at runtime — a `Doc` coercion from dynamic data
doesn't fit the requested shape, an external process exits nonzero, a
`fetch` hash mismatches — the failure is a *value in the demand trace* like
everything else the machine produces: attributable to the exact demand that
produced it, with its inputs' content identities attached. The debugging
move is the same as every other question you'd ask about a build: read the
receipts. There's no separate failure-reporting side channel, and there's no
print-debugging escape hatch — see [Demand semantics](/vix/demand/) for why
purity makes that a structural property rather than a style rule, and why
the observability stack (demand traces, per-event receipts) exists
specifically to make that non-negotiable choice livable.
