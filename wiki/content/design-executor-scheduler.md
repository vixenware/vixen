+++
title = "Design the executor scheduler"

[extra]
type = "Task"
status = "todo"
priority = "high"
tiers = ["2", "3"]
tags = ["scheduler", "runtime", "executor"]
created = "2026-05-26"
+++

When `@vixen/cargo` (or any Vixen build code) writes `exec` with `executor_class "default"`, the runtime's scheduler decides where each call actually runs. The decision weighs availability, online status, customer tier, historical data, and user-supplied constraints. The scheduler is supposed to be smart; the user is supposed to be able to constrain it.

The detailed shape is open.

## What needs deciding

- **Inputs to the decision.** What signals does the scheduler get? Build-graph node identity, expected duration estimate, the cache-state of upstream nodes, current fleet load, last-known executor for this node, customer's tier, customer's explicit constraints.
- **Constraint surface for users.** How does a customer say "this build must run locally" or "prefer the Linux executors in eu-north"? Workspace config, CLI flag, env var, programmable from `@vixen/...` packages? Where does the constraint live and how does it propagate to the scheduler?
- **Fallback semantics.** What happens when the scheduler's first choice fails — fleet has no capacity, network drops mid-build, executor returns an error? Retry on a different executor? Promote to local? Surface to the user?
- **Priority and fairness.** When multiple builds compete for fleet capacity, how do they get ordered? Per-customer fairness, per-tier weighting, queue-aging?
- **Cache-aware routing.** If an exec is likely cache-hit on the public cache, do we route to it before considering executors at all? Where's the decision point?
- **Historical data shape.** What does "where did this build succeed last time" actually store and look up? Per-node? Per-graph? Per-customer?

## What unblocks resolution

Concrete shape of the first deployment — single-region fleet, one or two executor classes — plus actual customer build traces from Tier 1 to inform the heuristics. The detailed scheduler design isn't a Tier-2-launch blocker; an opinionated default (cache first, else fleet, else local, with explicit user override) is enough to ship.

## Related

- [Epic: vx cargo build, end-to-end](/vx-cargo-build-trace/) — the trace that surfaced this.
- [Architecture and infrastructure](/architecture-and-infrastructure/) — the fleet shape.
