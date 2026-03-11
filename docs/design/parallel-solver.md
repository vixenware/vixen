# Vixen Parallel Solver Notes

This note records the current design direction for Vixen's justified parallel
solver so the conversation is not lost while the implementation focus stays on
the nearer-term build sketches.

The goal is not "Cargo, but in Rust again". The goal is a generic runtime for
justified demand-driven search, with Cargo-like resolution as one important
instance written in Vixen itself.

## 1. Core idea

Vixen already has:

1. demand-driven evaluation
2. memoization by keyed inputs
3. automatic concurrency
4. structured failure / poison
5. provenance for discovered facts

The solver direction extends that model instead of replacing it.

The central idea is:

1. some inputs accumulate monotonically
2. some branch-local assumptions refine the search space
3. every globally visible exclusion or choice is backed by a replayable
   provenance object
4. the runtime explores and prunes the search space in parallel while
   preserving a deterministic final answer

## 2. What the runtime owns

Rust owns only generic machinery:

1. keyed node evaluation
2. memoization
3. joins / accumulation of monotone facts
4. branch contexts / search volumes
5. incumbent tracking
6. pruning
7. frontier scheduling
8. structured conflict capture
9. replay of proven exclusions

Rust must not know domain semantics such as:

1. semver
2. Cargo features
3. registry metadata meaning
4. widening rules
5. package ordering policy
6. ecosystem-specific compatibility logic

Those belong in Vixen code.

## 3. What the Vixen program owns

A Vixen program should define:

1. the key space of the problem
2. the join semantics of accumulated facts
3. the shape of conflicts
4. how a conflict widens into one or more reusable exclusions
5. how to compare two complete solutions
6. how metadata change points are detected

Cargo, npm, pip, Maven, or some future planning problem should be expressible
by changing Vixen code, not by teaching the Rust runtime new solver tricks for
each domain.

## 4. Semantic volumes and conflict clusters

The current solver direction treats the search space as volumes inside conflict
clusters.

### 4.1 Volumes

A volume is a region of admissible solutions, not a single point. For package
resolution it can be described by:

1. an interval map of allowed versions per package
2. a set of fixed assumptions such as selected versions or enabled features
3. environmental witnesses such as target triple or host facts

The exact representation can change. The important thing is that volumes are
search-state values, not hardcoded Cargo concepts.

### 4.2 Conflict clusters

Clusters are dynamic connected components of the dependency / incompatibility
graph.

They matter because:

1. independent clusters can be searched concurrently
2. exclusions learned inside one cluster can stay local until a bridge fuses
   two clusters
3. fusion invalidation must be explicit and replayable

Cluster fusion is an optimization and scheduling concern. It is not a user-level
language concept.

## 5. Dominance and determinism

Parallel search is only acceptable if the final answer is deterministic.

The current direction is:

1. define a total order over complete resolved graphs
2. keep the best known valid graph as the incumbent
3. prove that pruned regions cannot contain a graph better than the incumbent

For Cargo-like resolution, the candidate dominance rule is:

1. order packages by minimum topological depth from the root set
2. tie-break by package name
3. compare two complete graphs at the first package where they differ
4. the graph with the higher version at that package wins

This rule must be nailed down precisely before the runtime can claim:

1. parallel search
2. Cargo-compatible determinism
3. sound dominance pruning

## 6. Why bound soundness matters

Pruning is sound only if the bound for a volume is genuinely optimistic under
the same dominance order.

The safe proof shape is intentionally boring:

1. lift every graph to a total vector over a fixed package universe
2. define `None < Some(version)`
3. define the bound of a volume as the pointwise maximum admitted value for
   each package slot
4. prove that every feasible graph inside the volume is pointwise `<=` that
   bound
5. conclude that if `Bound(V) <= Incumbent`, then pruning `V` is safe

Metadata-aware splitting improves pruning power. It is not the proof of
correctness.

## 7. Three execution tiers

The current converged architecture has three tiers.

### 7.1 Tier 1: causal parallel descent

