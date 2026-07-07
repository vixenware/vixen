+++
title = "What happens to my CI?"
weight = 3
+++

The slow-CI story, end to end. Today's CI reality for most Rust teams: every
PR builds from a cold-ish cache, reruns the full test suite, and spends its
minutes proving that unchanged things are unchanged. Every mitigation —
caching action snapshots, test sharding, "skip CI" labels — trades away either
correctness or confidence.

vixen's CI story rests on the two mechanisms this site keeps pointing at:

**Builds shrink because identity is content.** A PR that touches three files
recompiles what those items affect — the wave stops at the first unchanged
interface, and everything else is a cache hit *that survives across runners
and branches*, because a content hash means the same thing everywhere.
Timestamp-based caching can't give you this; it doesn't know what "same"
means.

**Test runs shrink because selection is computed, soundly.** For each test,
vixen knows — from the compiled artifacts' own reference graphs — whether the
change can possibly reach it. Tests that provably can't be affected are
skipped, *with receipts*. Two properties make this different from
coverage-based selection you may have been burned by:

- The failure direction is safe: over-approximation reruns too much, never
  too little.
- There's an **audit mode**: periodically run everything anyway and verify
  the skipped tests would have passed. Trust, then verify, continuously —
  see [Test selection](/using/test-selection/) for the mechanism and
  [Proof](/proof/) for the demonstration.

**What a green run means changes.** A vixen-green PR is "everything this
change could affect was built and tested, and here's the trace." When someone
asks "did CI actually test the thing I changed?" — the
[receipt](/using/receipts/) answers, structurally.

## The honest part

Early access is **single-machine first**: today's win is that one warm runner
does dramatically less work per PR. The shared-cache/multi-runner story —
where your laptop's build and CI's build are literally the same cache entries
— is what the architecture is built for (content addressing makes it
arithmetic), but it ships after the local story is boring.
[Status](/status/) tracks the line.
