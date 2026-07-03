# March Solver Discussion — Verbatim Companion

Source: `vixen-build-system-design-6a47d39d.md` (ChatGPT share export, conversation
dated 2026-03-11, 67 messages). All line numbers refer to that source file.
Companion to `march-solver-recovered.md`.

*Scissors, not pen. Everything inside a `>` block below is copied exactly from
the source. The only non-quoted text is the italic bracketed connective notes
between blocks, and the section headings, which mirror the digest's skeleton.
`[…]` marks a genuine elision of unrelated material inside an otherwise-quoted
passage; nothing is paraphrased or compressed.*

---

## 1. Thread map — verbatim cuts

### Convergent nodes (`#[key]`/`#[join]`)

**Amos — Msg 1, lines 73–92:**
> The Problem: Feature Unification
> In Cargo-style resolution, if two paths demand the same package with different features, those features must be unified. A standard memoized function resolve(pkg, features) would treat resolve(foo, {A}) and resolve(foo, {B}) as two different values, leading to graph duplication.
> The Solution: The cumulative Call-Site
> We introduce the cumulative keyword at the call site to signal that an argument is a Lattice Point.
> // Definition is a simple recursive rule
> fn resolve_pkg(id: String, features: Set[String]) (
>   let version = select_version(id, features);
>   version.deps.map((d) => 
>     resolve_pkg(d.id, cumulative d.features) // The CALL merges into a lattice
>   )
> )
>
> // Apex (Root) Call
> let resolution = resolve_pkg("root", cumulative ["default"]);
>
> The Semantic: Reactive Invalidation
>  * Unification: All calls to resolve_pkg with the same Identity (the id) within a single Apex (the root demand) are unified into a single node in the demand graph.
>  * Lattice Merge: The cumulative arguments from all callers are merged (e.g., via Set Union).
>  * Convergence: If a new demand "widens" the lattice (e.g., adding a new feature), the node is reactively invalidated and re-evaluated.
>  * No Loops: The "loop" is moved into the runtime's convergence engine. The user writes pure recursion; the runtime finds the stable state.

*[first `#[key]`/`#[join]` sketch, after the pushback on call-site-only `cumulative`]*

