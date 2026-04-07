# 014: Demand Roots + Transitive Demand Propagation (Remove `mains.is_empty()` Hack)

Status: Not started.

## Goal

Make the analysis phase consistently pick entry roots and propagate demand through the local call graph regardless of whether a `main` exists. This is the minimum step needed to stop `AnalysisResult` from being “fixture-shaped”.

This is analysis-level only. It should not change parsing, resolution, or type checking.

## Motivation

`crates/vx-analysis/src/bridge_to_analysis.rs` currently gates transitive local-call demand propagation on `mains.is_empty()`. That makes the analysis behave differently in “tests with main” vs “build-shaped sources”, and it prevents using the analysis artifact as a stable planner.

## Work

- Replace `use_transitive_local_call_propagation = mains.is_empty()` with a single consistent rule:
- Determine `entry_ids` (prefer `main`, else `build`, else roots).
- Always walk `source_local_call_edges` transitively from `entry_ids` and mark all reachable top-level bodies as demanded.
- Mark demanded closure layouts as demanded when the call graph includes `LocalClosure` edges.
- Keep `incoming_use_observation` as “there exists at least one demand site” for now, but make it deterministic and independent of test shape.
- Update `seed_kernel_demands` to only copy demanded observations after the above propagation is complete.

## Acceptance

- Existing `vx-analysis` unit tests remain green.
- Add a new `vx-analysis` unit test where:
- `main -> helper -> leaf` exists (3 bodies).
- After `build_minimal_analysis`, both `helper` and `leaf` bodies have `result_observation == NeedValue`.
- Add a new unit test where:
- `main` exists but a disconnected top-level helper exists.
- The disconnected helper remains `NoObs`.
- Un-ignore and pass `pair_passed_to_fn_dead_sibling` in [demand_driven.rs](/Users/amos/bearcove/vixen/crates/vx-rt/tests/demand_driven.rs).

## Notes

This work item is intentionally scoped to “demand at the body granularity” (call graph reachability). It does not yet implement expression-level observation propagation through products, projections, `if`, `match`, etc.
