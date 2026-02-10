+++
title = "RulePlan Lowering Bridge"
insert_anchor_links = "heading"
+++

# RulePlan -> ActionGraph (Starter Bridge)

This page describes the initial lowering bridge from `RulePlan` to `ActionGraph`.

Implementation lives in `crates/vxd/src/action_graph.rs` via:

- `ActionGraph::from_rule_plan(...)`

## Current Scope

The starter bridge maps planner IR nodes into synthetic executable graph nodes so they can run through the existing executor pipeline.

Supported node kinds:

- `group`
- `action`
- `artifact`
- `fact`

Supported edge kinds:

- `always`
- `ifwritten`
- `ifchanged`

Unsupported in starter bridge:

- `ifcreate`
- `ifdeleted`

Those fail with explicit provenance-rich lowering errors.

## Error Contract

Lowering failures return `RulePlanLoweringError` with:

- message
- `rule_id`
- `source_id`
- line/column

This keeps migration failures actionable and source-mapped.

## Topology Preservation

`ActionGraph` now stores edge kind metadata, and `to_topology()` preserves those edge kinds in report serialization.

This allows explain/report consumers to distinguish conditional edge semantics even before full native runtime execution mapping is complete.

## Tests

`crates/vxd/src/action_graph.rs` includes bridge tests for:

- successful lowering with preserved edge kinds
- unsupported edge kind failures with provenance context

## Build Integration Mode

When `VX_RULE_PLAN_BRIDGE=1` is set, vxd currently:

1. builds + validates + lowers `RulePlan`
2. uses lowered RulePlan topology in reports
3. still executes the existing Rust action graph for actual compilation

This keeps real build behavior stable while planner semantics move forward.
