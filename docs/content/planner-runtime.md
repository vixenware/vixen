+++
title = "Planner Runtime Boundary"
insert_anchor_links = "heading"
+++

# In-Process Runtime Boundary

This page defines the first in-process runtime boundary between the vixen planner host and the Vixen language interpreter.

Scope in this vertical slice:

- deterministic internal planner value model
- single host interface for builtin registration and invocation
- capability-gated builtin execution
- source-mapped runtime diagnostics
- direct `RulePlan` emission path for planner output

## Modules

Implementation lives in `crates/vx-plan/src/runtime.rs`.

The runtime is intentionally in-process and Rust-native:

- no external wire format
- no cross-process ABI
- planner and host evolve together

## Internal Value Model

`PlannerValue` supports deterministic data structures:

- `Map(BTreeMap<String, PlannerValue>)`
- `Set(BTreeSet<PlannerScalar>)`
- `Vector(Vec<PlannerValue>)`
- `Keyword`, `String`, `Int`, `Bool`, `Null`

Ordering-sensitive structures (`Map`, `Set`) are tree-backed for stable iteration.

## Host Boundary

`PlannerHost` is the boundary contract:

- `register_builtin(...)`
- `invoke_builtin(...)`

`PlannerRuntime` owns policy enforcement and interpreter execution.
`RulePlanHost` is the initial host implementation that materializes planner calls into a `RulePlan`.

## Builtins and Capabilities

The builtin surface and policy are specified in [`/planner-builtins/`](/planner-builtins/).

Capabilities are explicit (`emit-graph`, `transform`, `env`, `time`, `random`, `network`, `filesystem`, `process`).
Runtime checks capability before invoking host builtins.

## Diagnostics Contract

Runtime failures return `PlannerRuntimeError` with:

- message
- `rule_id`
- `source_id`
- line/column span
- stack frames (builtin call context)

This keeps failures actionable and source-mapped without requiring any external protocol.

## Vertical Slice Path

A minimal planner program can call builtins and produce a validated `RulePlan`:

1. interpreter evaluates forms
2. runtime validates capability policy
3. host builtin invocation mutates in-memory plan
4. resulting `RulePlan` is validated and can be serialized canonically

## Tests

`crates/vx-plan/src/runtime.rs` includes regression tests for:

- deterministic output from the same program
- capability-gating failures with source-mapped diagnostics
- end-to-end valid `RulePlan` emission from runtime calls
