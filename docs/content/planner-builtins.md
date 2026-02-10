+++
title = "Planner Builtins and Capability Policy"
insert_anchor_links = "heading"
+++

# Deterministic Builtin Surface

This page defines the initial builtin catalog and capability policy for the in-process planner runtime.

Implementation lives in `crates/vx-plan/src/runtime.rs`.

## Capability Model

Capabilities are explicit and checked before host invocation.

- `emit-graph`
- `transform`
- `time`
- `random`
- `network`
- `filesystem`
- `process`

`PlannerPolicy::deterministic()` allows only:

- `emit-graph`
- `transform`

This ensures nondeterministic/ambient operations are denied by default.

## Builtin Catalog

### Graph emission

- `emit-node` (`emit-graph`)
- `emit-edge` (`emit-graph`)
- `emit-fact` (`emit-graph`)

These builtins materialize into `RulePlan` through `RulePlanHost`.

### Deterministic transforms

- `sort-map-keys` (`transform`): returns map keys in canonical sorted order
- `sort-scalars` (`transform`): returns scalar values in canonical sorted order

### Denied-by-default host effects

- `clock-now` (`time`)
- `random-int` (`random`)
- `net-request` (`network`)
- `fs-read` (`filesystem`)
- `process-spawn` (`process`)

In deterministic policy mode, these fail with source-mapped diagnostics.

## Typed Signatures

Builtins are identified by typed IDs (`PlannerBuiltinId`), not raw strings.
Argument parsing is typed through `PlannerBuiltinArgs` variants:

- `EmitNode(EmitNodeArgs)`
- `EmitEdge(EmitEdgeArgs)`
- `EmitFact(EmitFactArgs)`
- `SortMapKeys(SortMapKeysArgs)`
- `SortScalars(SortScalarsArgs)`
- `None`

## Diagnostics Contract

Capability denials and parse/runtime errors include:

- message
- `rule_id`
- `source_id`
- line/column
- runtime stack frames

This guarantees guardrail failures are actionable and source mapped.

## Test Coverage

`crates/vx-plan/src/runtime.rs` includes tests for:

- capability denial matrix across `time/random/network/filesystem/process`
- deterministic transform output ordering for `sort-map-keys` and `sort-scalars`
- end-to-end vertical slice from runtime calls to valid `RulePlan`
