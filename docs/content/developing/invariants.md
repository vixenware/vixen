+++
title = "The invariants"
+++

A few properties of the machine cannot be recovered once lost — they aren't
features you can regress and re-add, they're load-bearing structure that
everything above assumes. Contributors need to know them because the test
suite *will* enforce them, and it helps to understand why the test that failed
exists.

**Demand-drivenness is the big one.** Nothing computes unless transitively
demanded. The memo, read-sets, and incremental re-evaluation are all built on
that assumption — an eager path added "for simplicity" quietly breaks
invalidation in ways results-based tests can't see, because *an eager
implementation produces the same answers*. So the machine's tests assert on
the **trace**: warm demands spawn nothing; undemanded functions never appear;
untaken match arms never execute. If your change makes one of those
assertions fail, the corpus being green doesn't save it — the trace is the
spec.

**Reuse must be invisible.** In-place mutation of molten values is legal
exactly when no observer can tell. The enforcement is a differential oracle:
every aggregate-update path can be forced down the copy path, and results,
traces, and stores must match bit-for-bit against the reuse path. New
update operations join that oracle or don't merge.

**Lane equivalence.** The interpreter and the JIT must produce identical
observable behavior — same results, same demand traces. Tests run both lanes;
"works in interp" is half a feature.

**Identity is content, only content.** Nothing allocation-ordered,
process-local, or positional may leak into a content hash or memo key. The
type-identity layer (taxon) and the store both assume two processes agree
without coordination; one sneaky pointer-derived tiebreak breaks distributed
caching in ways that surface months later.

The meta-invariant: **guards are added before the code they guard.** When a
change touches demand-sensitive machinery, the discipline is to land the
tripwire test first — red or vacuous — then the change. It's slower by an
hour and faster by a debugging week.
