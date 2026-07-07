+++
title = "The method"
weight = 4
+++

How work lands, distilled from practice. This is descriptive of how the
project actually operates — including its heavy use of AI agents doing
implementation under human architectural direction — and prescriptive for
contributions of any authorship.

**Work happens on branches; a gate folds it.** Nothing reaches the main line
without a review pass that is *adversarial by role*: verify the base isn't
stale, read the diff for scope (every touched file explainable by the
mission), run the relevant suites independently rather than trusting the
report, and read — actually read — any hunk touching invariant-adjacent
machinery. Green tests are necessary, never sufficient.

**Claims carry evidence.** "It's faster" comes with numbers and the profile;
"it matches cargo" comes with the differential run; "it's equivalent" comes
with the oracle output. A report without its evidence gets sent back for the
evidence, and the requirement is the same whether the author is a person or
an agent.

**Stage the risky thing behind its guard.** Tripwire first, change second
(see [Invariants](/developing/invariants/)). Big migrations land as
staged commits on one branch — each stage green — so the gate can bisect
by reading.

**Findings beat workarounds.** An implementation that hits an unexpected
obstacle and *stops with a precise description* has succeeded; one that
contorts around it (vendoring a patched dependency, stubbing a boundary,
weakening a test) has failed twice. The project's history is full of
mid-mission findings that redirected design — that only works when stopping
is rewarded.

**Everything pushes.** Branches push when work pauses, reports reference
commits, and learnings fold back into the shared record even when code
doesn't. Assume any machine can vanish; committed-but-local is not done.

**Never rewrite history; never revert work.** Failures stay on their
branches as evidence. The negative result is a result.
