# Concurrency in Vixen

Vixen code looks sequential. There is no `spawn`, no `async`, no `await`,
no channels. Parallelism emerges from the runtime analyzing data
dependencies. This document shows when and how that happens.

## Source 1: independent bindings

When two bindings don't depend on each other, the runtime can evaluate
them concurrently:

```vixen
fn build_both(src: Tree) -> Result[Tree, BuildError] {
  lib = compile_lib(src);     // these two don't depend on each other
  tests = compile_tests(src); // so the runtime can run them in parallel
  Tree.of({ "lib": lib, "tests": tests })
}
```

`compile_lib` and `compile_tests` both depend on `src` (which is already
resolved — it's a function argument), but neither depends on the other.
The runtime sees this and may evaluate them as separate tasks. The final
`Tree.of` depends on both, so it suspends until both are done.

This is the most common source of parallelism in a build system. A
workspace with 50 crates has many independent compilation units.

## Source 2: pending values from exec

`exec` returns a `Stream[T]`. Values derived from that stream are pending
until the executor produces them. Code that doesn't touch those pending
values keeps running:

```vixen
fn build_workspace(src: Tree) -> Result[Tree, BuildError] {
  // launch three exec calls — they run on separate executors concurrently
  core_artifacts = compile_crate(src, "core");
  utils_artifacts = compile_crate(src, "utils");
  docs = build_docs(src);

  // core_lib and utils_lib are pending — they come from exec streams
  core_lib = core_artifacts.find($.kind == "lib");
  utils_lib = utils_artifacts.find($.kind == "lib");

  // this depends on core_lib and utils_lib, so it suspends until both
  // resolve. meanwhile, docs keeps building independently.
  app = compile_crate_with_deps(src, "app", [core_lib, utils_lib]);

  // only here does everything join — all three must be done
  Tree.of({
    "lib": app,
    "docs": docs,
  })
}
```

The three `compile_crate` / `build_docs` calls produce pending values.
The runtime launches all three and only blocks where actual data
dependencies demand it.

## Source 3: stream operations

Stream operations like `find`, `filter`, and `map` create independent
pending values from the same stream:

```vixen
artifacts = exec[RustcArtifact](...);

// these are independent queries on the same stream
metadata = artifacts.find($.kind == "metadata");
lib = artifacts.find($.kind == "lib");
```

`metadata` might resolve before `lib` (rustc emits metadata early).
Anything that depends only on `metadata` can proceed without waiting
for `lib`. This is how pipelining works — a downstream crate starts
compiling as soon as its dependency's metadata is available, even though
the dependency's code generation is still running.

## Source 4: collection operations

Operations that process collections element-by-element can run elements
concurrently:

```vixen
fn compile_all(crates: Array[CrateSpec], src: Tree) -> Array[Blob] {
  crates.map((spec) => compile_crate(src, spec))
}
```

Each element of the `map` is independent. The runtime can evaluate
multiple elements concurrently — each `compile_crate` call may launch
its own `exec`, and those execs run on separate executors.

Similarly, `for` loops where the body doesn't depend on previous
iterations can be parallelized:

```vixen
results = [];
for crate_spec in workspace.crates {
  results.append!(compile_crate(src, crate_spec));
}
```

Each iteration appends an independent pending value. The `append!`
modifies the local `results` array (value semantics — no shared state),
but the actual compilation work happens concurrently on executors.

## What does NOT run concurrently

Sequential dependencies are respected:

```vixen
fn build_app(src: Tree) -> Result[Blob, BuildError] {
  core = compile_crate(src, "core");
  // app depends on core — this cannot start until core resolves
  app = compile_crate_with_deps(src, "app", [core]);
  app
}
```

`compile_crate_with_deps` needs `core` as an argument. The runtime
suspends that call until `core` resolves. There is no way to "start
early" — the data dependency is the scheduling constraint.

Loop bodies that depend on previous iterations are also sequential:

```vixen
acc = 0;
for item in items {
  acc += process(item);  // each iteration depends on the previous acc
}
```

Each iteration reads `acc` from the previous iteration, so they must
run sequentially. The runtime can see this — `acc` is reassigned in the
loop body and read in the next iteration.

## The mental model

Think of Vixen code as a dataflow graph, not a sequence of instructions:

- Each binding is a node.
- Each use of a binding is an edge.
- Nodes with no edges between them can run concurrently.
- Pending values are nodes whose producers are still running.

The sequential-looking syntax is just a convenient way to describe
the graph. The runtime recovers the parallelism from the dependency
structure. You never have to think about threads, tasks, or
synchronization — you just write code that says what depends on what,
and the scheduler does the rest.
