# Built-in functions and types

Built-ins that are part of the runtime, not the language itself.

## `exec`

`exec` runs an external process on an executor — a sandboxed runner with
a virtual filesystem, potentially a VM in the cloud.

```vixen
exec[T](
  args: Array[ExecArg],
  env: Map[String, ExecArg],
  mounts: Map[Path, Mount],
  observer: Fn(Process, Tree) -> Stream[T],
) -> Stream[T]
```

`exec` is generic over `T`. The observer closure determines `T`, and `exec`
returns the same `Stream[T]` to the caller.

### ExecArg

```vixen
ExecArg = String | Path;
```

A plain `String` is a literal argument. A `Path` carries a store reference —
when it appears in an argument, the runtime ensures the referenced content
is available in the VFS at that path.

String interpolation with a `Path` produces an `ExecArg`:

```vixen
arg = `--extern=dep=${rmeta}`;  // ExecArg, carries rmeta's store ref
plain = `--jobs=4`;              // String, no store refs
```

### Mount

```vixen
Mount = struct {
  source: Blob | Tree,
  writable: Bool,
};
```

- **Read-only** (`writable: false`, the default) — content is available in
  the VFS but cannot be modified. Most inputs (source files, dependencies,
  toolchains) are mounted read-only.
- **Writable** (`writable: true`) — the process can write to this location.
  Used for output directories.

All mount paths are relative to the working directory — no absolute paths.

### Process

```vixen
Process = struct {
  stdout: Stream[Blob],
  stderr: Stream[Blob],
  exit_code: U64,       // pending until the process exits
};
```

- `stdout` and `stderr` are streams of chunks. The observer can read them
  incrementally as the process runs.
- `exit_code` is a pending value that resolves when the process exits.

No stdin — processes are not interactive. No signal API — the runtime
handles cancellation and timeouts.

### The observer

The observer runs **on the executor**, not the orchestrator. It is
serialized along with its captured values and shipped to the executor.
A closure cannot be sent until all its captures are resolved — attempting
to serialize a pending value is just another form of depending on it.

The observer receives a `Process` handle and the VFS root as a `Tree`,
and returns a stream of events:

```vixen
fn observer(p: Process, vfs: Tree) -> Stream[RustcArtifact] {
  p.stdout
    .filter_map(Json.parse?[RustcMessage])
    .filter($.artifact_path != None)
    .map((msg) => RustcArtifact {
      kind: msg.kind,
      blob: vfs.at(msg.artifact_path),
    })
}
```

### The yield boundary

When the observer yields a value back to the orchestrator, any VFS paths
in the yielded value are committed to the content-addressable store
automatically. This is the **materialization boundary** — the runtime
handles the data movement so the executor can be released. There is no
explicit `commit` function.

### Observer patterns

**Streaming observer** — for tools like `rustc` that emit structured output
during execution. The observer parses stdout incrementally and yields
events as artifacts become available. Downstream dependents can start work
early — e.g., compiling a downstream crate as soon as the metadata file
is emitted, without waiting for code generation.

**Simple observer** — for tools where you only care about the final result.
The observer references `exit_code` (pending until the process exits) and
VFS paths for the output. Everything resolves when the process finishes:

```vixen
fn observer(p: Process, vfs: Tree) -> Stream[Output] {
  Stream.of(Output {
    result: vfs.at("output") / "result",
    exit_code: p.exit_code,
  })
}
```

### Stream errors

When a process crashes or the executor fails, the stream terminates. Any
pending value derived from that stream becomes poisoned.

Application-level errors (e.g., `rustc` reporting a compilation error)
are the observer's responsibility — parse stdout/stderr and call `fail`
with a meaningful message.

## String methods

Strings have built-in methods for common operations:

### Testing

```vixen
s.starts_with("prefix")     // Bool
s.ends_with("suffix")       // Bool
s.contains("needle")        // Bool
s.is_empty()                // Bool
```

### Splitting and joining

```vixen
s.split("/")                 // Array[String]
s.split_once("/")            // Option[(String, String)] — first occurrence
parts.join(", ")             // String — join array elements with separator
s.lines()                    // Array[String] — split by newline
```

### Transforming

```vixen
s.trim()                     // strip leading/trailing whitespace
s.trim_start()               // strip leading whitespace
s.trim_end()                 // strip trailing whitespace
s.to_upper()                 // "HELLO"
s.to_lower()                 // "hello"
s.replace("old", "new")     // replace all occurrences
s.replace_once("old", "new") // replace first occurrence
```

### Slicing

```vixen
s.len()                      // U64 — byte length
s.chars()                    // Array[String] — Unicode grapheme clusters
s.slice(0..5)                // String — substring by byte range
```

### Parsing

```vixen
s.parse_u64()                // U64 — poisons if not a valid number
s.parse_i64()                // I64
s.parse_f64()                // F64
s.parse_u64()?               // Option[U64] — None if not a valid number
```

### Regex

```vixen
s.matches("\\d+")            // Bool — test against a regex pattern
s.find_all("\\d+")           // Array[String] — all matches
s.captures("(\\w+)=(\\w+)")  // Option[Array[String]] — capture groups
s.replace_regex("\\s+", " ") // replace by regex pattern
```

### Path-oriented string operations

Build systems work heavily with paths as strings. These complement the
`Path` type for cases where you're working with raw path strings:

```vixen
s.strip_prefix("src/")       // Option[String]
s.strip_suffix(".rs")        // Option[String]
```

## `Stream.of`

Creates a single-element stream:

```vixen
s = Stream.of(value);  // Stream[T] with one element
```

## `acquire`

Fetches a resource by URL. Cached forever when a hash is provided:

```vixen
fn fetch_rustc(version: String, target: String) -> Tree {
  acquire(
    `https://static.rust-lang.org/dist/rustc-${version}-${target}.tar.xz`,
    sha256 = "abc123...",
  )
}
```

## Memoization

The runtime memoizes function calls transparently. A function without
revalidation annotations (like `@ttl`) is assumed to be deterministic —
same arguments, same result, cached forever. Functions with `@ttl` are
cached provisionally and revalidated according to their policy.

`exec` is a special case. Its cache key includes the runner fingerprint
in addition to the declared inputs: **(args, env, mounts, observer +
captures, runner fingerprint)**. If all match a previous execution, the
runtime replays the cached stream without running the process again.

Runner-contributed inputs (toolchain versions like Xcode, MSVC, GCC) are
fingerprinted and tracked — if the toolchain version changes, the cache
key changes. These are not visible in the Vixen language.

## Mutable resources and `@ttl`

Not all inputs to a build are stable. `@ttl` marks functions that access
mutable resources:

```vixen
@ttl("1h")
fn rust_stable_version() -> String {
  // fetch channel manifest, parse, return version string
}
```

The runtime caches the result provisionally and revalidates according to
the policy. If the revalidated result equals the cached result, downstream
nodes are not re-evaluated (early cutoff).

Operational knobs (runtime/UX decisions, not language semantics):

- Default TTL for mutable queries
- Whether CI can force a refresh (`vx build --refresh`)
- Whether local dev can use stale values for speed
- Whether lockfiles bypass TTL entirely
