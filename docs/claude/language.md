# Language Reference

This is a reference for Vixen's language semantics. For a tutorial
introduction, see [building-rust.md](building-rust.md).

## Types

### Scalars

- `Int` — integer
- `String` — UTF-8 text
- `Bool` — true or false
- `Blob` — immutable, content-addressed bytes in the store. Identity is BLAKE3
  hash. A blob has no path — it is pure content.

### Composite types

- `[T]` — list of T. Known length. Ordered.
- `{ field: T, ... }` — record/struct. Named fields.
- `Tree` — content-addressed directory structure. A manifest of named entries
  (blobs and subtrees). Identity is its merkle root.

### Streams

`Stream<T>` — a progressive, variable-length sequence of typed elements.

Streams differ from lists:
- A list has a known spine immediately. A stream produces elements over time.
- A list's length is known. A stream's length is not known until it completes.
- A stream may have elements still being produced while consumers read earlier
  elements.

Streams are reusable. Multiple consumers read the same stream. The producer
runs once; produced elements are cached by the runtime.

Streams are ordered. Elements arrive in a deterministic order for a given
input. This order is part of the stream's identity.

#### Stream operations

- `stream.find(predicate)` — returns the first matching element. Blocks until
  a match arrives.
- `stream.filter(predicate)` — returns a sub-stream of matching elements.
  Progressive: elements pass through as they arrive.
- `stream.map(f)` — transforms each element. Progressive.
- `stream.collect_list(order: ...)` — collapses the stream into a list.
  Requires an explicit ordering policy. Blocks until the stream completes.
- `stream.collect_tree()` — collapses a stream of `TreeEntry` elements into a
  `Tree`. Canonicalizes by path and structure. Blocks until the stream
  completes.

There is no `zip`, `fold`, `reduce`, `group_by`, or `join` on streams in v0.
If a program needs those, it should collect first, then operate on the list.

### Enums

Variants can have named fields or positional fields (tuple-style):

```
// Named fields
enum Shape {
  Circle { radius: Float }
  Rect { w: Float, h: Float }
}

// Tuple-style (positional)
enum RustcEvent {
  RMeta(Blob)
  RLib(Blob)
  Diagnostic(RustDiagnostic)
  Finished(ExitStatus)
}
```

Enum variants are proper types. `Shape::Circle` is both a variant of `Shape`
and a standalone type `Circle`. A value of type `Circle` is a value of type
`Shape`. Similarly, `RMeta` is a standalone type.

Tuple-style variants expose their fields positionally: `rmeta_event.0` gives
the `Blob` inside an `RMeta(blob)`. Named-field variants use field names:
`circle.radius`.

This is unlike Rust, where `Option::Some(5)` is type `Option<i32>`, not a
type `Some`. In Vixen, enum declaration simultaneously defines the variant
types as real, usable types.

Variant fields can have default values:

```
enum SourceReq {
  Registry {
    crate: String
    req: VersionReq
    registry: String = "crates-io"
  }
}
```

### The `as` operator

`value as T` narrows a value to type T. If the value is of type T (or a
variant matching T), it returns the narrowed value. Otherwise, nothing.

This is the primary mechanism for working with enums:

```
let event: RustcEvent = ...
let meta = event as RMeta  // RMeta or nothing
```

Combined with `$` closures, this gives concise stream filtering:

```
let rmeta = events.find($ as RMeta).0
let diagnostics = events.filter($ as Diagnostic).map($.0)
```

### Sets

`Set<T>` — an unordered collection of unique values.

```
let features = set { "default", "serde", "std" }
features.contains("serde")  // true
features.insert("alloc")    // new set with "alloc" added
```

### Options

`Option<T>` — a value that may or may not be present.

- `some(value)` — wraps a value
- `none` — absent
- `opt.some?` — true if present
- `opt.none?` — true if absent
- `opt.map(f)` — applies f if present
- `opt.or(default)` — unwraps, or returns default
- `opt ?? fallback` — same as `.or(fallback)` but also works for non-Option
  expressions that might fail

## Bindings and values

`let x = expr` creates a binding. Bindings name computation graph nodes.
Evaluation is lazy — `expr` is not computed until something demands `x`.

Rebinding is allowed:

```
let x = 5
x = x + 1  // x is now 6. The old 5 is unreferenced.
```

This is not mutation. It creates a new binding that shadows the old one. Any
function that received the old value still has it.

### Value semantics

All values except streams have copy semantics. `let y = x` gives `y` an
independent copy. Passing a value to a function gives the function a copy.

Streams have reference semantics: multiple bindings to the same stream share
the underlying producer. This is not shared mutable state — stream elements
are produced once and cached. No consumer can modify the stream.

### Record update with `with`

`expr with { field = new_value }` creates a new record with one or more
fields replaced:

```
let updated = dep with {
  role = Host,
  requested_features = dep.features ++ extra_features,
}
```

This is not mutation — it produces a new value. The original is unchanged.

### Concatenation with `++`

`++` concatenates lists and unions sets:

```
let all_deps = normal_deps ++ build_deps ++ target_deps
let all_features = requested ++ set { "default" }
```

### Local mutation as syntax sugar

Locals can be ergonomically rebindable without introducing shared mutable
state. The model:

```
let items = []
for dep in pkg.deps {
  items = items.push(build(dep))
}
```

This is semantically a series of rebindings. The `items` binding is
reassigned, not mutated. If `items` had been passed to a function before the
loop, the function's copy would be unchanged.

