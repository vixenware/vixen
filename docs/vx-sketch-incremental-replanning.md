# Vixen Sketches: Incremental Re-planning

This document sketches how Vixen's rule language interacts with incremental
re-planning — what happens when one file changes and the build graph needs to
be updated without replanning everything from scratch.

This is where the rule language meets picante (the incremental query runtime).
The interesting question is not "can picante memoize?" — it can. The
interesting question is what the rule language surface looks like when rules are
designed to be incrementally re-evaluated.

## The Problem

A full build plans every target, emits every node and edge, and produces a
complete build graph. That is fine for a clean build. But for the common case
— one file changed, rebuild what is affected — replanning the entire graph is
wasteful.

The incremental story has three layers:

1. **Execution-level incrementality.** The action graph already has IfChanged
   edges. If a source file changes, only actions downstream of that edge need
   to re-execute. This works today and does not require any rule-language
   support.

2. **Planning-level incrementality.** If the set of targets has not changed
   (no new files, no manifest edits), the build graph itself is stable. There
   is nothing to replan. The executor just re-evaluates which actions are dirty
   based on file timestamps or content hashes.

3. **Structural incrementality.** If the project structure changes (a new
   source file is added, a dependency is added to a manifest, a feature flag
   is toggled), the build graph itself must change. This requires replanning,
   but only the affected subgraph.

Layer 3 is the hard one. It is where the rule language matters.

## How Picante Helps

