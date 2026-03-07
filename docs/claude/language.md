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
- `{ field: T, ... }` — anonymous record. Named fields.

#### List operations

- `list.map(f)` — transform each element
- `list.filter(predicate)` — elements matching a predicate
- `list.flat_map(f)` — map, then flatten one level
- `list.push(element) -> List<T>` — new list with element appended
- `list.into_set() -> Set<T>` — collect into a set
- `list.into_map() -> Map<K, V>` — collect key-value pairs into a map
- `Tree` — content-addressed directory structure. A manifest of named entries
  (blobs and subtrees). Identity is its merkle root.

### Structs

`struct` declares a named record type:

```
struct BuiltCrate {
  rmeta: Blob
  rlib: Blob
  diagnostics: Stream<RustDiagnostic>
}

struct Package {
  name: String
  version: String
  edition: String
}
```

Struct values are constructed by name or anonymously. Both forms use the same
field syntax:

```
// Named construction
Toolchain {
  rustc   = root["bin/rustc"],
  std     = root.subtree("lib"),
  version = resolved.version,
  target  = target,
}

// Anonymous — inferred from return type or binding
{
  name    = manifest.package.name,
  version = manifest.package.version,
  source  = source,
}
```

Fields are accessed with dot syntax: `pkg.manifest.package.name`.

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

Enum variants are proper types. `Circle` is both a variant of `Shape`
and a standalone type. A value of type `Circle` is a value of type
`Shape`. Similarly, `RMeta` is a standalone type.

Variants can be referenced with qualified syntax when disambiguation is
needed:

```
Channel.Stable
SourceRef.Path { path: path }
```

Or unqualified when the variant name is unambiguous (including after `use`):

```
use std.rust.types { Role, Target, Host }
let role = Target
```

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

### Operators

#### Arithmetic

`+`, `-`, `*`, `/` — standard numeric operators.

#### Comparison

- `==`, `!=` — structural equality
- `<`, `>`, `<=`, `>=` — ordering

```
deps.filter($.role == Target)
deps.filter($.kind != Dev)
```

#### Boolean

- `!` — negation
- `&&` — short-circuit and
- `||` — short-circuit or

```
.filter(!$.optional || enabled_features.contains($.extern_name))
.filter($.target_when.none? || eval_cfg($.target_when?, ctx.target))
```

#### Concatenation with `++`

`++` concatenates lists and unions sets:

```
let all_deps = normal_deps ++ build_deps ++ target_deps
let all_features = requested ++ set { "default" }
```

#### Key-value with `=>`

`key => value` creates a key-value pair. Used with `.into_map()`:

```
deps.map(|dep| dep.name => build_dep(dep)).into_map()
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
a map.

Empty map literal: `{}`

```
let rustc_env = {}
```

#### Map operations

- `map["key"]` — index access, fails if key is absent
- `map.get(key) -> Option<V>` — look up a key, returns nothing if absent
- `map.entries() -> List<(K, V)>` — all key-value pairs (iterable with `for`)
- `map.has(key) -> Bool` — test for key presence

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

## Control flow

### match

Pattern matching on values. Arms use `=>` and are checked top-to-bottom.
`match` is an expression — each arm produces a value.

```
let channel_name = match channel {
  Stable        => "stable"
  Beta          => "beta"
  Nightly { d } => "nightly-${d}"
}
```

Supported patterns:

- **Unit variants**: `Stable =>`, `None =>`
- **Tuple-style destructuring**: `Some(v) =>`, `String(version_req) =>`
- **Named-field destructuring**: `Nightly { d } =>`, `Path { path } =>`
- **Named-field with rebinding**: `Registry { crate, req, registry = reg } =>`
  binds the `registry` field to the local name `reg`
- **String literals**: `"compiler-artifact" =>`, `"build-finished" =>`

Arms can have block bodies:

```
match msg["reason"] {
  "compiler-artifact" => {
    let filenames = msg["filenames"]
    ...
  }
  "compiler-message" => {
    ...
  }
}
```

Nested matching works naturally:

```
match feature_map.get(feat) {
  Some(items) =>
    for item in items {
      match item {
        Feature(name) => ...
        Dep(name)     => ...
        DepFeature { dep, feature } => ...
      }
    }
  None => {}
}
```

### if

Two forms, both expressions.

**Expression form** with `then`/`else` — for inline use:

```
let edition = raw.get("edition").map($.as_string()).or("2021")
let role = if dep.kind == Build then Host else Target
```

**Block form** — for statements and side effects:

```
if filename.ends_with(".rmeta") {
  yield RMeta(store.commit(filename))
}
```

Block form with else:

```
if src.get("src/lib.rs").some?
  then some(LibTarget { path = "src/lib.rs", crate_type = "lib" })
  else none