Whether this needs an explicit `mut` keyword or not is an open question. See
[open-questions.md](open-questions.md).

### Maps

`Map<K, V>` — a key-value mapping. Built from key-value pairs:

```
let m = deps.map(|dep| dep.name => build_dep(dep, tc)).into_map()
```

The `=>` operator creates a key-value pair. `.into_map()` collects pairs into
a map. Maps support index access: `m["key"]`.

### Tree literals

`tree { ... }` builds a `Tree` value declaratively:

```
let externs = tree {
  for (name, dep) in built_deps {
    "lib${name}.rmeta" = dep.rmeta
  }
}
```

Inside the block, string keys map to blob values. Supports `for` loops and
conditionals for generating entries. The tree is content-addressed — its
identity is its merkle root.

## Functions

```
fn compile(src: Tree, tc: Toolchain) -> Stream<RustcEvent> {
  ...
}
```

Functions are the primary authoring unit. There is no `pub`/`private` in v0 —
discoverability comes from the shape of top-level definitions.

Functions may call host primitives (`exec`, `acquire`, etc.) without
declaration. There is no effect coloring in the type system. The runtime knows
which computations are pure and which are effectful.

### Expression bodies

Functions with a single expression body use `=`:

```
fn parse_manifest(src: Tree) -> CargoManifest =
  src["Cargo.toml"].read_utf8().toml().decode<CargoManifest>()
```

Functions with multiple statements use `= { ... }`:

```
fn build_crate(src: Tree, tc: Toolchain) -> BuiltCrate = {
  let cargo = parse_manifest(src)
  ...
}
```

### `$` closures

`$` is a shorthand for the anonymous closure argument:

```
list.map($ + 1)            // |x| x + 1
list.filter($.age > 18)    // |x| x.age > 18
stream.find($ as Metadata) // |x| x as Metadata
```

For multi-argument closures or complex bodies, use the full form: `|a, b| ...`

### Implicit memoization

There is no `memo` keyword or explicit cache annotation. Function calls are
automatically cached by computation identity.

```
// Two callers requesting the same dep get the same cached build.
// The runtime deduplicates automatically.
fn build_pkg(pkg: ResolvedPkg, ctx: BuildCtx) -> BuiltCrate = {
  ...
}
```

The computation identity of a call is derived from the structural identity of
its arguments. Two calls are the same computation if and only if they receive
structurally identical values. The runtime caches results by computation
identity and reuses them across callers.

This means:
- Recursive calls with the same arguments execute once, not N times
- The dependency graph is a DAG, not a tree — shared dependencies are shared
- No explicit deduplication or memoization annotation in user code

## Modules

A Vixen file is a module. It exports types, values, and functions. There is no
separate rule-definition phase.

```
// std/rust.vx — part of the standard library

fn compile_crate(src: Tree, deps: [...], tc: Toolchain) -> Stream<RustcEvent> {
  ...
}

fn link_binary(crates: [...], tc: Toolchain) -> Blob {
  ...
}
```

## Host builtins

The primitive set is small. Everything else should be written in Vixen.

### exec

Runs a sandboxed process. Generic over the event type yielded by the parser.

```
exec(
  cmd   = [program, args...],
  env   = { KEY: "value", ... },
  mount = { "/path": tree_or_blob, ... },
  parse = parser_function,
) -> Stream<E>
```

- `cmd`: the command and arguments. Supports inline `for` comprehensions for
  building argument lists.
- `env`: environment variables (optional).
- `mount`: trees or blobs mounted at paths in the sandbox.
- `parse`: a function `(Process) -> Stream<E>` that runs on the executor
  alongside the process, parsing structured output and yielding typed events.

`exec` returns `Stream<E>` where `E` is the type yielded by the parser.

If no parser is provided, `exec` returns a stream that emits a single
completion event after the process exits.

The process runs in a sandbox:
- Only mounted inputs are visible
- No network access
- No ambient host state
- Nonzero exit is a failure

#### Inline comprehensions in cmd

```
cmd = [
  tc.rustc,
  "--crate-name", cargo.package.name,

  for entry in externs.entries() {
    "--extern", "${entry.stem}=${entry.path}"
  },

  "/src/src/lib.rs",
]
```

The `for` loop generates argument sequences that are spliced into the list.

### acquire

Downloads, verifies, and extracts an archive into the store.

```
acquire(url: String, sha256: String) -> Stream<AcquireEvent>
```

Returns a stream of events as archive entries are extracted and committed to
the store. The hash is mandatory.

### download

Fetches a single file (no archive extraction).

```
download(url: String, sha256: String) -> Blob
```

### source_tree

Returns the project source as a content-addressed tree.

```
source_tree() -> Tree
```

### File operations

- `read_text(blob) -> String` — decode blob as UTF-8
- `read_bytes(blob) -> Bytes` — raw bytes
- `write_file(content) -> Blob` — create a pathless blob in the store
- `json_parse(text) -> Value` — parse JSON
- `toml_parse(text) -> Value` — parse TOML
- `blake3(blob) -> String` — hash

### Tree operations

- `tree["path"]` — index access, look up a blob by relative path
- `tree.get("path")` — like index, but returns nothing instead of failing
- `tree.entry(path) -> TreeEntry` — look up an entry with metadata
- `tree.subtree(path) -> Tree` — get a subtree rooted at path
- `tree.entries() -> Stream<TreeEntry>` — all entries
- `tree.glob(pattern) -> Stream<TreeEntry>` — entries matching a glob
