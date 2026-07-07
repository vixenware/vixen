+++
title = "Test selection"
+++

The question every CI pipeline mumbles about: *a change came in — which tests
must rerun?* The industry's answers are "all of them" (correct, wasteful),
timestamp/target graphs (file-granular, over-invalidating), or coverage-based
selection (statistical, unsound — it skips tests that would have failed).

vixen's answer is computed from the artifacts themselves, and it is designed
to be **sound**: it never skips a test the change could have affected. The
mechanism is a lattice of two read-sets:

**Predicted (static).** A test binary's object files contain, in their
relocation tables, the true reference graph: which function refers to which
function, which data, which vtable. Walking that graph from each test's entry
gives everything the test *could* touch — and hashing each function's
normalized body (position-independent, the way linkers do for identical-code
folding) gives change detection at *function* granularity. Bump a dependency:
tests whose reachable set contains no changed function hash are provably
unaffected. This is cheap enough to run always, requires no instrumentation,
and its failure mode is the safe one (over-approximation).

**Observed (dynamic).** Runs of the test record what it actually executed and
read — tighter than reachability (framework code is *reachable* from
everything but *executed* by little). Observed sets refine predicted ones
where the over-approximation hurts.

The two check each other: an observed read outside the predicted set would
mean the static analysis missed an edge — that's an assertion, continuously
verified, not a hope. And the whole mechanism is periodically audited the
honest way: run everything anyway, confirm the skipped tests would have
passed.

The pieces here have been validated on real object files (the motivating
scenario — "half my tests depend on this crate, it changed, skip the other
half" — works, monomorphized generics and all); productization into the build
flow is tracked on [Status](@/status/_index.md). Granularity note: this
composes with [item-granular invalidation](@/concepts/read-sets.md) — a
comment-only change never even reaches test selection, because nothing's
content changed.