```

Multi-branch:

```
if t.has("path") then
  Path { path = t["path"].as_string() }
else if t.has("git") then
  Git { url = t["git"].as_string(), ... }
else
  Registry { ... }
```

### for

`for` iterates over lists, sets, streams, and map entries.

```
for line in process.stdout_lines() {
  ...
}
```

Destructuring in the binding:

```
for (name, dep) in target_deps {
  "lib${name}.rmeta" = dep.rmeta
}
```

`for` works in multiple contexts:

- **Statements** — general iteration with side effects (yield, rebinding):
  ```
  for feat in worklist {
    if !result.contains(feat) {
      result = result.insert(feat)
    }
  }
  ```

- **Tree literals** — generates entries:
  ```
  tree {
    for (name, dep) in built_deps {
      "lib${name}.rmeta" = dep.rmeta
    }
  }
  ```

- **Inline in cmd lists** — splices generated arguments:
  ```
  cmd = [
    tc.rustc,
    for entry in externs.entries() {
      "--extern", "${entry.stem}=${entry.path}"
    },
  ]
  ```

- **Inline in env maps** — generates key-value pairs:
  ```
  env = {
    "OUT_DIR": "/out",
    for feat in pkg.enabled_features {
      "CARGO_FEATURE_${feat.to_upper()}": "1"
    },
  }
  ```

### yield

`yield` produces an element into a stream. A function that uses `yield` is a
generator — its return type is `Stream<T>` where `T` is the type of yielded
values.

```
fn rustc_json(process: Process) -> Stream<RustcEvent> = {
  for line in process.stdout_lines() {
    let msg = json_parse(line)

    match msg["reason"] {
      "compiler-artifact" => {
        for filename in msg["filenames"] {
          if filename.ends_with(".rmeta") {
            yield RMeta(store.commit(filename))
          }
        }
      }
      "compiler-message" => {
        yield Diagnostic(...)
      }
    }
  }
}
```

`yield` does not suspend or return. The generator continues executing after
each yield. Yielded elements are cached by the runtime and available to
consumers via `find`, `filter`, `map`, etc.

This is the mechanism behind pipelining: a monitor function passed to `exec`
runs on the executor, parsing process output and yielding typed events as
they appear. Consumers block on specific events (e.g. `events.find($ as
RMeta)`) without waiting for the entire stream to complete.

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

### Named arguments

All arguments are named at call sites using `name: value` syntax:

```
exec(
  cmd:   [tc.rustc, "--edition", "2021"],
  mount: { "/src": src },
  parse: rustc_json,
)

download(url: manifest_url, sha256: checksum)
```

When passing a binding whose name matches the parameter, use `@` as shorthand:

```
let url = "https://..."
let sha256 = "abc123..."
download(@url, @sha256)
// equivalent to: download(url: url, sha256: sha256)
```

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

### impl blocks

`impl` adds methods to a named type. No traits — just methods on types.

```
impl Blob {
  fn read_utf8() -> String { ... }
  fn read_bytes() -> Bytes { ... }
}

impl String {
  fn toml() -> TomlValue { ... }
  fn json() -> JsonValue { ... }
  fn decode<T>() -> T { ... }
}
```

Methods enable pipeline-style chaining:

```
src["Cargo.toml"].read_utf8().toml().decode<CargoManifest>()
```

Some `impl` blocks are builtins (provided by the runtime). User code can
also define `impl` blocks for its own types.

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
// std/rust/build.vx — part of the standard library

fn compile_crate(src: Tree, deps: [...], tc: Toolchain) -> Stream<RustcEvent> {
  ...
}

fn link_binary(crates: [...], tc: Toolchain) -> Blob {
  ...
}
```

### use

`use` imports names from other modules. Module paths are dot-separated,
matching the file hierarchy (`std.rust.types` → `std/rust/types.vx`).

Selective imports list the names to bring into scope:

```
use std.rust.types { Toolchain, BuildCtx, Triple }
use std.rust.build { build_root }
use std.rust.monitor { rustc_json, parse_build_script_stdout }
```

Multiple names from the same module go in a single `use`:

```
use std.rust.types {
  BuildCtx, BuiltCrate, RustcEvent, RMeta, RLib, Diagnostic,
  ResolvedPkg, ResolvedEdge, BuildScriptOut, Role,
}
```

Enum variants are imported by name (since variants are standalone types):

```
use std.rust.types { Role, Target, Host }
```

## Host builtins

