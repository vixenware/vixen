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

Two decisions from the language discussion matter here:

1. join semantics belong to the value type, not to the runtime
2. optimization preference belongs to an ordinary runtime value, not to a type
   annotation

In practice that means:

1. `#[join]` does not mean `#[join(Union)]` or `#[join(Intersect)]`
2. the runtime only needs to know that a parameter is joinable
3. the actual operation is provided by the type's `join(...)` method
4. search direction such as "prefer newest" vs "prefer oldest" should be an
   ordinary value such as `prefer: Fn(T, T) -> Bool`

This keeps the runtime generic and allows domains to define their own lattice
types and preference policies in Vixen.

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

For Cargo-like resolution, the natural "find a floor fast" strategy is:

1. run with the minimization preference first
2. obtain a valid lowest-feasible graph if one exists
3. use that graph as the initial incumbent

This matters because:

1. the floor is itself a correct answer
2. every later search tier becomes optimization over a known-valid solution
3. if optimization is interrupted, the floor remains usable

### 7.2 Tier 2: squeeze

Goal: prove that nothing better than the incumbent exists.

Characteristics:

1. every remaining volume gets an optimistic bound
2. if `Bound(V) <= Incumbent`, the whole volume is discarded
3. volumes are split at metadata change points to maximize pruning power

This is where the exactness story lives. The point is not just "find a valid
answer", but "prove there is no better one."

The important optimization idea here is range widening:

1. a conflict is first discovered at a point in the search space
2. user-defined widening logic scans adjacent metadata regions
3. if the same dependency declaration / incompatibility persists across a
   contiguous range, the runtime may exclude the whole range instead of only
   the original point
4. metadata change points determine the natural boundaries of safe widening and
   splitting

So:

1. point conflicts are the minimum correctness story
2. widened exclusions are the practical pruning story
3. the widening logic itself still belongs to Vixen code, not Rust

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

Range-widened exclusions are still trusted only if the widening argument is
itself justified by metadata or replayable evidence. Otherwise they remain
heuristic hints and must not be persisted as proven facts.

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

One useful way to think about the relationship is:

1. branch-local assumptions are the probable semantic primitive
2. `#[choose]` is plausible surface sugar over those assumptions
3. neither one should move semver, feature logic, or conflict widening into the
   runtime

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

Likewise, the runtime should not be described as "rerunning a closure until the
graph stops changing." The intended story is:

1. nodes have keys
2. joined inputs widen monotonically
3. wider demands cause the relevant nodes to be re-evaluated
4. the graph converges by ordinary demand-driven recomputation

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

## 12. Prototype order / MVP path

The architecture above is the destination. The route there still matters.

The intended prototype order is:

1. convergent demand only
2. structured conflicts
3. branch-local assumptions
4. exact search
5. learned point no-goods
6. range widening
7. metadata-aware splitting
8. parallel frontier work

### 12.1 Convergent demand only

Start with:

1. `#[key]`
2. `#[join]`
3. keyed memoization with join-on-collision

This is already enough for domains such as:

1. Go minimal version selection
2. pure feature unification
3. NixOS-style option merging

The runtime still knows nothing about Cargo.

### 12.2 Structured conflicts

The next step is not search. It is structured failure.

Resolver code should be able to `fail Conflict { ... }` with domain-specific
data. The runtime only needs to preserve and replay that value with provenance.

This gives:

1. precise explanations before optimization exists
2. a later input to no-good learning and widening

### 12.3 Branch-local assumptions

This is the first real search primitive.

The runtime must be able to evaluate the same convergent graph under different
branch-local assumption contexts without turning those assumptions into global
joins.

This can exist before any aggressive parallel machinery.

### 12.4 Exact search

Once branch-local assumptions exist, the runtime can:

1. explore a branch
2. observe success or structured conflict
3. split into narrower branches
4. compare complete solutions by the user-defined dominance order

This is already enough for a correct solver, even without no-good learning.

### 12.5 Learned point no-goods

The first learning step should be very conservative:

1. user code widens nothing
2. each discovered conflict can at least become a point exclusion
3. proven point exclusions become replayable facts

This validates the justified-fact model before any aggressive widening logic
appears.

### 12.6 Range widening

Only after point no-goods work should user-defined widening logic arrive.

The key idea is:

1. scan adjacent versions or adjacent metadata regions
2. detect where the same incompatibility declaration still holds
3. publish one larger exclusion instead of many point exclusions

This is where the geometry of volumes starts paying off.

### 12.7 Metadata-aware splitting

Once widening exists, the runtime should split only at metadata change points
rather than at arbitrary version boundaries.

That keeps the search aligned with real declaration changes instead of raw
version count.

### 12.8 Parallel frontier work

Parallelism is not the semantic core, but it is not an afterthought either.

The language/runtime model should remain parallel-friendly from day one. But
the most aggressive frontier scheduling, cluster fusion, and squeeze
optimizations can arrive after:

1. convergent demand
2. structured conflict
3. branch assumptions
4. exact search
5. learned exclusions

That keeps the implementation staged without redefining the semantics as
sequential.

## 13. Why this matters

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
