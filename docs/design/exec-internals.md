# Exec internals

This content has moved to `builtins.md`.

## Executor lifecycle

An executor stays alive as long as:

1. The process is still running, OR
2. There are unmaterialized VFS paths from yielded values

Once the process exits and all yielded values are materialized to the
content-addressable store, the executor is released. The yield boundary
is the materialization point — it ensures the executor doesn't need to
hold file data indefinitely.