The primitive set is small. Everything else should be written in Vixen.

### exec

Runs a sandboxed process. Generic over the event type yielded by the parser.

```
exec(
  cmd:   [program, args...],
  env:   { KEY: "value", ... },
  mount: { "/path": tree_or_blob, ... },
  parse: parser_function,
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
cmd: [
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
download(url: String, sha256: Option<String>) -> Blob
```

When `sha256` is provided, the downloaded content is verified against it.
When omitted, the download is trusted — use this for mutable resources like
channel manifests where the hash isn't known ahead of time. The result is
still content-addressed once stored.

### source_tree

Returns the project source as a content-addressed tree.

```
source_tree() -> Tree
```

### Blob methods (builtin)

- `blob.read_utf8() -> String` — decode as UTF-8
- `blob.read_bytes() -> Bytes` — raw bytes
- `blob.blake3() -> String` — content hash

### String methods (builtin)

Parsing:

- `string.toml() -> TomlValue` — parse as TOML
- `string.decode<T>() -> T` — decode a parsed value into a typed struct
- `json_parse(text) -> JsonValue` — parse JSON (free function)

Testing:

- `string.starts_with(prefix) -> Bool`
- `string.ends_with(suffix) -> Bool`
- `string.contains(substring) -> Bool`

Manipulation:

- `string.strip_prefix(prefix) -> String` — remove prefix
- `string.split(separator) -> List<String>` — split into parts
- `string.split_once(separator) -> (String, String)` — split at first occurrence
- `string.to_upper() -> String` — uppercase
- `string.to_string() -> String` — convert to string (for non-string types like Triple)

Collection:

- `list.join(separator) -> String` — join a list of strings

### TomlValue (builtin)

`string.toml()` returns a `TomlValue`. This is a dynamically-typed
representation of parsed TOML, used for navigating manifest files before
decoding into typed structs.

- `value["key"]` — index into a table, fails if absent
- `value.get(key) -> Option<TomlValue>` — look up a key, returns nothing if absent
- `value.has(key) -> Bool` — test for key presence
- `value.as_string() -> String` — extract as string
- `value.as_bool() -> Bool` — extract as bool
- `value.as_list() -> List<TomlValue>` — extract as list
- `value.entries() -> List<(String, TomlValue)>` — table entries as key-value pairs

`json_parse()` returns a `JsonValue` with the same API.

### Other builtins

- `write_file(content) -> Blob` — create a pathless blob in the store
- `empty_tree() -> Tree` — an empty tree (no entries)
- `git_checkout(url: String, rev: String) -> Tree` — checkout a git repo at a
  specific revision
- `crates_io_registry() -> Registry` — the default crates.io registry
- `blob.extract_tree() -> Tree` — extract an archive blob into a tree

### Executor-side APIs

These are available inside monitor functions — the `parse` argument to `exec`.
Monitors run on the executor alongside the sandboxed process.

#### Process

The `Process` value is passed to the monitor function. It provides access to
the running process's output:

- `process.stdout_lines() -> Stream<String>` — stdout as a stream of lines,
  delivered progressively as the process runs

#### store

The `store` object is available on the executor. It commits artifacts produced
by the sandboxed process into the content-addressed store:

- `store.commit(path) -> Blob` — reads a file from the executor filesystem,
  computes its BLAKE3 hash, and stores it. The file must be complete on disk
  before calling commit. The monitor relies on structured output (e.g. rustc's
  JSON) to know when a file is ready — not filesystem events.

Together, these are how monitors parse process output and yield typed events:

```
fn rustc_json(process: Process) -> Stream<RustcEvent> = {
  for line in process.stdout_lines() {
    let msg = json_parse(line)
    match msg["reason"] {
      "compiler-artifact" => {
        for filename in msg["filenames"] {
          if filename.ends_with(".rmeta") {
            yield RMeta(store.commit(filename))
          }
        }
      }
    }
  }
}
```

### Tree operations

- `tree["path"]` — index access, look up a blob by relative path
- `tree.get("path")` — like index, but returns nothing instead of failing
- `tree.entry(path) -> TreeEntry` — look up an entry with metadata
- `tree.subtree(path) -> Tree` — get a subtree rooted at path
- `tree.entries() -> Stream<TreeEntry>` — all entries
- `tree.glob(pattern) -> Stream<TreeEntry>` — entries matching a glob

#### TreeEntry

A `TreeEntry` represents a single entry in a tree:

- `entry.path` — full path within the tree (e.g. `"lib/libfoo.rmeta"`)
- `entry.stem` — filename without extension (e.g. `"libfoo"`)
- `entry.blob` — the content as a `Blob`
