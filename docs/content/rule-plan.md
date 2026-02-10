+++
title = "RulePlan Schema"
description = "Typed planning IR between rule evaluation and execution"
+++

`RulePlan` is vixen's typed intermediate representation between rule evaluation and execution.
Its canonical implementation lives in `crates/vx-plan`.

## Node Kinds

- `action`
- `artifact`
- `fact`
- `group`

## Edge Kinds

- `ifchanged`
- `ifwritten`
- `ifcreate`
- `ifdeleted`
- `always`

## Provenance

Every emitted node, edge, and fact carries provenance metadata:

- `rule_id`
- interned `source_id` (resolved through `RulePlan.sources`)
- span (`line`, `column`, optional end position)
- optional comment
- optional key/value metadata map

This metadata is required and is used for explainability, diagnostics, and graph diff tooling.

## Validation Invariants

`RulePlan::validate()` enforces:

- identity: non-empty unique node IDs and fact IDs
- referential integrity: every edge endpoint and fact `node_id` references an existing node
- source integrity: every provenance `source_id` references an existing source entry
- edge-kind constraints:
  - `ifwritten`/`ifcreate`/`ifdeleted`: `action -> artifact`
  - `ifchanged`: `artifact|fact -> action|group`
  - `always`: `action|group -> action|group`

Validation failures return actionable diagnostics that include the offending IDs and edge kinds.

## Deterministic Serialization

`RulePlan::to_canonical_json()` emits stable JSON suitable for explain/diff/caching workflows.

Canonicalization rules:

- nodes sorted by `(id, kind, label)`
- edges sorted by `(from, to, kind)`
- facts sorted by `(id, node_id)`
- sources sorted by `(source_id, path)`
- provenance metadata stored as deterministic key order (`BTreeMap`)
