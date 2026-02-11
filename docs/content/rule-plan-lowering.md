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

Current bridge plans also annotate compile action nodes with `legacy_node_id` metadata
so planner output can be correlated with existing executor node identities.
Compile action nodes that include `crate_name` + `crate_type` provenance metadata
are lowered to a dedicated planner-compile action that reports as
`rust.compile_bin`/`rust.compile_rlib`.

Registry artifact nodes that include `registry_name`, `registry_version`, and
`registry_checksum` provenance metadata are lowered to real
`AcquireRegistryCrate` executor actions.

When `VX_RULE_PLAN_BRIDGE=1`, vxd also runs a focused parity check between the
legacy planner topology and RulePlan-lowered topology for compile/registry node IDs.
Set `VX_RULE_PLAN_PARITY_STRICT=1` to fail builds when that parity check detects drift.

## Loading .vx Rules Files (Vertical Slice)

When `VX_RULE_PLAN_BRIDGE=1` and `VX_RULES_FILE` is set, vxd will parse and evaluate a
`.vx` rules file through `PlannerRuntime` to produce a `RulePlan` (instead of using the
hardcoded Rust `CrateGraph -> RulePlan` builder).

Current `.vx` syntax is intentionally tiny: one builtin call per line with `key=value` args.

Example:

```text
emit-node id=n_action kind=action label="compile-bin:app"
emit-node id=n_artifact kind=artifact label="artifact:app"
emit-edge from=n_action to=n_artifact kind=ifwritten
```

Note: in this migration mode, execution is still performed by the legacy action graph; the `.vx`
rules file currently affects the report topology and parity harness, not the actual compilation steps.

## Real Execution (Migration Slice)

Set `VX_RULE_PLAN_EXEC=1` (with `VX_RULE_PLAN_BRIDGE=1`) to execute a real Rust `ActionGraph`
constructed from the subset of crates selected by the RulePlan.

In `.vx` rules mode, any `compile-*` action nodes in the RulePlan select crates by
`compile-<bin|lib>:<crate_name>`. vxd computes the transitive dependency closure and executes that subset.

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
