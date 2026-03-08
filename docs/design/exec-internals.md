# Exec internals

`exec` runs an external process on an executor with a virtual filesystem.

## Mount

A `Mount` describes how a store object appears in the executor's VFS:

```
Mount = struct {
  source: Blob | Tree,
  writable: Bool,
};
```

- **Read-only mount** (`writable: false`, the default) — the content is
  available in the VFS but the process cannot modify it. Most inputs
  (source files, dependencies, toolchains) are mounted read-only.

- **Writable mount** (`writable: true`) — the process can write to this
  location. Used for output directories. The observer can `commit` paths
  under writable mounts to capture outputs.

Mounts are specified as a map from `Path` (the VFS location) to `Mount`.
All paths are relative to the working directory — no absolute paths.

## Process

The `Process` handle available inside the observer:

```
Process = struct {
  stdout: Stream[Blob],
  stderr: Stream[Blob],
  exit_code: U64,       // pending until the process exits
};
```

- `stdout` and `stderr` are streams of chunks (blobs). The observer can
  read them incrementally as the process runs.
- `exit_code` is a pending value that resolves when the process exits.
  The observer can yield events before the process exits (streaming
  pattern) or wait for `exit_code` to resolve first (simple pattern).

The observer does not have access to stdin — processes are not interactive.
There is no signal API — if the observer needs to stop a process, the
runtime handles it (e.g., when the build is cancelled or a timeout fires).

## Arg and env types

Exec arguments and environment values must carry store references so the
runtime knows what to materialize in the VFS:

```
ExecArg = String | Path;
```

A plain `String` is a literal argument. A `Path` carries a store reference —
when it appears in an argument, the runtime ensures the referenced content
is available in the VFS at that path.

String interpolation with a `Path` produces an `ExecArg`, not a plain
`String`:

```vixen
arg = `--extern=dep=${rmeta}`;  // ExecArg, carries rmeta's store ref
plain = `--jobs=4`;              // String, no store refs
```

Environment values follow the same pattern:

```
env: Map[String, ExecArg]
```

The exact types for args and env values are `Array[ExecArg]` and
`Map[String, ExecArg]`.

## Commit and process lifecycle

`commit(path)` returns a lazy `Blob` handle. The observer can call `commit`
while the process is still running — but the path must exist and be stable
at the time the handle is demanded (i.e., when it crosses the yield
boundary back to the orchestrator).

In practice this means:

- **Streaming pattern**: the observer watches stdout for structured messages
  (e.g., rustc's `--json` output) that announce "file X is complete." The
  observer calls `commit` on the announced path, then yields an event
  carrying the handle. The yield forces materialization.

- **Simple pattern**: the observer waits for `exit_code`, then commits the
  output directory. Everything is stable because the process has exited.

There is no filesystem watching built into the runtime. The observer is
responsible for knowing when files are ready, typically by parsing process
output. This keeps the model simple — the observer is the interpreter
between the raw process and the structured build graph.

## Executor lifecycle

An executor stays alive as long as:

1. The process is still running, OR
2. There are unmaterialized `commit` handles from that executor

Once the process exits and all yielded handles are materialized, the
executor is released. This is why the yield boundary is the materialization
point — it ensures the executor doesn't need to hold file data indefinitely.
