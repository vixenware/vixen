# Building Rust with Vixen

This document introduces Vixen's language and execution model through a
concrete example: compiling Rust code. It starts simple and builds up, letting
you discover the semantics as they become necessary.

For aspirational Vixen source files that encode a full cargo build — including
dependency resolution, feature activation, build scripts, host/target roles,
and lockfile support — see [examples/std/rust/](examples/std/rust/).

## The shape of a Rust build

Here is what building a Rust crate looks like in Vixen. This is the design
target — not all syntax is final, but the shape is right.

```
fn build_pkg(pkg: ResolvedPkg, ctx: BuildCtx) -> BuiltCrate =
  memo(pkg.id, pkg.role, pkg.enabled_features, ctx.host, ctx.target) {
    let target_deps =
      pkg.deps
        .filter($.role == Target)
        .map(|edge| edge.extern_name => build_pkg(edge.pkg, ctx))
        .into_map()

    let externs = tree {
      for (name, dep) in target_deps {
        "lib${name}.rmeta" = dep.rmeta
      }
    }

    let events = rustc_events(pkg, externs, build_out, ctx)

    {
      rmeta       = events.find($ as RMeta).0,
      rlib        = events.find($ as RLib).0,
      diagnostics = events.filter($ as Diagnostic).map($.0),
    }
  }
```

This reads like ordinary code. Parse the manifest, resolve dependencies,
recursively build them, prepare inputs, run rustc, project the outputs.

The important line is:

```
"lib${name}.rmeta" = dep.rmeta
```

`dep.rmeta` is a lazy projection from the dependency's rustc event stream. The
current crate can start compiling as soon as each dependency has emitted its
`RMeta` event. It does not wait for `dep.rlib`. That is the pipelining story.

The returned value is ergonomic:

```
let built = build_crate(src, tc)
built.rmeta       // blocks until rmeta is available
built.rlib        // blocks until rlib is available
built.diagnostics // a stream of warnings/errors
```

The struct is ordinary. Its fields are lazy computations over a shared stream.
No mutation, no half-filled struct, no promise type.

## How it works, piece by piece

### Types

```
struct BuiltCrate {
  rmeta: Blob
  rlib: Blob
  diagnostics: Stream<RustDiagnostic>
}

enum RustcEvent {
  RMeta(Blob)
  RLib(Blob)
  Diagnostic(RustDiagnostic)
  Finished(ExitStatus)
}
```

`BuiltCrate` is a regular struct. Its fields happen to be demand-driven — they
won't be computed until someone accesses them. But there is nothing special
about the type itself.

`RustcEvent` is an enum with tuple-style variants. `RMeta` is both a variant
of `RustcEvent` and a standalone type. `events.find($ as RMeta)` narrows the
stream to `RMeta` values; `.0` extracts the inner `Blob`.

### Parsing the manifest

```
fn parse_manifest(src: Tree) -> CargoManifest =
  src["Cargo.toml"]
    .read_utf8()
    .toml()
    .decode<CargoManifest>()
```

`src["Cargo.toml"]` indexes into the tree. `.read_utf8()` decodes the blob.
`.toml()` parses TOML. `.decode<CargoManifest>()` maps it to a typed value.

Each of these is a tracked computation boundary. If the file bytes change but
the parsed TOML is identical, downstream doesn't re-evaluate (early cutoff).

Expression-body functions use `=` instead of `{ ... }`.

### Resolving dependencies

```
fn resolve_deps(src: Tree, cargo: CargoManifest) -> List<ResolvedCrate> =
  crates_io.resolve(
    name    = cargo.package.name,
    version = cargo.package.version,
    deps    = cargo.dependencies,
    lock    = src.get("Cargo.lock").map(parse_lock),
  )
```

`src.get("Cargo.lock")` returns the entry if it exists, or nothing. `.map()`
applies the function only if the value is present.

Resolution is a memoized computation. The same dependency set resolves the
same way.

### Running rustc

```
fn rustc_events(
  tc: Toolchain,
  src: Tree,
  externs: Tree,
  cargo: CargoManifest,
) -> Stream<RustcEvent> =
  exec(
    cmd = [
      tc.rustc,
      "--crate-name", cargo.package.name,
      "--edition", cargo.package.edition,
      "--crate-type", "lib",
      "--emit", "metadata,link",
      "--out-dir", "/out",
      "-L", "dependency=/deps",

      for entry in externs.entries() {
        "--extern", "${entry.stem}=${entry.path}"
      },

      "/src/src/lib.rs",
    ],

    mount = {
      "/src": src,
      "/deps": externs,
    },

    parse = rustc_json,
  )
```