Goal: find a valid incumbent fast.

Characteristics:

1. independent branches explore concurrently
2. coarse heuristic exclusions may be used locally to pivot workers away from
   obviously bad neighborhoods
3. these heuristic exclusions are not globally trusted facts

The first valid graph found becomes the incumbent.

### 7.2 Tier 2: squeeze

Goal: prove that nothing better than the incumbent exists.

Characteristics:

1. every remaining volume gets an optimistic bound
2. if `Bound(V) <= Incumbent`, the whole volume is discarded
3. volumes are split at metadata change points to maximize pruning power

This is where the exactness story lives. The point is not just "find a valid
answer", but "prove there is no better one."

### 7.3 Tier 3: diagnostic fallback

Goal: explain global failure.

If no incumbent exists, or all regions are pruned as impossible:

1. run a high-precision audit search
2. produce a structured explanation
3. distinguish:
   1. environmental witnesses
   2. joined facts
   3. dominance pruning
   4. proven exclusions

This tier is about trust and debuggability, not speed.

## 8. Justified facts and provenance

The most important part of the design is not the search. It is the justification
model.

Every globally visible exclusion or decision should be backed by a provenance
object.

The current taxonomy is:

1. manifest-proven exclusions
2. build-proven exclusions
3. environmental witnesses

These facts should be:

1. replayable
2. inspectable
3. cacheable
4. transportable across runs

Heuristic local exclusions used only to speed up Tier 1 must stay clearly
outside this trusted fact set.

## 9. Language-level pressure

The solver discussion exposed a language/runtime question.

We do not want:

1. manual worklists
2. explicit "repeat until stable" loops as the semantic story
3. Cargo-specific builtins in Rust

We probably do want a language-level way to express:

1. keyed nodes
2. monotone joined inputs
3. branch-local assumptions
4. structured conflicts

There are two plausible surface directions.

### 9.1 Branch-local assumptions

One option is a parameter class for branch-local search assumptions. In prose:

1. `#[key]` identifies the canonical node
2. `#[join]` accumulates monotone facts
3. branch-local assumptions are supplied by the runtime and are not globally
   joined

This keeps the runtime generic and keeps search policy mostly outside the
function signature.

### 9.2 Search-variable sugar

Another option is a more explicit search-variable surface such as `#[choose]`.

This can be a later ergonomic layer over branch-local assumptions, but should
not force Cargo logic into Rust.

The open design question is:

1. should the first implementation expose branch-local assumptions directly?
2. or should it expose a smaller sugar like `#[choose]` from the start?

This note does not settle that question.

## 10. What we should not do

We should avoid these traps:

1. implementing Cargo resolution as an imperative resolver loop in the sketch
2. treating topological order as a semantic product
3. hardcoding semver or feature logic in Rust
4. letting heuristic no-goods leak into the globally trusted exclusion set
5. treating "run a closure until the graph stops changing" as the language
   story

The fixpoint is a property of the graph. It should not be explained to users as
"we just loop until stable."

## 11. Research and implementation posture

The solver is a serious research topic. It is fine for the implementation to
start smaller than the end-state, but the design should not pretend that
single-threadedness is the language model.

Concurrency is already core to Vixen.

So the intended direction is:

1. keep the semantics parallel-friendly from day one
2. keep the runtime generic from day one
3. allow a simpler initial implementation if needed
4. but do not design the model around sequential control flow

In short:

1. the semantics should already speak in terms of graphs, volumes, and
   justified facts
2. the implementation may stage features
3. the design should not collapse into "just write Cargo in Rust, then maybe
   parallelize later"

## 12. Why this matters

If this works, Vixen is not just "a build language that can run a solver."

It becomes a host for justified demand-driven planning:

1. package resolution
2. toolchain selection
3. CI/test planning
4. deployment/configuration resolution
5. other domains where multiple facts accumulate, search is required, and
   explanations matter

The novelty is not only speed.

The novelty is:

1. speed
2. provenance
3. replayability
4. determinism
5. generic domain logic written in Vixen itself