Picante (Vixen's incremental query runtime) provides memoized, dependency-
tracked computation. The key properties:

- queries are pure functions of their inputs
- picante tracks which inputs each query read
- when an input changes, only queries that transitively depend on it are
  re-evaluated
- if a re-evaluated query produces the same output, its downstream dependents
  are not re-evaluated

This means if rule functions are modeled as picante queries, incremental
replanning comes from the runtime, not from careful hand-coding in the rule
language.

## Assumed Domain Types

```text
struct RustProject {
  crate_graph: CrateGraph,
  config: BuildConfig,
}

struct CrateGraph {
  crates: [CrateNode],
  topo_order: [CrateId],
}

struct CrateNode {
  id: CrateId,
  crate_name: String,
  crate_type: CrateType,
  sources: [SourceFile],
  deps: [DepEdge],
  edition: String,
}

struct CrateId {
  value: String,
}

enum CrateType {
  Binary,
  Library,
}

struct DepEdge {
  extern_name: String,
  crate_id: CrateId,
}

struct SourceFile {
  path: String,
}

struct BuildConfig {
  platform: Platform,
  profile: String,
  target_triple: String,
}

enum Platform {
  Linux,
  MacOS,
  Windows,
}
```

## Example 1: Per-crate planning functions

The key to incremental replanning is granularity. Instead of one function that
plans the entire graph, break planning into per-crate functions.

```text
fn plan_project(project: RustProject) -> Unit throws PlanError {
  project.crate_graph.crates.each((krate) =>
    plan_crate(krate, project.crate_graph, project.config)
  )
}

fn plan_crate(
  krate: CrateNode,
  graph: CrateGraph,
  config: BuildConfig,
) -> Unit throws PlanError {
  plan_compile(krate, config)
  plan_deps(krate, graph)

  if krate.crate_type == Binary {
    plan_link(krate, config)
  }
}

fn plan_compile(krate: CrateNode, config: BuildConfig) -> Unit {
  compile_id = format::rustc_id(krate.crate_name)

  emit::node {
    id: compile_id,
    kind: Action,
    label: `rustc ${krate.crate_name}`,
    metadata: {
      crate_name: krate.crate_name,
      crate_type: match krate.crate_type {
        Binary  => "bin"
        Library => "lib"
      },
      edition: krate.edition,
    },
  }

  krate.sources.each((src) => emit::edge {
    from: format::source_id(src.path),
    to: compile_id,
    kind: IfChanged,
  })
}

fn plan_deps(krate: CrateNode, graph: CrateGraph) -> Unit throws PlanError {
  compile_id = format::rustc_id(krate.crate_name)

  krate.deps.each((dep) => {
    dep_krate = graph.lookup_crate(dep.crate_id)

    emit::edge {
      from: format::rustc_id(dep_krate.crate_name),
      to: compile_id,
      kind: IfChanged,
    }
  })
}

fn plan_link(krate: CrateNode, config: BuildConfig) -> Unit {
  link_id = format::link_id(krate.crate_name)

  emit::node {
    id: link_id,
    kind: Action,
    label: `link ${krate.crate_name}`,
    metadata: { target_name: krate.crate_name },
  }

  emit::edge {
    from: format::rustc_id(krate.crate_name),
    to: link_id,
    kind: Always,
  }
}
```

What this shows:

- each crate is planned independently by `plan_crate`
- if picante models `plan_crate(krate, graph, config)` as a tracked query,
  then changing one crate's source list only re-evaluates that crate's planning
- if the re-evaluated plan produces the same nodes and edges (e.g. adding a
  comment to a source file does not change the source list), downstream
  planning is not re-evaluated

## Example 2: What changes when a source file is added

Suppose `src/new_module.rs` is added to a library crate.

The input change is: `krate.sources` gains one entry.

What happens:

1. The host detects that the crate manifest or file system changed.
2. The host re-parses the crate graph. The `CrateNode` for this crate now has
   an additional `SourceFile`.
3. Picante sees that the input to `plan_crate` for this crate has changed.
4. `plan_crate` re-runs for this crate only.
5. `plan_compile` emits the same compile node but with one additional
   IfChanged edge (from the new source file).
6. `plan_deps` and `plan_link` produce identical output — the dependency
   structure did not change.
7. The resulting graph diff is: one new edge (source → compile).

No other crate's planning is re-evaluated.

In the rule language, this requires **no special code**. The incrementality
comes from picante tracking which `CrateNode` each `plan_crate` call read.

## Example 3: What changes when a dependency is added

Suppose crate `app` adds a new dependency on crate `utils`.

The input change is: `app.deps` gains one `DepEdge`.

What happens:

1. The host re-parses the crate graph. `app`'s `CrateNode` has a new dep.
2. Picante re-runs `plan_crate` for `app`.
3. `plan_deps` emits one additional IfChanged edge (from `utils` compile to
   `app` compile).
4. `plan_compile` and `plan_link` for `app` produce identical output.
5. `plan_crate` for `utils` is not re-evaluated — its inputs did not change.

Graph diff: one new edge.

## Example 4: What changes when a toolchain version changes

The input change is: `config.toolchains.rust.version` changes.

What happens:

1. The host provides an updated `BuildConfig`.
2. Every `plan_crate` call received `config` as an input. Picante sees that
   `config` changed.
3. All `plan_crate` calls are re-evaluated.
4. Each `plan_compile` emits the same nodes, but the metadata includes the
   new toolchain version.
5. Each `plan_link` emits the same nodes with updated linker metadata.
6. The graph diff is: updated metadata on every compile and link node.

This is the one case where re-evaluation is broad. But it is correct — a
toolchain change really does affect every compilation.

The rule language can mitigate this by not passing the full config to functions
that do not need it:

```text
fn plan_compile(krate: CrateNode, edition: String) -> Unit {
  // only depends on krate and edition, not on the full config
  // ...
}
```

If `plan_compile` does not receive the toolchain version, picante does not
re-evaluate it when only the toolchain changes. The toolchain dependency is
expressed as a graph edge (see toolchain-acquisition sketch), not as a
planning-function input.

This is a real design tension:

- putting toolchain info in the planning function makes it visible in the rule
  code
- putting it only in graph edges makes planning more incremental but less
  transparent

The right answer is probably: rule code should not know the toolchain path.
The executor resolves toolchain paths from the graph. Rule code emits a
"depends on Rust toolchain" edge, and the executor fills in the details.

## Example 5: Structural incrementality with feature flags

Suppose a crate has a feature flag that changes its dependency set.

```text
fn plan_crate_with_features(
  krate: CrateNode,
  graph: CrateGraph,
  features: [String],
) -> Unit throws PlanError {
  plan_compile(krate, krate.edition)

  // Base deps always present
  plan_deps(krate, graph)

  // Feature-conditional deps
  if features.contains("json") {
    emit::edge {
      from: format::rustc_id("serde_json"),
      to: format::rustc_id(krate.crate_name),
      kind: IfChanged,
    }
  }

  if features.contains("compression") {
    emit::edge {
      from: format::rustc_id("flate2"),
      to: format::rustc_id(krate.crate_name),
      kind: IfChanged,
    }
  }

  if krate.crate_type == Binary {
    plan_link(krate, krate.edition)
  }
}
```

What happens when a feature is toggled:

1. The input `features` changes.
2. Picante re-runs `plan_crate_with_features` for this crate.
3. The conditional edges change — e.g. the `serde_json` edge appears or
   disappears.
4. Other crates are not affected (unless their own feature sets changed).

Graph diff: edges added or removed for the toggled features.

## The Rule Language's Role in Incrementality

The rule language does **not** implement incrementality. It does not call
picante APIs, manage caches, or track dependencies. It writes pure planning
functions.

What the rule language **does** is:

1. **Structure planning into fine-grained functions.** The granularity of
   incremental re-evaluation is determined by how planning is decomposed.
   One monolithic `plan_everything` function means everything replans on any
   change. Per-crate functions mean only affected crates replan.

2. **Minimize input surfaces.** Functions that take only what they need
   (a single `CrateNode`, not the full `CrateGraph`) are cheaper to
   re-evaluate because fewer input changes trigger them.

3. **Produce stable output.** If a function's output is the same despite
   its input changing (e.g. adding a comment to a source does not change the
   source list), picante skips downstream re-evaluation. Rule functions that
   produce deterministic output from their inputs get this for free.

These are ordinary good-programming-practice concerns, not special incremental-
programming techniques. The runtime does the hard work.

## What Incremental Re-planning Reveals About Vixen

1. **The rule language is deliberately not incremental-aware.** Rule functions
   are pure, total, and deterministic. Picante provides the incrementality.
   This is the right separation of concerns — rule authors think about
   correctness, the runtime thinks about efficiency.

2. **Function granularity is the main design lever.** Per-crate planning
   functions are the natural unit. Going finer (per-source-file planning)
   is probably not worth the complexity. Going coarser (whole-project planning)
   defeats incrementality.

3. **Input minimization matters.** Passing `config` to every function means
   every function replans when config changes. Passing only the fields each
   function actually needs keeps re-evaluation narrow. This is an ordinary
   API design concern, not a special language feature.

4. **Graph edges carry the execution-level incrementality.** IfChanged edges
   mean "re-execute if this input changed." That is independent of whether
   planning was incremental — even a full replan produces the same graph,
   and the executor still only re-executes dirty actions.

5. **The two levels are complementary.** Planning-level incrementality (via
   picante) avoids re-emitting unchanged graph fragments. Execution-level
   incrementality (via IfChanged edges) avoids re-running unchanged actions.
   Both are needed. Neither requires special rule-language support.