`exec` runs a sandboxed process. It takes:
- `cmd`: the command and arguments. Supports inline `for` comprehensions.
- `mount`: named trees mounted at paths in the sandbox.
- `parse`: a function that parses the process output and yields typed events.

The process runs in isolation. Only mounted inputs are visible. No network,
no ambient host state. This is what makes memoization sound.

`exec` returns `Stream<RustcEvent>`. The parser (`rustc_json`) runs on the
executor alongside the process, parsing JSON stdout and yielding events as
artifacts and diagnostics appear.

### Tree literals

```
let externs = tree {
  for (name, dep) in built_deps {
    "lib${name}.rmeta" = dep.rmeta
  }
}
```

`tree { ... }` builds a `Tree` value. Inside the block, string keys map to
blob values. The `for` loop generates entries. Each entry is a path-to-blob
mapping.

`dep.rmeta` is a lazy value — it blocks until the dependency's metadata is
available. The tree construction itself is lazy: the tree isn't fully realized
until something demands it. But once any entry is demanded (e.g. `externs`
being mounted for an exec), the runtime materializes the needed blobs.

### Recursion and memoization

```
fn build_dep(dep: ResolvedCrate, tc: Toolchain) -> BuiltCrate =
  build_crate(fetch_source(dep), tc)
```

`build_crate` calls `build_dep` which calls `build_crate`. This is
recursive. The runtime memoizes at function boundaries: if two crates depend
on the same package, `build_dep(dep, tc)` runs once.

No manual dependency ordering. No build graph construction. The dependency
graph emerges from function calls.

### Pipelining

The line that makes pipelining work:

```
rmeta = events.find($ as RMeta).0,
```

`events` is the stream from `rustc_events()`. `.find($ as RMeta)` blocks
until the monitor yields an `RMeta` event — which happens when rustc emits
metadata, before it finishes codegen. `.file` extracts the blob.

A dependent crate's `externs` tree references `dep.rmeta`. Constructing that
tree blocks on the dependency's rmeta. It does NOT block on the dependency's
rlib. So the dependent starts compiling as soon as metadata is ready.

### Diagnostics

```
diagnostics = events.filter($ as Diagnostic).map($.0),
```

Diagnostics are stream events. They're stored with the cached computation. On
a cache hit, the stream is replayed — same warnings surface regardless of
whether the build was fresh or cached.

Diagnostics flow to the user (via the CLI), not through the dependency graph.

## Streams

Streams are Vixen's mechanism for progressive, variable-length data. They show
up everywhere:

- `acquire(url, sha256)` → `Stream<AcquireEvent>` — archive entries
- `exec(cmd, mount, parse)` → `Stream<E>` — process output events
- `tree.entries()` → `Stream<TreeEntry>` — directory contents

Streams are:
- **Reusable**: multiple consumers share one producer. Elements are cached.
- **Ordered**: deterministic element order for a given input.
- **Lazy**: elements are produced on demand.

Key operations:
- `find(predicate)` — first match, blocks until found
- `filter(predicate)` — sub-stream of matches, progressive
- `map(f)` — transform elements, progressive
- `collect_tree()` — collapse into a Tree, blocks until complete
- `collect_list(order: ...)` — collapse into a List with explicit ordering

## `$` closures and `as`

`$` is shorthand for the anonymous closure argument:

```
list.map($ + 1)            // |x| x + 1
list.filter($.age > 18)    // |x| x.age > 18
events.find($ as RMeta)    // |x| x as RMeta
```

`as` narrows an enum value to a specific variant type. Because variants are
proper types, `event as RMeta` gives you an `RMeta` value (or nothing).

## Demand

None of this code runs eagerly. Work happens when demanded from outside:

```
vixen build my_crate.rlib
vixen test my_crate.tests
```

There is no `force`, no default target, no top-level side effects. A Vixen
file is a module. The outside world decides which values to realize.

## Memoization and early cutoff

The runtime records a dependency trace for each memoized computation. On
rebuild:

1. Check each dependency in the trace: did its value change?
2. If all match → cached result is valid. No recomputation.
3. If any changed → re-evaluate the computation.
4. After re-evaluation, compare the new output to the previous output.
5. If identical → **early cutoff**: downstream does NOT re-evaluate.

This is why the build is structured as many small tracked computations. Each
boundary is a potential cutoff point:

- File bytes change → reparse TOML → AST unchanged → cutoff
- Private function changes → rlib changes → rmeta unchanged → cutoff
- Reformatted source → same compiled output → cutoff

For the full execution model, see [runtime.md](runtime.md).
For the language reference, see [language.md](language.md).
For how Vixen compares to other build systems, see [positioning.md](positioning.md).
