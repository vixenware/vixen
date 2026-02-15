# Edge Emission Contract (No Shortcut Edges)

## Why this exists

The debugger graph must be explainable and traversable.

If we emit both immediate edges and transitive shortcuts, the graph becomes noisy and SCC/deadlock analysis becomes less trustworthy.

This contract defines exactly which `needs` edges are allowed.

## Core invariant

Emit only **immediate** dependency edges.

Given a path `A -> B -> C`, do **not** also emit `A -> C` unless `A` directly and immediately depends on `C` at runtime.

In short: no shortcut edges.

## Meaning of `needs`

`src --needs--> dst` means:

`src` cannot make progress right now unless `dst` changes state / advances.

`needs` is directional and immediate.

## Execution-context model

Each task maintains a logical async stack:

- task root
- current future
- nested awaited futures
- leaf resource interaction (lock/channel/socket/request/etc.)

Edge emission is from the **top active frame** to exactly one immediate dependency target.

## Allowed edge patterns

### Structural ownership (immediate scope only)

- `future --needs--> task` only if future execution is currently bound to that task context.
- `resource-end --needs--> resource-peer-end` for gateway-style resources:
  - `mpsc_tx -> mpsc_rx`
  - `oneshot_rx -> oneshot_tx` (or tx/rx direction matching real blocking dependency)
  - `request -> response`

No extra ownership shortcuts.

### Wait edges from active frame

When a frame waits, emit only one immediate wait edge:

- `future --needs--> lock`
- `future --needs--> semaphore`
- `future --needs--> channel_end`
- `future --needs--> socket`
- `future --needs--> request|response`

If the waiter is currently represented by a task frame rather than future frame, use task as source.

### Lock-holder bridge (only when contended)

To traverse waiter -> lock -> holder chain, allow:

- `lock --needs--> holder_task_or_future`

Emit this only for currently held/contended locks where holder identity is explicit.

## Forbidden edges

- Any transitive shortcut edge (e.g. `request -> lock` when `request -> future -> lock` exists).
- Any duplicate semantic edge between same nodes with same meaning.
- Any inferred/heuristic edge not backed by explicit runtime event/state.
- Any edge that exists "for convenience of UI" but not as an immediate runtime dependency.

## Channels and RPC gateways

Channels and RPC are modeled as gateways, not magical jumps.

### Local channels

- waiting sender/receiver frame points to channel end it is blocked on.
- channel end points to peer end when peer advancement is required.

Example:

`future(send) -> mpsc_tx -> mpsc_rx -> receiver_future`

### Cross-process RPC

- caller side emits request/response nodes and immediate wait edges.
- receiver side emits processing futures/tasks and edges from response-side dependencies.
- cross-process linkage uses explicit propagated correlation metadata only.

No synthetic "request directly needs random downstream resource" edges.

## Event-to-edge discipline

For each instrumentation event:

1. Identify current active node (top of logical async stack).
2. Identify one immediate dependency target.
3. Emit one `needs` edge for that immediate dependency.
4. Do not emit ancestors' dependencies from this event.

## De-duplication rule

Within one snapshot, keep one canonical edge row per `(src_id, dst_id, kind='needs')`.

Count/frequency can live in edge attrs if needed, but must not create parallel duplicate edges.

## Sanity checks

The graph is healthy if:

- For most leaf waits, there is a short explicit chain to a resource state bottleneck.
- SCCs are small and interpretable (not inflated by shortcut edges).
- Removing transitive reduction does not change causal conclusions.

## Practical examples

Good:

- `request -> handler_future`
- `handler_future -> mpsc_tx`
- `mpsc_tx -> mpsc_rx`
- `mpsc_rx -> worker_future`
- `worker_future -> lock`
- `lock -> holder_future`

Bad:

- `request -> lock`
- `request -> holder_future`
- `handler_future -> worker_future` (if only connected through channel)

---

If in doubt, prefer missing edge over guessed edge.
A sparse explicit graph is better than a dense speculative graph.
