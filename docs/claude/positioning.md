# Positioning

Where Vixen sits relative to existing build systems.

## Build Systems à la Carte

The paper decomposes build systems into:

- **Tasks**: what work needs doing
- **Scheduler**: what order to do it in
- **Rebuilder**: how to decide what needs redoing

In that taxonomy, Vixen is:

- **Tasks**: not user-authored rules but runtime-discovered computations —
  `exec`, `acquire`, file/tree observations, and memoized pure functions. The
  computation graph is an artifact of evaluation, not the authoring model.
- **Scheduler**: demand-driven. Computations start when demanded values depend
  on them. Independent work runs in parallel. No static DAG traversal.
- **Rebuilder**: content-addressed and value-based. Semantic equality with
  early cutoff, not just "did we already run this command."

The paper's taxonomy becomes insufficient where Vixen diverges from classical
build systems:

- The primary authoring unit is a value definition, not a task
- The runtime graph contains both pure and effectful nodes
- Dependencies are discovered by ordinary evaluation, not declared upfront
- Memoization covers typed computations, not just external actions

## Comparison

| System | Primary unit | Dep discovery | Caching | Where Vixen differs |
|--------|-------------|---------------|---------|---------------------|
| Make | file + command | static, file-based | timestamps / file state | Vixen is content-based and value-based, not file-centered |
| Bazel | declared action / target | configured graph, explicit | action cache + artifact graph | Vixen has no rule-definition phase; graph emerges from evaluation |
| Shake | task with dynamic deps | dynamic during build | task result cache | Vixen goes further toward a language evaluator with first-class pure computations |
| Nix | derivation / store object | mostly explicit, pure-ish | content-addressed store | Vixen makes dynamic dependency discovery and demand-driven evaluation first-class |

### Make

Make propagates file changes through commands. Dependencies are mostly static.
Caching is timestamp-based. Build descriptions are imperative shell recipes.
Make has no concept of typed values, semantic equality, or content-addressed
storage.

### Bazel

Bazel organizes work as declared targets with explicit actions. The graph is
known before execution starts. This enables remote execution and aggressive
caching, but means build descriptions must be static and declarative. Vixen's
graph emerges from evaluation — there is no separate rule-definition phase.

### Shake

Shake is the closest relative. Dynamic dependencies, demand-driven scheduling,
memoized task results. The key difference: in Shake, the authoring unit is
still a task (an IO action that produces a result). In Vixen, the authoring
unit is a typed value. Pure computations between host effects are also memoized
and participate in early cutoff.

### Nix

Nix describes derivations purely and stores results content-addressed. Vixen
shares the content-addressed store and purity emphasis. The differences: Nix's
dependency graph is mostly known at evaluation time (derivations are explicit),
and Nix does not have demand-driven incremental recomputation with early
cutoff. A Nix rebuild re-evaluates the full expression; Vixen traces
dependencies and skips unchanged subgraphs.

## What Vixen is not

- Not a generic distributed compute platform
- Not a shell-oriented task runner
- Not a system where undeclared host state can influence results
- Not a static-DAG planner with nicer syntax

The scope is: deterministic build-shaped effects, explicit inputs, demand-driven
evaluation, value-based caching.