**Amos — Msg 3, lines 200–219:**
> The Final cargo-resolve.vx Logic
> The resolve_crate function now simply takes the exclusions as a third #[join] argument. The "Search" is handled by the reactive engine's propagation of these exclusions.
> #[convergent]
> fn resolve_pkg(
>   #[key] id: String,
>   #[join] features: Set[String],
>   #[join] version_req: SemverRange,
>   #[join] exclusions: Set[NoGoodConstraint], // The cumulative "No-Good" facts
> ) -> ResolvedCrate {
>
>   // find_best now filters out versions that, combined with 
>   // currently selected versions elsewhere, violate a NoGoodConstraint.
>   let best_v = registry.find_best(id, version_req, exclusions);
>
>   if (best_v == None) ( return ⊥ );
>
>   version.deps.map((d) => 
>     resolve_pkg(d.name, #[join] d.features, #[join] d.req, #[join] exclusions)
>   )
> }

**Amos — Msg 5, lines 333–337:**
> 1. The Core Data Model: Convergent Nodes
> Vixen replaces the traditional "Package Manager Loop" with a reactive demand graph. Functions involved in resolution are marked with attributes to define their role in the convergence.
>  * #[key] (Identity): Arguments that define the uniqueness of a node (e.g., PackageID).
>  * #[join] (Lattice): Arguments that contribute to a shared value via a monotonic merge (e.g., Set[Features] or SemverRange).
> When multiple parts of the graph demand serde, the runtime unifies them into a single node. The features are unioned, the version ranges are intersected, and the node re-evaluates reactively whenever a #[join] argument widens.

*[the "20 keywords" rejection, and the retreat back to exactly this core]*

**Amos — Msg 23, line 3079:**
> Sure but it doesn't fit Vixen's dataflow language with demand driven evaluation. You also just invented 20 keywords and made a system so flexible it's impossible to implement generically. Read the current spec to get a better idea of where things fit.

**Assistant — Msg 25, lines 3132–3163:**
> After reading the spec, I think the right move is **not** to invent a second language for search. The right move is to keep the existing shape:
>
> - `#[key]` for node identity
> - `#[join]` for monotone accumulated inputs
> - ordinary pure values / functions
> - runtime-managed demand graph
>
> and add only the **minimum new runtime concept** needed for exact search:
>
> ## The missing thing is not "20 keywords"
> It's one extra semantic dimension:
>
> **branch-local assumptions**
>
> Right now Vixen already has:
> - global / apex-scoped monotone facts via joins
> - pure evaluation over keyed nodes
>
> What exact resolution needs, and what monotone convergence alone could not express, is:
>
> - a way for the runtime to evaluate the same demand graph under a **specific search slice**
> - without turning that slice into a global join
> - and without hardcoding Cargo in Rust
>
> So I think the real extension is something like:
>
> - `#[key]` — persistent node identity
> - `#[join]` — monotone accumulated facts
> - `#[assume]` or equivalent — **branch-local refinement**, supplied by the runtime, not globally joined
>
> Not twenty things. Really one.

---

### Tiering (Tier 1 / 2 / 3)

**Amos — Msg 5, lines 338–354:**
> 2. The Tiered Resolution Strategy
> To maximize hardware utilization without sacrificing error precision, Vixen uses a three-tier execution model.
> Tier 1: Aggressive Parallel Search (The Fast Path)
> The engine assumes a valid solution exists and prioritizes throughput over precision.
>  * Speculative Branching: The engine spawns "Shadow Apexes" to explore different version choices (e.g., trying v1.2.0 and v1.1.9 in parallel).
>  * Coarse-Grained No-Goods: When a conflict occurs, the engine doesn't pause for complex analysis. It simply records the Conflict Set (the list of versions currently selected in that branch) as a No-Good fact and kills the branch.
>  * Simultaneous Shifting: All nodes participating in a conflict are notified. They may shift versions simultaneously, breaking symmetry with deterministic heuristics (e.g., lexicographic ID) to avoid oscillation.
> Tier 2: Sequential Refinement (The Fallback)
> If the aggressive Tier 1 search exhausts the space and concludes "No Solution," Vixen does not immediately error. It triggers a Sequential Fallback to account for "Over-Pruning."
>  * Tier 1's coarse No-Goods might have been too broad, accidentally excluding a valid region of the graph.
>  * Tier 2 runs a precise, serial CDCL (Conflict-Directed Clause Learning) algorithm to ensure no valid path was missed.
>  * If Tier 2 finds a solution, it succeeds. If it fails, the failure is now guaranteed.
> Tier 3: Diagnostic Trace (The "Explain" Path)
> Only after Tier 2 confirms a total failure does Vixen enter Diagnostic Mode.

**Amos — Msg 13, lines 1525–1536:**
> 3. Tiered Execution Strategy
> Tier 1: Parallel Greedy Descent (Finding the Floor)
> Vixen first attempts to find a valid "baseline" solution as fast as possible.
>  * Aggressive Exploration: Workers pick the highest available version for every package in parallel.
>  * Coarse No-Goods: When a conflict occurs, the worker identifies the transitive neighborhood of the failure and publishes a coarse exclusion range.
>  * Convergence: The first worker to reach a valid leaf publishes the first Incumbent. This "Floor" immediately prunes a massive volume of the search space.
> Tier 2: The Squeeze (Exhaustive Optimization)
> Once a Floor exists, the solver enters a formal optimization phase to find the Vixen-Optimal solution.
>  * Dominance Ordering: We define a stable, deterministic "Best" (\succ):
>    * G_1 \succ G_2 if, at the first package P (ordered by name) where they differ, Version(P, G_1) > Version(P, G_2).
>  * Frontier Splitting: Regions are split at Metadata Change Points (e.g., where a Cargo.toml dependency changes) rather than arbitrary version ticks.
>  * Range Widening: When a conflict is found, Vixen scans the registry to generalize the failure into a Forbidden Hyper-rectangle, pruning thousands of points at once.

**Amos — Msg 17, lines 2166–2185:**
> 3. Tiered Execution Strategy
> Tier 1: Parallel Greedy Descent (Floor Discovery)
> The engine attempts to find a valid "baseline" solution as fast as possible to seed the Incumbent.
>  * Causal Parallelism: Independent branches of the dependency tree are resolved concurrently.
>  * Heuristic Pruning: Conflicts trigger "Neighborhood No-Goods"—coarse, non-persistent exclusions used only to accelerate the discovery of the first valid floor.
> Tier 2: The Squeeze (Exhaustive Refinement)
> Once an Incumbent exists, the solver enters a formal optimization phase to find the Vixen-Optimal solution.
>  * Metadata-Aware Splitting: Volumes are not split at arbitrary versions, but at Metadata Change Points (e.g., where a Cargo.toml's dependencies or features change).
>  * Dominance Pruning: If Bound(\mathcal{V}) \preceq \mathcal{I}, the entire volume is discarded.
>  * Incompatibility Widening: When a conflict is proved, Vixen scans the registry to generalize the failure into a Proven Exclusion Range, pruning thousands of points at once.
> 4. Operational Components
> Conflict Clusters
> Packages are partitioned into Conflict Clusters—connected components of the dependency/incompatibility graph. Clusters are dynamic; they fuse if a newly discovered dependency or exclusion links two previously independent groups.

---

### Range widening

**Amos — Msg 7, lines 738–744:**
> 2. Range Widening: From "Point Facts" to "Lattice Facts"
> This is the "Research-Grade" accelerator you suggested. In a standard solver, if serde 1.0.10 fails, the solver just learns "not 1.0.10."
> In Vixen, when we learn a No-Good, we Scan the Registry to generalize the failure.
>  * The Logic: If A@1.0.10 failed because it required B <2.0, and B is currently at 3.0, the runtime doesn't just blame 1.0.10.
>  * The Widening: It checks 1.0.11, 1.0.12, etc. If they all have the same requirement (B <2.0), the No-Good is widened from a point-fact to a Range-Fact: NOT (A >= 1.0.10 AND B >= 2.0).

*[the embedded "Feedback from Claude" text, pasted by Amos inside his own turn]*

**Amos (quoting Claude) — Msg 7, lines 784–795:**
> The range-widening no-goods are a brilliant optimization
> This is the idea I'm most excited about. When you learn "A@1.4 is incompatible with B@2.0" because A@1.4 depends on C ^3.0 and B@2.0 needs C ^2.0, the reason for the incompatibility is A's dependency declaration on C. That declaration is registry metadata — it's free to read. So you scan:
> 	∙	A@1.5: depends on C ^3.0? Yes, same problem.
> 	∙	A@1.6: depends on C ^3.0? Yes.
> 	∙	A@1.3: depends on C ^3.0? Yes.
> 	∙	A@1.2: depends on C ^2.5? No — different constraint. Stop.
> Now your no-good isn't "A@1.4 + B@2.0 is bad." It's "A@[1.3..1.6] + B@2.0 is bad." One conflict discovery just pruned four versions instead of one. And you can do the same widening on the B side — scan to find the range of B versions that require C ^2.0.
> This is essentially free because you're reading dependency manifests, not building anything. The registry index is already fetched. And the payoff is enormous: instead of learning point exclusions and bumping into the same root cause repeatedly at adjacent versions, you learn range exclusions that carve out large chunks of the search space in one step.
> […]
> What to watch for
> The range-widening assumes the reason for incompatibility is stable across adjacent versions — that the dependency declaration on C doesn't fluctuate. In practice this is almost always true (dependency requirements change at feature boundaries, not every patch), but you should verify during the scan rather than assuming. If A@1.5 depends on C ^3.0 but A@1.5.1 switches to C ^2.0, the range has a hole. The scan needs to be precise, not interpolated.
> Also, the "reason" for incompatibility isn't always a single dependency declaration. Sometimes it's a conjunction — A@1.4 is bad with B@2.0 only when feature X is enabled, which pulls in C. Your widening scan needs to check the same feature-conditional path, not just the top-level deps. This is still cheap (feature-conditional deps are in the manifest), but it's an additional dimension to scan.

**Assistant — Msg 8, lines 836–848:**
> ### 1. Range widening is the best idea here
> This is the most convincing part.
>
> If the cause of failure is manifest-level metadata that is identical across adjacent versions, then learning:
>
> - not just `A@1.4 + B@2.0`
> - but `A@[1.3..1.6] + B@2.0`
>
> is a real win.
>
> That is basically clause generalization over registry metadata. It should help a lot because ecosystems usually have long stretches of versions with unchanged dependency declarations.
>
> This feels like a **real optimization**, not just a change in vocabulary.

**Assistant — Msg 41, line 4609:**
> **2. Range facts / widening:** the closest prior art to your "widen a point conflict into a range fact" idea is actually quite close. Dart's `pub` already collapses adjacent package versions with identical dependencies into single incompatibilities over version ranges, and it also groups adjacent versions that are unselectable for the same reason. So this part of Vixen is **not unprecedented**; your twist is pushing that idea higher up into the runtime as a first-class pruning/provenance object, rather than keeping it as an internal resolver optimization.

---

### Floor / incumbent

**Amos — Msg 7, lines 729–737:**
> 1. The Vixen Hybrid Strategy: The "Squeeze"
> Instead of Cargo's depth-first serial search, Vixen uses a two-phase "Squeeze" to find the Maximal Valid Graph (the Cargo-compatible "best" solution) using all available cores.
> Phase A: The Parallel Descent (Finding the Floor)
> Vixen doesn't pick one version at a time. It fires off demands for every package in the root manifest simultaneously.
>  * Greedy Bias: Every node attempts to select its MAX version.
>  * Reactive Conflict: When A@MAX and B@MAX conflict at a shared dependency D, they both "ripple" downward.
>  * Goal: Reach any valid solution as fast as possible. This creates a "Floor"—a known-buildable state for the entire graph.

**Assistant — Msg 8, lines 864–882:**
> ### 1. The "floor proves a solution exists" part is only true if the floor is actually valid
> The commentary slides between two different notions:
>
> - **minimum-version style floor**
> - **some valid lower baseline found by descent**
>
> Those are not the same.
>
> A minimum-version solution does **not** generally prove much unless it actually resolves successfully. In real ecosystems, lower versions can fail while higher versions succeed, because dependency metadata and feature structure are irregular.
>
> So this sentence:
>
> > the floor is cheap to compute — it's pure lattice convergence, no search, fully parallel
>
> is too optimistic.
>
> Finding **any valid floor** is still solving.
>
> You may have good heuristics for descending to one, but that is not the same as "pure lattice convergence."

**Amos — Msg 9, lines 1023–1026:**
> I agree finding a floor is a search and it is (parallel) work. Not the same as minimal versions, and not deterministic.
>
> Cargo compatibility IS the goal. Once we have a floor, the squeeze starts and doesn't stop until we find the same solution cargo would: using range facts to speed things up.

**Assistant — Msg 10, lines 1090–1105:**
> ## The real semantic center
>
> I think the right way to describe the solver now is:
>
> - **Constraint state** is monotone  
>   learned exclusions only grow
>
> - **Feasible region** is monotone  
>   candidate space only shrinks
>
> - **Incumbent solution** is monotone under Cargo's ordering  
>   each newly accepted full solution is strictly better than the previous floor
>
> That third point is important. The individual package picks are not monotone, but the **best complete solution found so far**, ordered the same way Cargo orders solutions, should be monotone improving.
>
> That is probably the cleanest formal story you have so far.

---

### The "squeeze" framing

**Assistant — Msg 8, lines 884–907:**
> ### 2. If you want Cargo-compatible output, "stop upgrading that cluster and keep the floor" is not acceptable
> This is the biggest mismatch.
>
> There are really two possible goals:
>
> 1. **Find any valid solution quickly**
> 2. **Find Cargo's greedy-best solution**
>
> Those are different objectives.
>
> The "graceful degradation" idea is great for goal 1.
> It is not compatible with goal 2.
>
> If Vixen says:
> - "I found a working graph, but couldn't upgrade X further, so I stopped there"
>
> that may be a good product decision, but it is **not Cargo compatibility**.
>
> So the design needs to choose:
>
> - are you building a **best-effort resolver with explanations**, or
> - an **exact optimizer matching Cargo semantics**?
>
> Right now it is trying to be both.

**Assistant — Msg 10, lines 1074–1088:**
> ## Why the "squeeze" framing now works
>
> Once you say the floor is just a known-valid lower bound, the search space becomes:
>
> - below floor: irrelevant
> - above ceiling: impossible by manifest bounds
> - between floor and ceiling: the only remaining region
>
> So the solver can legitimately operate as a bounded optimizer.
>
> That gives you a much cleaner invariant:
>
> > at all times, Vixen maintains one known-valid solution and a frontier of not-yet-disproven better solutions.
>
> That's a very good invariant. It matches your goal much better than the earlier "all nodes ripple downward until convergence" framing.

---

### Semantic volumes / orthotopes

**Assistant — Msg 12, lines 1241–1259:**
> ## The core model
>
> For packages `A, B, C, D`, each with a finite ordered version domain, a search region is something like:
>
> - `A ∈ [a_lo .. a_hi]`
> - `B ∈ [b_lo .. b_hi]`
> - `C ∈ [c_lo .. c_hi]`
> - `D ∈ [d_lo .. d_hi]`
>
> plus:
> - manifest constraints,
> - already-learned exclusions,
> - maybe some fixed decisions.
>
> So the frontier is not "all points." It is a set of **boxes with holes**.
>
> That is the first important shift.

**Amos — Msg 13, lines 1520–1521:**
> 2. The Search Model: The "Squeeze"
> Vixen does not "step" through versions. It manages a Frontier of Version Volumes. A Volume is a hyper-rectangle (orthotope) representing the Cartesian product of all possible version ranges for a conflict cluster of packages.

**Assistant — Msg 14, lines 1675–1700:**
> ## 2. A "volume" needs a stricter definition
> Right now a volume is described as a hyper-rectangle over package version ranges.
>
> But in practice that is not enough.
>
> A region likely also needs:
> - package identity partition / cluster definition
> - fixed decisions already made
> - active contextual assumptions
> - applicable feature state
> - relevant exclusions index
> - maybe target/source partition
>
> Otherwise two boxes that look geometrically identical may not mean the same thing semantically.
>
> So I'd define something more like:
>
> ```text
> Volume = {
>   cluster: ConflictClusterId,
>   bounds: Map<PackageKey, VersionIntervalSet>,
>   fixed: PartialAssignment,
>   context: ResolutionContext,
>   local_exclusions: Set<NoGoodId>
> }
> ```
>
> Even if the implementation compresses this, the semantics need it.

**Assistant — Msg 16, lines 2033–2046:**
> ### 2. "Orthotope" is probably too narrow as the semantic core
> It is a good canonical representation, but probably not expressive enough as the only semantic object.
>
> Some forbidden regions will naturally be:
> - unions of intervals,
> - feature-conditional,
> - context-conditional,
> - or only representable after projecting away hidden dimensions.
>
> So I'd phrase it as:
>
> > A Volume is canonically represented as an orthotope over version intervals, possibly refined by fixed assignments and applicable exclusions.
>
> That gives you room to breathe.

---

### No-goods and Tier-1 exclusions

**Amos — Msg 3, lines 180–184:**
> The Shift: From "Blame" to "Constraint"
> Instead of the runtime "choosing" a package to change, it identifies the Minimal Conflict Set (the smallest set of version choices that together make the graph impossible).
> It then publishes a No-Good Fact to the Apex-scoped context. This fact is a new lattice: the Exclusion Lattice.
> The Exclusion Lattice
> A No-Good isn't just "Don't use A@1.0.10." It is a conditional:
> > "The set {A@1.0.10, B@2.0.5, C@1.1.0} is Forbidden."

**Assistant — Msg 4, line 252:**
> I would also avoid encoding no-goods as exact version tuples like `{A@1.0.10, B@2.0.5, C@1.1.0}` unless that is only a user-facing rendering. Solver-internally, you want incompatibilities over **terms/ranges**, because that generalizes much better. PubGrub's basic objects are terms such as `foo ^1.0.0` and their negations, which denote sets of allowed versions rather than singletons. If Vixen learns only singleton tuples, it will learn overly specific facts and thrash.

**Assistant — Msg 14, lines 1704–1730:**
> ### 3. "Coarse exclusion range" in Tier 1 is still dangerous
> This is still the biggest soundness risk.
>
> You say Tier 1 can publish a coarse exclusion range when a conflict occurs. If that exclusion is used by Tier 2 as if it were proven, you can kill valid regions.
>
> So the spec needs an explicit distinction between:
>
> - **proven exclusions**  
>   sound, reusable, globally valid under stated assumptions
>
> - **heuristic exclusions**  
>   search accelerators, not correctness-critical
>
> Without that distinction, the tier story is unstable.
>
> I would strongly recommend two classes:
>
> ```text
> ExclusionKind =
>   | Proven
>   | Heuristic(branch-local or epoch-local)
> ```
>
> Only proven exclusions should:
> - persist,
> - appear in strong explanations,
> - participate in exact pruning proofs.

**Assistant — Msg 16, lines 2014–2029:**
> ### 1. "Proven exclusion" is still hiding two different proof modes
> You currently say:
>
> > justified by metadata or compiler failure
>
> Those are not the same kind of proof.
>
> - **Metadata-based proof** is structural and usually reusable under clearly defined assumptions.
> - **Compiler-failure proof** is much trickier. It may depend on toolchain version, target, feature set, host state, source patching, and even nondeterministic build conditions if you are not careful.
>
> So I would split proven exclusions into at least:
>
> - **Manifest-Proven Exclusion**
> - **Build-Proven Exclusion**
>
> And I would be much more conservative about caching the second kind.
>
> Otherwise "proven" risks sounding stronger than it really is.

---

### Assumption/fact split

**Assistant — Msg 16, lines 1979–1987:**
> ## What is now very good
>
> ### 1. The assumption/fact split is finally correct
> This is the biggest improvement.
>
> You now clearly distinguish:
> - heuristic neighborhood no-goods in Tier 1,
> - proven exclusions in Tier 2,
> - committed results vs tentative exploration.
>
> That fixes one of the biggest soundness issues from the earlier versions.

**Assistant — Msg 22, lines 2618–2650:**
> ## The key shift
>
> Originally the model was:
>
> - node identity via `#[key]`
> - monotone shared state via `#[join]`
> - runtime repeatedly re-evaluates until a fixed point
>
> Now the model needs at least **three semantic classes**, not two:
>
> ### 1. Convergent inputs
> These are still the old lattice things:
> - features
> - semver requirements
> - proven exclusions
> - maybe environment/context
>
> These are monotone accumulated facts.
>
> ### 2. Search assumptions
> These are not facts. They are temporary branch-local restrictions:
> - "in this region, `tokio ∈ [1.26..1.29]`"
> - "in this slice, `hyper = 0.14.x`"
> - "assume feature `rt-multi-thread` off"
>
> These belong to a **volume / branch context**, not a global join.
>
> ### 3. Optimization metadata
> These define how search should proceed:
> - ordering / objective
> - bound function
> - split function
> - widening function
> - proof construction
>
> Those are neither inputs nor outputs in the usual function sense. They are more like **solver hooks**.

---

### Solver-as-optimizer

**Assistant — Msg 6, lines 500–524:**
> ### 3. Tier 1 is not really "convergent"
> This is the biggest conceptual issue.
>
> Tier 1 includes:
> - speculative branches,
> - branch killing,
> - coarse no-goods,
> - simultaneous shifting,
> - symmetry breaking.
>
> That is not a convergent lattice process in the strict sense. That is **heuristic search with learned exclusions**.
>
> That is fine. It may even be great. But calling the whole thing "convergent" risks mixing:
> - monotone accumulation of knowledge,
> with
> - non-monotone movement of current guesses.
>
> A selected version can go:
> - 1.2.0
> - then 1.1.9
> - then later back to 1.2.0 once another branch changes.
>
> So the solver's **knowledge** may converge monotonically, while the **current witness** does not.

**Assistant — Msg 8, lines 926–947:**
> ### 4. The upgrade phase is still search
> This is the conceptual mistake that keeps coming back in different forms.
>
> Even with:
> - a valid floor,
> - widened no-goods,
> - parallel exploration,
>
> the moment you ask for the **maximal valid graph** under an ordering, you are still doing search.
>
> A current witness can go:
> - upgrade A
> - that forces downgrade B
> - that re-enables upgrade C
> - that later allows A again
>
> So the knowledge can grow monotonically, but the chosen assignment does not.
>
> That means this is not "lattice convergence all the way down."  
> It is **search accelerated by monotone learned facts**.
>
> That is still strong. It is just a narrower and more honest claim.

**Amos — Msg 19, line 2389:**
> Human here: how come all reviewers are pleased with this solution, I thought our problem was NP hard?

**Assistant — Msg 20, lines 2411–2506** *(quoted in full under the NP-hardness thread below)*

---

### Metadata-stable blocks

**Assistant — Msg 12, lines 1319–1341:**
> ## Better than binary: split by metadata-stable blocks
>
> Because you have range widening, you can do better than one-version splits.
>
> Suppose versions `1.5..1.8` of `A` all have identical relevant manifest behavior.  
> Then split by **behavior blocks**, not raw versions:
>
> - `R1: A ∈ [1.5 .. 1.8]`
> - `R2: A ∈ [1.0 .. 1.4]`
>
> That is much better.
>
> This is probably the most important research connection in your design:
>
> **the same metadata scanner that widens no-goods should also define split boundaries.**
>
> So the frontier splitter should prefer cutting along:
> - dependency-manifest change points,
> - feature-structure change points,
> - target/cfg change points,
> - source/revision boundaries.
>
> Not arbitrary semver ticks.

**Assistant — Msg 18, lines 2320–2363:**
> ## What I think the MetadataMap should capture
>
> Since you're heading into the splitter, the internal representation should probably not just be "version → manifest hash."
>
> The useful representation is closer to:
>
> ```text id="uwuih2"
> MetadataSlice {
>   package: PackageId,
>   version_interval: [v_lo, v_hi],
>   dep_fingerprint: Hash,
>   feature_fingerprint: Hash,
>   cfg_fingerprint: Hash,
>   source_fingerprint: Hash,
>   change_points: [v1, v2, ...],
> }
> ```
>
> Or conceptually:
>
> - for each package,
> - partition all versions into maximal contiguous intervals
> - where the **relevant resolution behavior** is identical.
>
> That "relevant resolution behavior" should probably fingerprint:
> - dependency names
> - version constraints
> - optional/feature-gated edges
> - target-specific edges
> - source/replacement info
>
> Then the splitter works on slices, not raw versions.
>
> That gives you the "200 versions become 5 search slices" effect you want.
>
> ## The subtle design win
>
> The nice thing is that the same metadata partition can serve **three** roles:
>
> - **splitting boundaries**
> - **range-widening boundaries**
> - **explanation boundaries**
>
> That reuse is elegant and likely very valuable.

---

### Distributed fact caching + gossip bus

**Amos — Msg 5, lines 363–367:**
> 4. Persistent & Distributed Fact Caching
> The most powerful aspect of this architecture is the Persistence of Learned Facts.
>  * Learned Incompatibilities: A No-Good like "v1.0 of A and v2.0 of B do not compile together" is a universal truth about the registry.
>  * Global Fact Store: These No-Goods are hashed and stored in the Remote/Shared Vixen Cache.
>  * Warm Starts: When a new developer clones the repo, their solver starts with a "Knowledge Base" of thousands of previously discovered conflicts. Their resolver doesn't "search"—it simply converges using the pre-cached facts.

**Assistant — Msg 6, lines 574–617:**
> ## The biggest overstatement left
>
> ## "Registry-universal"
> This is still too strong.
>
> A cached fact is only universal relative to some assumption frame, for example:
> - registry snapshot
> - target triple
> - resolver policy
> - feature context
> - source overrides / patches
> - maybe toolchain or host constraints if buildability is involved
>
> So I would replace:
>
> > learned facts are Registry-Universal
>
> with something like:
>
> > learned facts are Snapshot-Scoped and Assumption-Scoped.
>
> That still supports warm starts, but it is correct.

**Amos — Msg 13, lines 1537–1552:**
> 4. Distributed Fact-Learning
> The solver's performance scales with your cores because workers communicate via a Lock-Free Gossip Bus:
>  * New Incumbent: "I found a better graph!" → All other workers immediately prune any region that cannot beat it.
>  * Widened No-Good: "This range of A conflicts with this range of B!" → All workers lazily carve this hole out of their assigned volumes.
> 5. Universal Provenance: The explain Engine
> […]
> 6. Incremental & Distributed Persistence
> Vixen's learned facts are Registry-Universal.
>  * Fact Caching: No-Good ranges are stored in a local/remote cache, keyed by the registry snapshot.
>  * Warm Starts: A new developer clones the project and inherits the "Exclusion Lattice" from their team. Their resolver doesn't search for known conflicts; it simply converges using the pre-cached facts.

**Assistant — Msg 14, lines 1770–1783:**
> ## 6. The gossip bus section is good architecture language, but not yet semantic language
> "Lock-free gossip bus" is an implementation choice, not really part of the formal spec.
>
> The formal part is:
>
> - incumbents are globally monotone improving,
> - proven exclusions are globally monotone accumulating,
> - workers observe these updates eventually,
> - stale work is permitted but must be discardable.
>
> That's the semantic contract.  
> The transport can be lock-free gossip, work-stealing, shared memory, whatever.

**Amos — Msg 15, lines 1937–1940:**
> 6. Incremental & Assumption-Scoped Caching
> Learned facts are Snapshot-Scoped and Assumption-Scoped.
>  * Fact Caching: Proven No-Goods are stored in a remote cache, keyed by the registry hash and environmental witness (e.g., "Incompatible on win32").

---

### Proposed solver-language constructs

*[quoted in full under "Tension (i)" below — that is where the full construct
list, the rejection, and the walk-back live]*

---

### Cache trust model

**Amos — Msg 17, line 2185:**
> Trust Model: Cached facts are Replayable. A consumer can verify an exclusion by re-running the trace before accepting it into the local search.

**Assistant — Msg 18, lines 2251–2256:**
> ### 3. The cache trust model is excellent
> "Replayable" is the right word.
>
> That fixes the biggest social/engineering risk in distributed learned facts:
> you do not need blind trust in somebody else's cache artifact.

---

### Provenance

**Amos — Msg 5, lines 355–361:**
> 3. Universal Provenance: The vixen explain Output
> Because every #[join] and No-Good is recorded, the "Why" of a build is a first-class value.
> > vixen explain openssl
> > 1. Environmental Fact: Target is aarch64-apple-darwin (discovered via cc -v).
> > 2. Logical Constraint: openssl requires feature vendored (demanded by postgres-native).
> > 3. Learned Exclusion: No-Good #402 proves v0.10.50 is incompatible with vendored on darwin.
> > 4. Search Limit: No other versions in ^0.10 satisfy the intersection of ^0.10.45 (crate-A) and >=0.10.48 (crate-B).

**Assistant — Msg 6, lines 554–572:**
> ### 5. "Unique Implication Point" is the wrong public center of gravity
> UIP is a solver-internal analysis concept. It is useful, but I would not make it the headline of user explanation.
>
> Users usually care about:
> - which packages demanded what,
> - which constraints became mutually incompatible,
> - why no remaining versions satisfy all demands.
>
> That is usually a minimal explanation graph or incompatibility chain, not "the exact choice that made the build impossible."
>
> Sometimes there is no human-meaningful single dramatic choice; the failure is distributed.
>
> So I would keep UIP as an implementation tool for clause learning, but present explanations as:
> - demand chain,
> - accumulated requirements,
> - learned incompatibility,
> - exhausted candidate set.
>
> That is more robust and more honest.

**Amos — Msg 15, lines 1929–1935:**
> 5. Universal Provenance: The explain Narrative
> Because every pruning step is recorded, the vixen explain command synthesizes a 4-layer narrative that distinguishes between assumptions and proven facts:
> > vixen explain tokio
> >  * Witness: Target is linux-gnu (Witnessed via #host.inspect).
> >  * Join: Feature net was unioned (Demanded by axum).
> >  * Pruning: Versions [1.0.0..1.25.0] were Pruned; they are Dominated by the current Incumbent (1.30.0).
> >  * Exclusion: Volume tokio@[1.26..1.29] x hyper@[0.14..] is Forbidden by No-Good #102 (Proven Incompatibility).

---

### NP-hardness discussion

**Amos — Msg 19, line 2389:**
> Human here: how come all reviewers are pleased with this solution, I thought our problem was NP hard?

**Assistant — Msg 20, lines 2411–2506:**
> Because "good design" and "not NP-hard anymore" are completely different claims.
>
> Your problem can stay NP-hard and still have reviewers say:
>
> - the formulation is coherent,
> - the invariants are sound,
> - the pruning strategy is clever,
> - the parallelization story is plausible,
> - the explanation model is strong.
>
> What they're approving is mostly the **solver architecture**, not a proof that complexity disappeared.
>
> The clean way to think about it:
>
> ## NP-hard means worst-case hardness
> It means that in the worst case, exact dependency resolution can encode very hard combinatorial search.
>
> So no amount of cleverness gives you:
> - guaranteed polynomial time on all inputs,
> - guaranteed small frontier,
> - guaranteed easy splitting.
>
> Your solver does **not** escape that.
>
> ## What your design does improve
> It can still be a big advance if it improves one or more of these:
>
> - **constant factors**
> - **average-case behavior**
> - **parallel scalability**
> - **pruning power**
> - **incrementality**
> - **explainability**
> - **cache reuse across runs**
>
> That's already a lot.
>
> A SAT solver is still solving NP-hard problems, but modern SAT solvers are amazing because:
> - clause learning,
> - propagation,
> - heuristics,
> - restarts,
> - good data structures.
>
> They didn't make SAT easy in theory. They made huge classes of practical instances tractable.
>
> That's the category your proposal is drifting into.
>
> ## Why the reviewers sound pleased
> Because the later versions stopped making the suspicious claim:
>
> > "we replaced search with convergence"
>
> and instead moved toward:
>
> > "we are still doing exact search, but with stronger invariants, better pruning objects, metadata-aware partitioning, and a clean correctness story."
>
> That is much more believable.
>
> So the reaction is basically:
> - "yes, this could be a serious exact solver design"
> not
> - "wow, P = NP"
>
> ## Where the hardness still lives
> It's still in:
> - choosing which regions to split,
> - proving regions dead,
> - dealing with interacting constraints across packages,
> - handling pathological graphs with lots of near-solutions,
> - avoiding region explosion.
>
> Your widened no-goods and metadata slices may crush many practical cases, but there will still be nasty registries/manifests where the frontier blows up.
>
> ## The real research value
> The value is not "solving an NP-hard problem efficiently in all cases."
>
> It is more like:
>
> - a better **representation** of the search frontier,
> - better **learned facts** than point exclusions,
> - better **parallel work decomposition**,
> - better **proof/explain artifacts**,
> - better **warm-start behavior**.
>
> That is enough to make people excited.
>
> ## The blunt answer
> They're pleased because the proposal has become:
>
> **an exact NP-hard solver with a plausible plan for making many real instances much faster and much more understandable.**
>
> That is a very respectable outcome.
>
> NP-hardness is not an embarrassment here.  
> It just means you should be suspicious of any claim that the hard part disappeared.

---

### Use cases beyond package managers

**Amos — Msg 26, line 3521:**
> We've been thinking about Cargo this whole time. Let's think about npm. Or other package managers. Let's think about replacing nix! Are all those implementable in Vixen code given these primitives? Are there other use cases out there that I'm not seeing, that Vixen would make faster and/or more debuggable?

**Assistant — Msg 27, lines 3561–3582:**
> ### npm / pnpm / yarn
> Also a good fit, but different.
>
> npm is not just "solve one global version per package." It permits **multiple versions installed at different points in the tree**, hoisting, peer dependency constraints, optional deps, platform conditions, lockfile effects, etc.
>
> That means the identity is not just:
> - package name
>
> but more like:
> - package instance in a path / placement context
>
> Still expressible, but the shape changes:
> - joins are weaker in some places
> - search is more tree-placement oriented
> - "best" is not just highest version; it may involve hoisting and deduplication preferences
>
> So yes, implementable, but it would stress:
> - context-sensitive identity
> - placement constraints
> - explanation of why one subtree got a duplicate copy
>
> That actually sounds like a **great Vixen showcase**, because npm debugging is awful.

**Assistant — Msg 27, lines 3612–3637:**
> ### Nix
> This is where it gets subtle.
>
> If by "replace nix" you mean:
> - pure store path derivations
> - evaluation of package expressions
> - build graph explanation
> - cache/reproducibility metadata
>
> then **parts** of Nix are a strong fit.
>
> If you mean:
> - fully replace the Nix language / module system / lazy evaluation / derivation semantics wholesale
>
> that is a much bigger claim.
>
> Vixen seems especially promising for the parts of Nix that are painful today:
> - why did this derivation rebuild?
> - why was this input chosen?
> - why do these overlays conflict?
> - how can we incrementally recompute only the affected slice?
> - how do we distribute evaluation with provenance?
>
> So I would say:
> - **replace some Nix solver/evaluator roles:** plausible
> - **replace all of Nix as-is:** much less obvious

**Assistant — Msg 27, lines 3645–3654:**
> Here's the more important point:
>
> The package-manager framing may actually be too narrow.
>
> Vixen looks useful for any domain with these properties:
>
> - many derived nodes
> - partial demand-driven evaluation
> - monotone accumulation of facts
> - occasional exact search
> - need for replayable explanations
> - reuse of learned exclusions across runs
>
> That opens a lot.

**Assistant — Msg 27, lines 3790–3798:**
> ## The real generalization
>
> I think the right abstract pitch is not:
>
> > Vixen is for package managers
>
> It is:
>
> > Vixen is for **justified demand-driven planning problems**.

---

## 2. Four targeted sections — verbatim cuts

### 2a. FEATURES and feature unification

**Amos — Msg 1, lines 73–76:**
> The Problem: Feature Unification
> In Cargo-style resolution, if two paths demand the same package with different features, those features must be unified. A standard memoized function resolve(pkg, features) would treat resolve(foo, {A}) and resolve(foo, {B}) as two different values, leading to graph duplication.
> The Solution: The cumulative Call-Site
> We introduce the cumulative keyword at the call site to signal that an argument is a Lattice Point.

**Assistant — Msg 2, line 127:**
> The motivating example is also well chosen. Cargo really does treat features additively: when the same dependency is used in multiple places, Cargo uses the **union** of the enabled features for that dependency, and Cargo's newer resolver exists partly because naive unification causes bad results in practice. So "feature sets as a lattice" is not hand-wavy; it matches an existing pain point.

**Assistant — Msg 2, line 137:**
> Where I would push back is the decision to put `cumulative` **only at the call site**. Whether joining `features` is legal is not really a property of one call; it is a property of the **callee's semantics**. The fixed-point literature is very clear that monotonicity is the central contract here: Datafun tracks monotonicity with types, proves least fixed points for monotone functions on finite-height structures, and CALM makes the related systems point that monotone problems admit coordination-free accumulation while non-monotone ones do not. So I would make the function declare its key fields and join fields, and let the call site merely participate in that declared convergence regime.

**Assistant — Msg 4, line 269:**
> There is also a Cargo-specific modeling problem: `#[key] id: String` is too coarse. Cargo can activate multiple semver-incompatible versions of the same crate in one graph, and resolver v2 deliberately does **not** unify features across some contexts such as target-specific deps, build-deps/proc-macros, and some dev-dep situations. So the merge key has to include more than package name: at least source + compatibility/context partition, and probably target/build-kind context too.

**Assistant — Msg 4, lines 271–288:**
> Likewise, I would not force everything through one generic `#[join]`. For Cargo-like solving you really have a product of accumulators:
>
> - features accumulate by **union**,
> - version requirements accumulate by **intersection**,
> - learned incompatibilities accumulate by **union**.
>
> You can still present that as one lattice product, but the partial order has to be defined carefully.
>
> The design I would endorse is this:
>
> ```vx
> state Resolve(key: PackageInstance) {
>   demand_features : Set[Feature]        // union
>   demand_req      : VersionSet          // intersection
>   learned         : Set[Incompat]       // union
>   candidates      : VersionSet          // monotonically shrinking
> }
> ```

**Assistant — Msg 6, lines 450–479:**
> ### 1. `#[join] SemverRange` is underspecified
> You say version ranges are "intersected" and the node re-evaluates whenever a `#[join]` argument widens.
>
> That needs more care, because semver constraints do not behave like feature sets.
>
> For features:
> - more demands means larger set,
> - join is union,
> - that is a normal upward-growing lattice.
>
> For version requirements:
> - more demands means fewer allowed versions,
> - operationally the candidate set shrinks,
> - but the "constraint object" itself is not obviously widening in the same direction as features.
>
> So the runtime model needs one of these:
>
> - either `#[join]` does not mean "grow upward" in one universal sense, but "merge according to a declared partial order,"
> - or you represent version requirements as a domain where "more precise / more restrictive" is the upward direction.
>
> Without that, the proposal sounds elegant but the order theory is muddy.
>
> The clean version is:
>
> - `features`: join = union
> - `requirements`: join = intersection
> - `learned incompatibilities`: join = union
> - `candidate versions`: derived monotone narrowing state, not an input join
>
> That is a product domain, not one generic "join means widening" story.

**Assistant — Msg 29, lines 3901–3932 (Milestone 1):**
> ## Milestone 1 — No search, only convergent demand
>
> Implement only what your original design needed:
>
> ```vixen
> #[convergent]
> fn resolve_pkg(
>   #[key] id: PackageId,
>   #[join] features: Set[Feature],
>   #[join] req: VersionSet,
> ) -> Demand
> ```
>
> No version choice yet.
>
> The function should only:
> - accumulate demands
> - load metadata
> - emit more demands
> - fail if local joins become impossible
>
> This gives you:
> - the keyed demand graph
> - invalidation
> - joins
> - provenance for joins
>
> Success criterion:
> - you can express Cargo-style feature union and version-range intersection in Vixen
> - the runtime does nothing Cargo-specific
>
> This is the "language core works" checkpoint.

---

### 2b. Recorded FAILURE MODES and dragons

**Assistant — Msg 6, lines 535–552:**
> ### 4. Tier 1 coarse no-goods are dangerous
> You already noticed the problem by inventing Tier 2.
>
> That means the architecture admits Tier 1 can over-prune.
>
> Once that is true, I would be careful about presenting Tier 1 no-goods as "facts." They are not all facts in the same sense.
>
> You really have at least two classes:
>
> - **Proven incompatibilities**  
>   derived by sound analysis, safe to keep
>
> - **Heuristic exclusions**  
>   branch-local or search-local pruning hints, not globally valid
>
> That distinction matters a lot for persistence and explanation.
>
> If you do not separate them, your shared cache becomes polluted by search artifacts.

**Assistant — Msg 4, lines 254–259:**
> The deeper issue is your monotonicity claim. The thing that is monotone is the **knowledge state**: learned incompatibilities grow, and the **candidate domain** for a package shrinks. But `best_v` itself is not monotone in general. A tiny counterexample:
>
> - `A ∈ {1,2}`, `B ∈ {1,2}`
> - learned no-good: `{A=2, B=2}`
>
> If the current state picks `A=2, B=2`, one node may drop to `A=1`. But once `B` later drops to `1`, `A=2` becomes valid again. So a current "best version" can go down and then back up. What shrinks monotonically is the **set of viable assignments**, not the currently displayed witness.

**Assistant — Msg 8, lines 880–882:**
> Finding **any valid floor** is still solving.
>
> You may have good heuristics for descending to one, but that is not the same as "pure lattice convergence."

**Assistant — Msg 8, lines 909–924:**
> ### 3. Cluster independence is much harder than the text suggests
> This part is too hand-wavy:
>
> > most packages are independent  
> > so you can partition the graph into clusters of interdependent packages
>
> Sometimes yes, but identifying the right clusters is basically part of the solving problem.
>
> Because dependencies interact through:
> - shared transitive packages,
> - feature unification,
> - target-specific edges,
> - build/proc-macro contexts,
> - source/version compatibility partitions.
>
> So "just sweep each cluster in parallel" is nice intuition, but the hard part is discovering the true cluster boundaries without already doing a lot of solver work.

**Assistant — Msg 14, lines 1732–1749:**
> ### 4. "Conflict cluster" is currently a magic object
> The whole volume approach depends on solving inside conflict clusters rather than the full graph.
>
> But the spec does not define:
> - how a cluster is discovered,
> - when it grows,
> - when two clusters merge,
> - whether a learned no-good can force reclustering.
>
> That matters a lot. In practice, clusters are not static; they can fuse when a transitive dependency turns out to be shared.
>
> So I would not treat the conflict cluster as just an intuitive grouping. It needs a dynamic semantic rule.
>
> Something like:
>
> > a cluster is the connected component of the current incompatibility/dependency interaction graph projected onto version-carrying package nodes.
>
> Even that is only a start, but at least it is formalizable.

**Amos (quoting Claude) — Msg 7, line 794:**
> The range-widening assumes the reason for incompatibility is stable across adjacent versions — that the dependency declaration on C doesn't fluctuate. In practice this is almost always true (dependency requirements change at feature boundaries, not every patch), but you should verify during the scan rather than assuming. If A@1.5 depends on C ^3.0 but A@1.5.1 switches to C ^2.0, the range has a hole. The scan needs to be precise, not interpolated.

**Assistant — Msg 8, lines 951–967:**
> ### 1. It's sound only when the reason is metadata-stable
> You already noticed this, and it's the key condition.
>
> Range widening is safe when the incompatibility is caused by something like:
> - the same dependency constraint,
> - the same feature-conditional dependency structure,
> - the same target-conditional rule,
>
> across the widened interval.
>
> It is not safe to generalize from:
> - build failures,
> - toolchain quirks,
> - cfg-dependent behavior you did not check,
> - feature combinations you did not model fully.
>
> So I would treat widened no-goods as valid only when backed by a **verified common cause**, not just adjacency.

**Assistant — Msg 8, lines 969–975:**
> ### 2. Version ranges may have holes
> Exactly as Claude notes.
>
> You cannot assume contiguity just because nearby versions look similar.
> The solver should learn something more like a **union of compatible intervals / regions**, not necessarily one solid range.
>
> That is still fine, just more annoying.

**Assistant — Msg 2, line 141:**
> You also need a more explicit story for **node identity**. Cargo's resolver had to carve out distinct behavior for target-specific dependencies, dev-dependencies, and build/proc-macro dependencies, which is evidence that "same package id" is often too coarse as a merge key. In a real resolver, identity tends to include at least some combination of package, platform, dependency kind, source, and build context. Over-merging here will produce elegant semantics on paper and spooky action at a distance in practice.

**Assistant — Msg 6, lines 481–498:**
> ### 2. "Single node for serde" is sometimes too coarse
> You say that when multiple parts of the graph demand `serde`, the runtime unifies them into a single node.
>
> That is only true under a specific compatibility partition.
>
> Real resolution often needs to distinguish at least:
> - package source,
> - semver-incompatible major line,
> - target/platform context,
> - dependency kind / build context.
>
> Otherwise you over-unify.
>
> So the real key is not "package name" and maybe not even "package ID" in the casual sense. It is closer to:
>
> `PackageInstanceKey = (name, source, compatibility class, target context, dependency role, apex partition...)`
>
> You do not need to expose all of that in the language surface, but the runtime model needs it.

**Assistant — Msg 6, lines 594–611:**
> ### Over-claim
> This sentence is too strong:
>
> > A No-Good like "v1.0 of A and v2.0 of B do not compile together" is a universal truth about the registry.
>
> Usually that is **not** a universal truth.
>
> It may depend on:
> - target triple,
> - compiler version,
> - feature set,
> - build flags,
> - optional transitive deps,
> - system libraries,
> - resolver policy,
> - source revision,
> - patch overlays.
>
> So cached facts need a scope. More like:
>
> > Under assumptions X, Y, Z, this incompatibility holds.

**Assistant — Msg 6, lines 554–562:**
> ### 5. "Unique Implication Point" is the wrong public center of gravity
> UIP is a solver-internal analysis concept. It is useful, but I would not make it the headline of user explanation.
>
> Users usually care about:
> - which packages demanded what,
> - which constraints became mutually incompatible,
> - why no remaining versions satisfy all demands.
>
> That is usually a minimal explanation graph or incompatibility chain, not "the exact choice that made the build impossible."
>
> Sometimes there is no human-meaningful single dramatic choice; the failure is distributed.

**Assistant — Msg 14, lines 1810–1822:**
> ### Replace "every value must be a justified fact"
> That sounds nice, but it blurs:
> - tentative search decisions,
> - proven exclusions,
> - derived joins.
>
> A tentative branch choice is not a fact. It is an assumption.
>
> So I'd say:
>
> > every committed output and every globally shared pruning step must be justified by recorded provenance.
>
> That is more accurate.

**Assistant — Msg 16, lines 2019–2023:**
> - **Metadata-based proof** is structural and usually reusable under clearly defined assumptions.
> - **Compiler-failure proof** is much trickier. It may depend on toolchain version, target, feature set, host state, source patching, and even nondeterministic build conditions if you are not careful.

**Assistant — Msg 18, lines 2299–2309:**
> ### 3. Build-proven exclusions need stricter replay keys than just environmental witness
> You mention compiler/linker failures scoped by witness, which is good.
>
> I'd still expect the cache key to include at least hints of:
> - registry snapshot
> - package source/revision
> - relevant feature set
> - target/toolchain
> - maybe patch/override set
>
> Not because the idea is wrong, just because "environmental witness" alone sounds too narrow for build-derived facts.

**Assistant — Msg 18, lines 2269–2286:**
> ### 1. "Cargo-compatible" still depends on the package ordering being canonical enough
> `Minimum Topological Depth from the floor solution` is clever, but it makes me nervous.
>
> Why:
> - the floor is heuristic and non-deterministic,
> - cluster fusion may change apparent structure,
> - two different floors could induce different ordering.
>
> If Cargo-compatibility is a hard goal, the ordering should come from something **independent of the discovered floor**.
>
> A safer source would be something like:
> - canonical root-reachable package instance order derived from the dependency specification graph,
> - or some deterministic resolver order based only on manifest structure and package identity.
>
> Using the floor as the source of order is elegant, but it risks leaking heuristic nondeterminism into the supposedly exact phase.
>
> That is the single biggest thing I would still change.

**Assistant — Msg 18, lines 2311–2318:**
> ## My strongest recommendation
>
> If you only tighten one thing before prototyping, make it this:
>
> **remove floor-derived package priority from the formal ordering.**
>
> Everything else can evolve during implementation.  
> That one can quietly poison determinism if it stays.

**Amos — Msg 23, line 3079:**
> Sure but it doesn't fit Vixen's dataflow language with demand driven evaluation. You also just invented 20 keywords and made a system so flexible it's impossible to implement generically. Read the current spec to get a better idea of where things fit.

---

### 2c. DETERMINISM and Cargo-compatible ORDERING

**Amos — Msg 7, lines 761–764:**
> 4. Deterministic Convergence (The Cargo Invariant)
> To ensure we get the exact same result as Cargo:
>  * Prioritized Ascent: During the "Exhaustive Ascent" phase, we prioritize packages by their lexicographical name.
>  * Greedy Preference: We always prefer the solution that has the highest version of the highest-priority package.

**Assistant — Msg 10, lines 1109–1121:**
> ## What still needs tightening
>
> ### 1. You need Cargo's ordering stated precisely
> "Same solution Cargo would" is only meaningful if the optimization order is defined exactly.
>
> Not just:
> - highest version first,
> - lexicographic package tie-break,
>
> but the actual comparison rule over whole resolved graphs.
>
> Because the squeeze needs a proof obligation like:
>
> > no unexplored assignment can dominate the current incumbent under Cargo order
>
> Without that, "same as Cargo" is still aspirational.

**Amos — Msg 13, lines 1533–1534:**
> Dominance Ordering: We define a stable, deterministic "Best" (\succ):
>    * G_1 \succ G_2 if, at the first package P (ordered by name) where they differ, Version(P, G_1) > Version(P, G_2).

**Amos — Msg 17, line 2165:**
> The Priority: Packages are ordered by Minimum Topological Depth (distance from root in the floor solution), then lexicographical name.

**Assistant — Msg 18, lines 2269–2277:**
> ### 1. "Cargo-compatible" still depends on the package ordering being canonical enough
> `Minimum Topological Depth from the floor solution` is clever, but it makes me nervous.
>
> Why:
> - the floor is heuristic and non-deterministic,
> - cluster fusion may change apparent structure,
> - two different floors could induce different ordering.
>
> If Cargo-compatibility is a hard goal, the ordering should come from something **independent of the discovered floor**.

**Assistant — Msg 16, lines 2048–2057:**
> ### 3. Your dominance order may need one more sentence
> You say:
>
> > ordered by depth, then name
>
> That is much better than before, but I would still want the spec to define exactly what "depth" means when the same package appears through multiple paths or when cluster fusion changes the apparent structure.
>
> You probably want:
> - a canonical package-instance ordering derived from the resolved dependency projection,
> not an intuitive tree depth.
>
> This is one of those details that can quietly break "Cargo-compatible determinism" if left fuzzy.

---

### 2d. UNIVERSES / cross-compilation / target-specific dependencies

*No dedicated verbatim thread exists for this in the source. The only passages
touching it at all are the node-identity fragments already quoted in full above
(§2b) and in the map's "convergent nodes" and "no-goods" threads:*

**Assistant — Msg 2, line 141 (already quoted in full in §2b):**
> […] identity tends to include at least some combination of package, platform, dependency kind, source, and build context. […]

**Assistant — Msg 4, line 269 (already quoted in full in §2a):**
> […] resolver v2 deliberately does **not** unify features across some contexts such as target-specific deps, build-deps/proc-macros, and some dev-dep situations. […]

**Amos — Msg 13, line 2161 context field:**
> Context: Environmental witnesses (Target Triple, OS) and registry snapshot.

**Assistant — Msg 22, lines 2686–2688, 2973–2974 (`#[context]` parameter, quoted in full in Tension (i) below):**
> #[context] target: TargetTriple,
> #[context] registry: RegistrySnapshot,

No message in this conversation discusses cross-compilation, host/target
resolution, or `#[context] target` as a subject in its own right — every
appearance above is a parenthetical inside a different argument.

---

## 3. Tension list — verbatim support

### Tension (i): `solve` as a runtime primitive with no search constructs in vix

**Amos — Msg 21, lines 2524–2553 (recalls the original syntax, asks the question that opens this whole thread):**
> Do you remember that this whole scheme is not "hardcoding Cargo resolver logic in Rust" but that it is part of the Vixen language runtime. That one must be able to express the shape of the problem using the Vixen language (encoding the Cargo specific bits), and that the runtime must handle evaluation, just like it does demand driven evaluation etc. Do you remember what the proposed syntax looked like, back when we thought it was just a monotonic lattice problem?
>
> It looked like this:
>
> ```vixen 
> #[convergent]
> fn resolve_pkg(
>   #[key] id: String,
>   #[join] features: Set[String],
>   #[join] version_req: SemverRange,
>   #[join] no_goods: Set[NoGood], // The cumulative learned clauses
> ) -> ResolvedCrate {
>
>   // find_best_fit handles the CDCL logic:
>   // It picks the highest version that satisfies version_req 
>   // AND doesn't violate any no_goods given other active selections.
>   let best_v = registry.find_best_fit(id, version_req, no_goods);
>
>   // If we hit ⊥, the runtime's exception handler kicks in,
>   // performs the provenance walk, learns a new NoGood,
>   // and re-triggers the convergence.
>   if (best_v == ⊥) ( return ⊥ );
>
>   version.deps.map((d) => 
>     resolve_pkg(d.name, d.features, d.req, no_goods)
>   )
> }
> ```
>
> At some point we decided to annotate the call sites too. How would we express the constraints now, with which language? We need to be able to express the floor, the incumbent, range widening for volume culling etc. Without hardcoding any Cargo knowledge in Rust. What are your thoughts here?

**Assistant — Msg 22, lines 2675–2723 (the `solver` block proposal, the direct antecedent of today's `solve`):**
> # Concretely: I'd evolve the syntax from `#[convergent] fn` to something like a `solver` declaration
>
> Something along these lines:
>
> ```vixen
> solver cargo_resolve(root: Manifest, ctx: ResolveContext) -> ResolvedGraph {
>   node resolve_pkg(
>     #[key] pkg: PackageId,
>     #[join(union)] features: Set[Feature],
>     #[join(intersect)] req: VersionSet,
>     #[join(union)] exclusions: Set[NoGood],
>     #[context] target: TargetTriple,
>     #[context] registry: RegistrySnapshot,
>   ) -> PackageNode
>
>   choose version(pkg, domain: versions(pkg, req, target, registry))
>
>   deps of version(pkg) => 
>     version(pkg).deps.map((d) =>
>       resolve_pkg(
>         d.id,
>         #[join] d.features,
>         #[join] d.req,
>         #[join] exclusions,
>         target,
>         registry
>       )
>     )
>
>   conflict when domain(version(pkg)) == ∅
>
>   objective lexicographic by package_order(root, registry)
>   bound optimistic_max
>   split at metadata_change_points
>   widen using cargo_widen_no_good
>   persist proven_exclusions scoped_by (registry, target)
> }
> ```
>
> Not this exact syntax, but this **shape**.
>
> The important point is that:
> - `node` describes convergent demand semantics
> - `choose` introduces a search variable
> - `objective / bound / split / widen` are declarative hooks into the runtime optimizer

**Assistant — Msg 22, lines 2761–2818 (the individual construct proposals — `#[choice]`, `#[assume]`, `bound`, `split`, `learn`/`widen`):**
> I think the language needs a small set of solver-oriented constructs. Maybe something like:
>
> ## 1. `#[choice]` or `choose`
> A search variable over a finite domain.
>
> ```vixen
> let v = choose Version
>   from registry.versions(id)
>   where satisfies(v, req)
> ```
>
> But more likely attached declaratively to a node field:
>
> ```vixen
> #[choice(max)] version: Version
> ```
>
> where `max` refers to the objective order.
>
> ## 2. `#[assume]` / branch-local context
> For volume carving and splitting.
>
> ```vixen
> search volume {
>   assume tokio in [1.26 .. 1.29]
>   assume hyper in [0.14 .. max]
> }
> ```
>
> These are not joins. They are local refinements.
>
> ## 3. `bound`
> A declarative upper-bound function over a volume.
>
> ```vixen
> bound resolve_pkg(volume) = optimistic_graph(volume)
> ```
>
> ## 4. `split`
> A declarative splitter.
>
> ```vixen
> split resolve_pkg(volume) by metadata_change_points(volume)
> ```
>
> ## 5. `learn` / `widen`
> A way to turn a conflict proof into a persistent exclusion.
>
> ```vixen
> learn from conflict {
>   let core = analyze(conflict);
>   widen cargo_manifest_ranges(core)
> }
> ```
>
> That lets range widening stay in Vixen.

**Assistant — Msg 22, lines 2938–2958 (the full inventory of new pieces this branch wanted):**
> # The language you probably need
>
> I think the evolved Vixen model is something like:
>
> ## Existing pieces
> - `#[key]`
> - `#[join(op)]`
> - pure functions
> - demand-driven memoization
>
> ## New pieces
> - `solver`
> - `#[choice]`
> - `#[context]`
> - `volume`
> - `assume`
> - `objective`
> - `bound`
> - `split`
> - `learn/widen`
> - `persist scoped_by`
>
> That is enough to express what you want without hardcoding Cargo logic in Rust.

**Amos — Msg 23, line 3079 (the rejection):**
> Sure but it doesn't fit Vixen's dataflow language with demand driven evaluation. You also just invented 20 keywords and made a system so flexible it's impossible to implement generically. Read the current spec to get a better idea of where things fit.

**Assistant — Msg 25, lines 3122–3163 (the walk-back to a single addition):**
> Yes — fair pushback.
>
> I was drifting toward "design a generic solver DSL," while your actual constraint is much tighter:
>
> - Vixen is already a **dataflow language**
> - evaluation is **demand-driven**
> - the runtime should stay **small and generic**
> - Cargo logic should be **expressed in Vixen**
> - but not by introducing a zoo of solver-specific keywords
>
> After reading the spec, I think the right move is **not** to invent a second language for search. The right move is to keep the existing shape:
>
> - `#[key]` for node identity
> - `#[join]` for monotone accumulated inputs
> - ordinary pure values / functions
> - runtime-managed demand graph
>
> and add only the **minimum new runtime concept** needed for exact search:
>
> ## The missing thing is not "20 keywords"
> It's one extra semantic dimension:
>
> **branch-local assumptions**
>
> […]
>
> So I think the real extension is something like:
>
> - `#[key]` — persistent node identity
> - `#[join]` — monotone accumulated facts
> - `#[assume]` or equivalent — **branch-local refinement**, supplied by the runtime, not globally joined
>
> Not twenty things. Really one.

**Assistant — Msg 25, lines 3198–3244 (the revised code, showing the ONE addition in place):**
> ```vixen
> #[convergent]
> fn resolve_pkg(
>   #[key] id: PackageId,
>   #[join] features: Set[Feature],
>   #[join] version_req: VersionSet,
>   #[join] no_goods: Set[NoGood],
>   #[assume] volume: Volume,
> ) -> ResolvedPkg {
>   let domain =
>     registry.versions(id)
>       .filter((v) => version_req.contains(v))
>       .filter((v) => volume.allows(id, v))
>       .filter((v) => !violates_no_goods(id, v, no_goods, volume));
>
>   let best_v = domain.max();
>
>   if (best_v == None) ( fail Conflict { id, features, version_req, volume } );
>
>   let manifest = registry.manifest(id, best_v.unwrap());
>
>   let deps = manifest.deps_for(features).map((d) =>
>     resolve_pkg(
>       d.id,
>       d.features,
>       d.req,
>       no_goods,
>       volume,
>     )
>   );
>
>   ResolvedPkg {
>     id,
>     version: best_v.unwrap(),
>     deps,
>   }
> }
> ```
>
> This is much closer to Vixen as specified:
>
> - still just a function
> - still keyed / joined
> - still pure
> - still demand-driven
> - no search DSL embedded in the source
> - Cargo-specific logic remains in user code

**Assistant — Msg 25, lines 3326–3353 (the minimal-addition list, restated as three items):**
> # What I would add, minimally
>
> If I try to stay maximally conservative with the language surface, I'd add only something like:
>
> ### 1. `#[assume]`
> A parameter class whose value is:
> - branch-local
> - supplied by the runtime
> - not part of monotone apex joins
> - but part of memoization identity for a search branch
>
> ### 2. A standard `fail` payload convention
> You already have `fail` in the language. That is actually very useful here.
>
> A package resolver can `fail Conflict { ... }`, and the runtime can:
> - capture that structured value
> - pass it to a user-defined widening/generalization function
> - turn the result into new proven no-goods
>
> That is very Vixen-shaped.
>
> ### 3. Maybe one attribute on functions
> Something like `#[search]` or reuse `#[convergent]` with extra meaning, so the runtime knows:
> - failures are solver conflicts, not just normal poison/failure
> - branch-local assumptions are meaningful
> - it should engage the frontier engine
>
> That's still tiny surface area.

**Assistant — Msg 25, lines 3392–3399, 3406–3425 (no new syntax needed for widening or splitting):**
> ```vixen
> fn widen_conflict(conflict: Conflict, registry: Registry) -> Set[NoGood] {
>   match conflict.cause {
>     DepMismatch { a, b, dep, req_a, req_b } =>
>       scan_adjacent_versions(registry, a, (v) =>
>         registry.manifest(a.id, v).dep_req(dep) == req_a
>       ).cartesian(
>         scan_adjacent_versions(registry, b, (v) =>
>           registry.manifest(b.id, v).dep_req(dep) == req_b
>         )
>       ).map((pair) => NoGood.dep_mismatch(pair, dep, req_a, req_b))
>
>     _ => [NoGood.point(conflict)]
>   }
> }
> ```
>
> No new syntax needed there at all.
>
> That is exactly the kind of thing Vixen should be good at:
> - pure
> - data-driven
> - structured values
> - replayable
>
> […]
>
> ```vixen
> fn metadata_change_points(id: PackageId, registry: Registry) -> Array[Version] {
>   registry.versions(id)
>     .adjacent_pairs()
>     .filter_map((pair) => {
>       let a = registry.manifest(id, pair.left);
>       let b = registry.manifest(id, pair.right);
>       if (resolution_fingerprint(a) != resolution_fingerprint(b)) {
>         Some(pair.right)
>       } else {
>         None
>       }
>     })
> }
> ```
>
> The runtime uses this to split volumes.
> Again: generic runtime, Cargo knowledge in Vixen.

**Assistant — Msg 25, lines 3436–3460 (the actual `solve(...)` primitive proposal):**
> # What the top-level shape might look like
>
> Not a whole new DSL. Just ordinary bindings plus one runtime entrypoint.
>
> Maybe conceptually:
>
> ```vixen
> let cargo_solver = Solver {
>   resolve: resolve_pkg,
>   widen_conflict,
>   metadata_change_points,
>   compare_graphs: cargo_compare,
> }
> ```
>
> or even simpler:
>
> ```vixen
> let result = solve(resolve_pkg) where {
>   widen_conflict,
>   metadata_change_points,
>   compare: cargo_compare,
> }
> ```
>
> That is only one builtin-ish entrypoint, not a new language tier.
>
> And it matches the style of `exec` in the spec:
> - one runtime primitive
> - ordinary values / closures plugged into it
>
> That feels much more consistent with the rest of Vixen.

**Assistant — Msg 25, lines 3464–3494 (final reframing):**
> # So my revised opinion
>
> I think the correct path is:
>
> - keep `#[key]` and `#[join]`
> - add **at most one new semantic parameter class** for branch-local assumptions, if needed
> - use ordinary structured `fail` values for conflicts
> - use ordinary Vixen functions for widening and metadata scanning
> - expose a small runtime `solve(...)` primitive that drives frontier search over the demand graph
>
> That gives you:
> - no Cargo hardcoding in Rust
> - no huge solver DSL
> - no violation of Vixen's dataflow feel
> - still enough structure to express incumbent / widening / splitting
>
> # The main correction to my previous answer
>
> You were right: I overfit to "design a generic solver language."
>
> The better fit for Vixen is:
>
> **a small extension of the existing demand-driven model, not a second declarative subsystem.**
>
> So the question is not "what new syntax expresses floor/incumbent/split?"
>
> It is:
>
> **what is the smallest runtime interface that lets ordinary Vixen code describe those pieces?**

**Assistant — Msg 29, lines 3877–3899 (Milestone 0, the Rust/Vixen boundary rule that frames the whole tension):**
> ## Milestone 0 — Freeze the boundary
>
> Before writing code, freeze one rule:
>
> **Rust owns only generic machinery. Vixen owns domain semantics.**
>
> Rust may know:
> - keyed node evaluation
> - memoization
> - joins
> - branch contexts
> - conflict capture
> - replay
> - scheduler
>
> Rust must not know:
> - semver
> - Cargo feature rules
> - manifest meaning
> - metadata widening logic
> - package ordering policy
>
> If you violate this once, you'll keep violating it.

**Assistant — Msg 29, lines 4245–4254 (the decisive-milestone framing):**
> # The key milestone to watch
>
> The decisive moment is not parallelism.
>
> It is Milestone 5:
>
> **can the runtime stay generic once compare/split/learn are all supplied in Vixen?**
>
> If yes, you've avoided the "Cargo logic in Rust" trap.  
> If no, stop and redesign there before adding more machinery.

---

### Tension (ii): the solve RESULT pinned in the journal as authority; recompute-from-pins as audit

*No passage in this document discusses a journal, pinning, or lockfile-as-authority
in these terms. The nearest analogue — a cache of learned facts that is
scoped and must be independently re-verifiable — is quoted in full here so the
contrast with today's "pin is authority" rule is visible directly.*

**Amos — Msg 5, lines 363–367 (the original, later-retracted "universal truth" framing):**
> 4. Persistent & Distributed Fact Caching
> The most powerful aspect of this architecture is the Persistence of Learned Facts.
>  * Learned Incompatibilities: A No-Good like "v1.0 of A and v2.0 of B do not compile together" is a universal truth about the registry.
>  * Global Fact Store: These No-Goods are hashed and stored in the Remote/Shared Vixen Cache.
>  * Warm Starts: When a new developer clones the repo, their solver starts with a "Knowledge Base" of thousands of previously discovered conflicts. Their resolver doesn't "search"—it simply converges using the pre-cached facts.

**Assistant — Msg 6, lines 590–617 (the retraction):**
> ### Over-claim
> This sentence is too strong:
>
> > A No-Good like "v1.0 of A and v2.0 of B do not compile together" is a universal truth about the registry.
>
> Usually that is **not** a universal truth.
>
> […]
>
> So cached facts need a scope. More like:
>
> > Under assumptions X, Y, Z, this incompatibility holds.
>
> That can still be extremely useful. But it is not universal unless the assumptions are part of the fact identity.
>
> I would rename this whole section from "global fact store" to something like:
>
> **assumption-scoped learned incompatibility cache**
>
> Less magical, more correct.

**Amos — Msg 17, line 2185 (the "Replayable" trust model — the closest March gets to "recompute is the audit"):**
> Trust Model: Cached facts are Replayable. A consumer can verify an exclusion by re-running the trace before accepting it into the local search.

**Assistant — Msg 18, lines 2251–2256 (why replay-before-trust matters):**
> ### 3. The cache trust model is excellent
> "Replayable" is the right word.
>
> That fixes the biggest social/engineering risk in distributed learned facts:
> you do not need blind trust in somebody else's cache artifact.

**Assistant — Msg 16, lines 2007–2010 (the scoping fix that stuck):**
> ### 4. The caching story is finally scoped correctly
> "Snapshot-scoped and assumption-scoped" is much better than "registry-universal."
>
> That change alone makes the persistence story credible.

---

### Tension (iii): feature/optional-dep activation as PubGrub-propagated terms; only version choices are decision points

**Assistant — Msg 4, lines 290–299 (the closest March gets to separating "decision" from "propagation," but only for version choice, and explicitly recommending exclusions live outside the joined-argument chain):**
> Runtime protocol:
>
> 1. Propagate demands and learned incompatibilities until quiescence.
> 2. If some `candidates = ∅`, analyze the conflict and learn a new incompatibility.
> 3. If every live package has a singleton candidate set, you are done.
> 4. Otherwise, make an explicit **decision** (an assumption), continue propagation, and if it fails, keep the learned incompatibility but retract the assumption level.
>
> That is basically PubGrub/CDCL recast in Vixen terms. It preserves the nice part of your idea—the monotone growth of learned knowledge—without pretending that singleton version choice is itself monotone.
>
> One more implementation note: I would not thread `exclusions` as a joined argument through every recursive call the way your sketch shows. That will invalidate too much. Better: keep learned incompatibilities in an apex-scoped indexed store, and let nodes subscribe to incompatibilities relevant to their package terms. That matches how propagation is normally organized: you look at incompatibilities referring to the currently affected package, not the entire global bag every time.

**Assistant — Msg 4, lines 271–277 (features as a monotone join, never as a propagated/retractable term — the assumption today's PubGrub-terms framing would revise):**
> Likewise, I would not force everything through one generic `#[join]`. For Cargo-like solving you really have a product of accumulators:
>
> - features accumulate by **union**,
> - version requirements accumulate by **intersection**,
> - learned incompatibilities accumulate by **union**.
>
> You can still present that as one lattice product, but the partial order has to be defined carefully.

**Assistant — Msg 6, lines 456–459 (features stated as purely upward-monotone, the assumption a Horn-clause/propagation model would need to revisit if feature activation can be retracted on backtrack):**
> For features:
> - more demands means larger set,
> - join is union,
> - that is a normal upward-growing lattice.

**Assistant — Msg 4, line 252 (the one place March explicitly reaches for PubGrub's term vocabulary, though only for no-goods, not for feature/optional-dep activation):**
> I would also avoid encoding no-goods as exact version tuples like `{A@1.0.10, B@2.0.5, C@1.1.0}` unless that is only a user-facing rendering. Solver-internally, you want incompatibilities over **terms/ranges**, because that generalizes much better. PubGrub's basic objects are terms such as `foo ^1.0.0` and their negations, which denote sets of allowed versions rather than singletons. If Vixen learns only singleton tuples, it will learn overly specific facts and thrash.

**Assistant — Msg 22, lines 2969–2996 (the last full code sketch before the "20 keywords" rejection — features are still a plain `#[join(union)]` input, filtered inline, never modeled as a propagated Horn-clause term; version choice is the only `#[choice]`):**
> ```vixen
> #[solver]
> fn resolve_pkg(
>   #[key] id: PackageId,
>   #[join(union)] features: Set[Feature],
>   #[join(intersect)] req: VersionSet,
>   #[join(union)] exclusions: Set[NoGood],
>   #[context] registry: RegistrySnapshot,
>   #[context] target: TargetTriple,
> ) -> ResolvedPkg {
>
>   #[choice(max_by = cargo_order(id))]
>   let version = registry.versions(id)
>     .where((v) => satisfies(v, req))
>     .where((v) => not_excluded(id, v, exclusions))
>     .where((v) => active_in_target(v, target));
>
>   let deps = manifest(id, version, registry)
>     .deps_for(features, target)
>     .map((d) =>
>       resolve_pkg(
>         d.id,
>         #[join] d.features,
>         #[join] d.req,
>         #[join] exclusions,
>         registry,
>         target
>       )
>     );
>
>   { id, version, deps }
> }
> ```

**Assistant — Msg 55, lines 5051–5052 (from the ASP comparison — the one place March comes close to naming "activation as logical consequence" as a distinct modeling style, without ever applying it to Cargo features):**
> First, **problem shape**. ASP is very good when you want to state the whole search problem as a logic program and optimize over complete models. That is exactly why it works well for Spack's concretization problem. Vixen is aiming for a world where only part of the graph may be demanded right now, some facts are discovered on demand, and the same runtime handles ordinary incremental computation and exact search.

No message in this document proposes feature or optional-dependency activation
as a Horn-clause implication propagated ahead of decision-making, and no
message states that version choice is the *sole* decision point with
everything else — including feature activation — as forced consequence. That
framing is absent from March; the closest analogues are quoted above.
