# `peeps::peep!` instrumentation guide

This is the quick reference for adding causal wait edges with `peeps`.

## What it does

`peep!` wraps a future so `peeps` records:

- which parent node was waiting
- what child wait node was touched
- pending vs ready transitions over time

Use it for logical waits that are not already first-class resources.

## API

Use the macro form:

```rust
let result = peeps::peep!(some_future(), "rpc.client.call").await;
```

With metadata and explicit node kind:

```rust
let result = peeps::peep!(
    some_future(),
    "rpc.client.call",
    kind = peeps_types::NodeKind::Future,
    {
        let mut m = std::collections::BTreeMap::new();
        m.insert("rpc.method".to_string(), method_name.to_string());
        m
    }
)
.await;
```

## Important rule

Do not wrap first-class resource operations manually.

That means no extra `peep!` around operations that already emit their own nodes/edges, such as:

- socket read/write/flush/connect/accept
- channel send/recv
- file operations (open/read/write/rename/metadata/etc.)
- process/command execution wrappers

Those resources should instrument themselves once, at the wrapper level.

## Where to instrument

Instrument logical wait boundaries instead:

- high-level RPC call waits
- orchestrator futures (`select`, `join`, retry loops)
- task-local control-flow waits where no first-class wrapper exists

## Agent checklist

1. Use `peeps::peep!(...)` (not `.peepable(...)` extension methods).
2. Do not add duplicate instrumentation around first-class resource wrappers.
3. Keep labels stable and low-cardinality.
4. Build and run checks.
5. Verify expected nodes/edges appear in the graph.
