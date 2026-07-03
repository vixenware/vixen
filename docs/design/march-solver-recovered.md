# March Solver Discussion — Recovered Digest

Source: `vixen-build-system-design-6a47d39d.md` (ChatGPT share export, conversation
dated 2026-03-11, 67 messages). All line numbers below refer to that source file.

**This is recovered INTENT from an earlier design iteration, not a spec.** User
turns (Amos) are proposals; Assistant turns are critique, sometimes accepted,
sometimes rejected in the next User turn. Nothing here is a decision — decisions
are the ones pinned in the July cargo-cycle notes. Where March's own back-and-forth
already reversed a position, both sides are recorded.

---

## 1. Complete map of every major thread

**Convergent nodes (`#[key]`/`#[join]`).** The seed idea, present from message 1
onward: a `#[convergent] fn resolve_pkg(#[key] id, #[join] features, ...)` where
calls to the same key unify into one demand-graph node and `#[join]` arguments
merge via a declared monotone operator (lines 76–98, 202–219, 335–337). Survives
essentially unchanged through the whole conversation — every later revision keeps
`#[key]`/`#[join]` as the stable core, even after the "20 keywords" pushback
(lines 3134–3163).

**Tiering (Tier 1/2/3).** First appears as a three-tier execution model: Tier 1
aggressive parallel search assuming a solution exists, Tier 2 sequential CDCL
fallback for completeness, Tier 3 diagnostic/explain-only pass (lines 338–373).
Re-derived at least three times with shifting responsibilities — by message 17 it
is Tier 1 = floor discovery, Tier 2 = the Squeeze (exhaustive optimization), Tier
3 = diagnostic fallback triggered only on total failure (lines 2166–2187).

**Range widening.** Introduced at lines 738–744, 761 as turning a point no-good
into a range no-good by scanning adjacent registry versions for identical
manifest-level dependency declarations. Repeatedly singled out by critique as the
single strongest, most defensible idea in the whole design (lines 836–848,
1004–1005, 4841–4843) — closest prior art is Dart `pub`'s existing adjacent-version
collapsing (lines 4609, 4733, 4758–4769).

**Floor / incumbent.** "Floor" = first valid solution found by greedy parallel
descent, later reframed as just "the first incumbent" — a search-produced lower
bound, not an algebraically special point (lines 729–737, 1023–1058, 1824–1830).
The final semantic center offered is: constraint state monotone, feasible region
monotone, **incumbent monotone under Cargo's dominance order** — individual
package picks are not monotone (lines 1090–1105).

**The "squeeze" framing.** Amos's name for parallel-descent-to-floor followed by
exhaustive-ascent-to-ceiling (lines 725–736, 772–776). Critiqued hard at first
(floor doesn't prove a solution exists unless it's actually valid; "stop
upgrading, keep the floor" contradicts an exact-Cargo-compatibility goal — lines
864–907) then narrowed once Amos clarified the squeeze doesn't stop at "good
enough," it stops at Cargo's exact optimum (lines 1023–1026, 1047–1058).

**Semantic volumes / orthotopes.** The frontier is represented as regions
("volumes") — hyper-rectangles (orthotopes) over per-package version intervals,
plus fixed assignments, context, and local exclusions (lines 1241–1259,
1520–1521, 1675–1700, 2158–2161). Repeatedly flagged as too narrow a
representation on its own (lines 1751–1768, 2033–2046) — real exclusions may be
unions of intervals or feature-conditional shapes.

**No-goods and Tier-1 exclusions.** No-goods evolve from exact version tuples
(lines 183–184) → terms/ranges (line 252) → a formal `ExclusionKind::Proven |
Heuristic` split (lines 1719–1725) → a further split of "Proven" into
Manifest-Proven vs Build-Proven (lines 2014–2029, 2182–2184). Tier-1's "coarse
no-goods" are flagged as the single most persistent soundness risk across the
whole conversation (lines 343, 535–552, 1704–1730, 1915).

**Assumption/fact split.** The recurring fix for the Tier-1 danger: distinguish
values that are monotone accumulated *facts* from values that are branch-local,
retractable *assumptions* (lines 1979–1987, 2620–2650, 3144–3163). This is the
single idea that survives every rewrite and is explicitly praised as "the
biggest improvement" in message 16 (line 1981).

**Solver-as-optimizer.** A running correction: the design keeps claiming
"convergence"/"no backtracking" and keeps getting corrected back to "this is
branch-and-bound with monotone learned facts" (lines 500–533, 926–947,
1990–1997, 2411–2506). The NP-hardness exchange (below) is the clearest
statement of this correction.

**Metadata-stable blocks.** The idea that the *same* registry-metadata scan
which powers range widening should also define where the frontier splits and
where explanations draw boundaries — "the same metadata scanner that widens
no-goods should also define split boundaries" (lines 1319–1341, 2355–2363,
2320–2353 for the concrete `MetadataSlice` sketch).

**Distributed fact caching + gossip bus.** Learned no-goods hashed and stored in
a shared cache so new clones "warm start" from prior conflicts (lines 363–367,
1537–1552). Initially claimed as "Registry-Universal truth" (line 592) — flagged
as the biggest single overreach in the whole document (lines 574–617) — and
walked back to "Snapshot-Scoped and Assumption-Scoped" (lines 1937–1940,
2007–2010). The "lock-free gossip bus" transport detail is explicitly demoted
from semantics to implementation (lines 1770–1783).

**Proposed solver-language constructs.** The most direct antecedent of today's
`solve` primitive question. Message 22 proposes a full second layer of syntax —
`solver`, `#[choice]`/`choose`, `#[assume]`/`volume`, `objective`, `bound`,
`split`, `learn`/`widen`, `persist scoped_by` (lines 2760–3007) — explicitly to
express floor/incumbent/widening without hardcoding Cargo in Rust. Amos rejects
this as too large a surface in message 23 (lines 3079: "you also just invented
20 keywords and made a system so flexible it's impossible to implement
generically"). Message 25 retreats to a minimal single addition — see §3(i).

**Cache trust model.** "Replayable": a consumer of a cached fact must be able to
re-run the trace and verify it before accepting it into local search, rather
than trusting the cache blindly (lines 2185, 2251–2255). Praised as "excellent"
and as fixing "the biggest social/engineering risk in distributed learned
facts" (lines 2251–2256).

**Provenance.** The `vixen explain` narrative recurs at every revision
(lines 355–361, 767–770, 1541–1547, 1929–1935) settling into a 4-layer model:
Witness / Join / Pruning / Exclusion. Critiqued for over-centering the
solver-internal "Unique Implication Point" concept (lines 554–572) in favor of
demand-chain / accumulated-requirements / learned-incompatibility / exhausted-
candidate-set explanations users actually want.

**NP-hardness discussion.** A direct question from Amos ("I thought our problem
was NP hard?", line 2389) after several rounds of AI praise. The answer:
NP-hardness is about worst-case behavior; nothing in the design escapes it; what
the design can plausibly improve is constant factors, average case, parallelism,
pruning power, incrementality, explainability, cache reuse — the same category
SAT solvers are in (lines 2393–2506).

**Use cases beyond package managers.** A late thread (messages 26–27, lines
3507–3826): npm/pnpm/yarn (harder — placement/hoisting identity, not just
name), Maven/Gradle (good fit for BOM/nearest-wins policy), pip (environment
markers), Nix (only *parts* — derivation evaluation and rebuild explanation, not
a wholesale language/module-system replacement), plus non-package-manager
candidates: build/feature-matrix solving, CI test selection/sharding,
toolchain/SDK selection, distributed build planning, deployment/config
resolution, data pipeline planning, IDE incremental analysis (lines 3656–3724).
Concludes: "Vixen is for justified demand-driven planning problems" (line 3798).

---

## 2. Four targeted sections

### 2a. FEATURES and feature unification

- Original framing, `cumulative` at the call site (lines 73–91):
  > "In Cargo-style resolution, if two paths demand the same package with
  > different features, those features must be unified... We introduce the
  > `cumulative` keyword at the call site to signal that an argument is a
  > Lattice Point." (lines 75–76)

- First critique: declaring monotonicity should be the *callee's* contract, not
  a call-site annotation, citing Datafun/CALM (lines 137–139):
  > "Whether joining `features` is legal is not really a property of one call;
  > it is a property of the **callee's semantics**... I would make the function
  > declare its key fields and join fields, and let the call site merely
  > participate in that declared convergence regime." (lines 137)

- Cargo resolver v2 specifically does **not** unify features everywhere (lines
  269, 481–498):
  > "Cargo can activate multiple semver-incompatible versions of the same crate
  > in one graph, and resolver v2 deliberately does **not** unify features
  > across some contexts such as target-specific deps, build-deps/proc-macros,
  > and some dev-dep situations." (lines 269)

- The order-theoretic mismatch between features (union, upward) and version
  requirements (intersection, downward-restrictive) under one `#[join]` keyword
  is raised twice, in nearly identical language (lines 460–479, 621–634):
  > "For features: more demands means larger set, join is union, that is a
  > normal upward-growing lattice. For version requirements: more demands means
  > fewer allowed versions... So the runtime model needs one of these: either
  > `#[join]` does not mean 'grow upward' in one universal sense, but 'merge
  > according to a declared partial order'..." (lines 456–470)
  > "features: join = union / requirements: join = intersection / learned
  > incompatibilities: join = union / candidate versions: derived monotone
  > narrowing state, not an input join. That is a product domain, not one
  > generic 'join means widening' story." (lines 474–479)

- Final settled shape before the language-surface fight (lines 282–288):
  ```
  state Resolve(key: PackageInstance) {
    demand_features : Set[Feature]        // union
    demand_req      : VersionSet          // intersection
    learned         : Set[Incompat]       // union
    candidates      : VersionSet          // monotonically shrinking
  }
  ```
  Note: this is features-and-versions-joined-inline, in the *same* recursive
  call — March never proposes literal two-phase resolution (resolve versions,
  then separately unify features). See tension (iii) below.

- MVP staging explicitly isolates feature union as buildable *before* any
  version choice exists at all (lines 3901–3932, "Milestone 1 — No search, only
  convergent demand"): success criterion is "you can express Cargo-style feature
  union and version-range intersection in Vixen" with zero Cargo-specific
  runtime code.

### 2b. Recorded FAILURE MODES and dragons

- **Tier-1 coarse no-goods, repeatedly flagged as dangerous.**
  > "Tier 1 coarse no-goods are dangerous. You already noticed the problem by
  > inventing Tier 2. That means the architecture admits Tier 1 can
  > over-prune... If you do not separate them, your shared cache becomes
  > polluted by search artifacts." (lines 535–552)
  > "'Coarse exclusion range' in Tier 1 is still dangerous... If that exclusion
  > is used by Tier 2 as if it were proven, you can kill valid regions."
  > (lines 1704–1707)

- **`best_v` / current witness is NOT monotone — the counterexample.**
  > "`A ∈ {1,2}`, `B ∈ {1,2}`, learned no-good: `{A=2, B=2}`. If the current
  > state picks `A=2, B=2`, one node may drop to `A=1`. But once `B` later
  > drops to `1`, `A=2` becomes valid again. So a current 'best version' can go
  > down and then back up. What shrinks monotonically is the **set of viable
  > assignments**, not the currently displayed witness." (lines 254–259)

- **Floor validity is not free.**
  > "Finding **any valid floor** is still solving. You may have good heuristics
  > for descending to one, but that is not the same as 'pure lattice
  > convergence.'" (lines 880–882)

- **Cluster independence is much harder than assumed.**
  > "identifying the right clusters is basically part of the solving problem...
  > 'just sweep each cluster in parallel' is nice intuition, but the hard part
  > is discovering the true cluster boundaries without already doing a lot of
  > solver work." (lines 915–924)
  > "'Conflict cluster' is currently a magic object... clusters are not static;
  > they can fuse when a transitive dependency turns out to be shared."
  > (lines 1732–1741)

- **Range-widening soundness conditions.** Sound only if the failure reason is
  metadata-stable across the widened interval, and must be *verified*, not
  assumed:
  > "The scan needs to be precise, not interpolated... if A@1.5 depends on C
  > ^3.0 but A@1.5.1 switches to C ^2.0, the range has a hole." (lines 794,
  > 969–975 — Claude's independent commentary embedded in the user turn at
  > lines 793–795 makes the same point about feature-conditional causes.)

- **Version-range holes.** Ranges cannot be assumed contiguous:
  > "You cannot assume contiguity just because nearby versions look similar.
  > The solver should learn something more like a **union of compatible
  > intervals / regions**, not necessarily one solid range." (lines 971–973)

- **Node identity / "single node for serde" is too coarse.**
  > "The real key is not 'package name' and maybe not even 'package ID' in the
  > casual sense. It is closer to: `PackageInstanceKey = (name, source,
  > compatibility class, target context, dependency role, apex
  > partition...)`" (lines 494–496)

- **"Registry-universal" is the biggest single overreach in the document.**
  > "Usually that is **not** a universal truth. It may depend on: target
  > triple, compiler version, feature set, build flags, optional transitive
  > deps, system libraries, resolver policy, source revision, patch overlays...
  > cached facts need a scope." (lines 594–611)

- **Replay keys for build-proven exclusions need to be much wider than
  "environmental witness."**
  > "I'd still expect the cache key to include at least hints of: registry
  > snapshot, package source/revision, relevant feature set, target/toolchain,
  > maybe patch/override set. Not because the idea is wrong, just because
  > 'environmental witness' alone sounds too narrow for build-derived facts."
  > (lines 2299–2309)

- **Manifest-proven vs build-proven conflated under one "proven" label.**
  > "Metadata-based proof is structural and usually reusable under clearly
  > defined assumptions. Compiler-failure proof is much trickier. It may
  > depend on toolchain version, target, feature set, host state, source
  > patching, and even nondeterministic build conditions." (lines 2019–2023)

- **Floor-derived dominance order can quietly poison determinism — the single
  strongest recommendation in the whole document.**
  > "If Cargo-compatibility is a hard goal, the ordering should come from
  > something **independent of the discovered floor**... Using the floor as
  > the source of order is elegant, but it risks leaking heuristic
  > nondeterminism into the supposedly exact phase. That is the single biggest
  > thing I would still change." (lines 2277–2285)
  > "If you only tighten one thing before prototyping, make it this: **remove
  > floor-derived package priority from the formal ordering.** Everything else
  > can evolve during implementation. That one can quietly poison
  > determinism if it stays." (lines 2311–2318)
  — Note: this critique was raised in message 18 but the *next* proposal
  (message 17 actually precedes it chronologically; re-check ordering) still
  ships "Minimum Topological Depth (distance from root in the floor solution)"
  as the priority source (line 2165), i.e. Amos had not yet incorporated the
  fix by the end of this thread.

- **"20 keywords, impossible to implement generically" — Amos's own rejection
  of the solver-language-constructs branch.**
  > "it doesn't fit Vixen's dataflow language with demand driven evaluation.
  > You also just invented 20 keywords and made a system so flexible it's
  > impossible to implement generically. Read the current spec to get a better
  > idea of where things fit." (line 3079)

### 2c. DETERMINISM and Cargo-compatible ORDERING

- First (informal) statement — lexicographic package name, greedy preference:
  > "Prioritized Ascent: During the 'Exhaustive Ascent' phase, we prioritize
  > packages by their lexicographical name. Greedy Preference: We always
  > prefer the solution that has the highest version of the highest-priority
  > package." (lines 763–764)

- Demand for a precise total order once "same as Cargo" becomes the actual
  termination condition:
  > "You need Cargo's ordering stated precisely... Not just: highest version
  > first, lexicographic package tie-break, but the actual comparison rule
  > over whole resolved graphs. Because the squeeze needs a proof obligation
  > like: 'no unexplored assignment can dominate the current incumbent under
  > Cargo order.'" (lines 1109–1121)

- Formal dominance relation, v1 — by package name only:
  > "G_1 \succ G_2 if, at the first package P (ordered by name) where they
  > differ, Version(P, G_1) > Version(P, G_2)." (lines 1533–1534)

- v2 — ordered by depth then name, and immediately flagged as underspecified:
  > "Priority: Packages are ordered by Minimum Topological Depth (distance
  > from root in the floor solution), then lexicographical name." (line 2165)
  > "'ordered by depth, then name'... I would still want the spec to define
  > exactly what 'depth' means when the same package appears through multiple
  > paths or when cluster fusion changes the apparent structure. You probably
  > want a canonical package-instance ordering derived from the resolved
  > dependency projection, not an intuitive tree depth." (lines 2048–2057)

- The floor-as-order-source problem is the determinism thread's unresolved
  end state — see §2b above (lines 2269–2318). No later message in this
  document revisits or fixes it.

### 2d. UNIVERSES / cross-compilation / target-specific dependencies

**Largely absent as a dedicated thread.** Target/platform context appears only
as one dimension folded into the node-identity and volume-context discussions,
never explored on its own:

- Node identity: "...at least some combination of package, platform, dependency
  kind, source, and build context" (line 141); "target/platform context...
  target-specific edges, build/proc-macro contexts" (lines 489–490, 917–921).
- Volume `Context` field: "Environmental witnesses (Target Triple, OS) and
  registry snapshot" (line 2161); `#[context] target: TargetTriple` as a
  parameter class (lines 2687, 2973–2974, 3206–3207 region, 2192–2193).
- Cargo's actual target-specific/build-dep/proc-macro non-unification is cited
  once, factually, as evidence node identity must be finer-grained (line 269) —
  not discussed as a resolution-semantics problem in its own right.

No message in this conversation discusses cross-compilation as a first-class
concern, multi-target resolution (resolving for host + target simultaneously),
or how `#[context] target` participates in the join/assumption split. This is a
gap in March's exploration, not a settled absence.

---

## 3. Tension list against today's decisions (2026-07-03)

Catalog only — no resolution attempted here.

### (i) `solve` is a runtime primitive; language stays fully declarative — NO search constructs in vix

**What March explored.** Message 22 (lines 2760–3007) proposes a full second
syntactic layer for search: `solver { ... }` blocks, `#[choice]`/`choose` (a
search variable over a finite domain), `#[assume]`/`volume { assume ... }`
(branch-local refinement), `objective`, `bound`, `split`, `learn`/`widen`, and
`persist scoped_by`. Amos rejected this outright in message 23 (line 3079) as
"20 keywords... impossible to implement generically." Message 25's walk-back
(lines 3122–3505) is the closest prior-art analogue to today's decision: it
argues the *only* new primitive needed is one branch-local parameter class
(`#[assume] volume: Volume`, lines 3199–3235) plus a single generic runtime
entrypoint — `solve(resolve_pkg) where { widen_conflict, metadata_change_points,
compare: cargo_compare }` (lines 3446–3452) — into which ordinary Vixen
closures are plugged, with **no new syntax at all** for widening or splitting
(lines 3392–3399, 3406–3425: "No new syntax needed there at all").

**What each of the rejected constructs served, since the needs don't
disappear:**
- `#[choice]`/`choose` — expressing the search domain (which versions are
  live candidates) as distinct from ordinary joined demand.
- `#[assume]`/`volume` — branch-local restriction for parallel exploration,
  explicitly *not* a global join (line 2635: "These are not joins. They are
  local refinements.").
- `objective` — Cargo's dominance/comparison order as a pluggable, swappable
  function, not hardcoded.
- `bound` — the admissible optimistic upper bound per region, used for
  dominance pruning.
- `split` — the region-decomposition/frontier-splitting policy.
- `learn`/`widen` — turning a structured conflict into a persistent no-good,
  including range widening.
- `persist scoped_by` — the assumption/snapshot scope key for cached facts.

**What today says.** `solve` is a runtime primitive; vix itself carries no
search constructs.

**Question a reconciliation must answer.** March's own end state (message 25)
already converges on "one runtime primitive + ordinary closures," which is
compatible in spirit — but it still wanted one new parameter *annotation*
(`#[assume]`) to distinguish branch-local values from joined values inside the
callee's own signature. If today's rule is genuinely zero new search syntax,
how does the runtime distinguish, at the call site of `solve`, which inputs are
monotone joins (persist across the whole resolution) versus which are
branch-local search state (scoped to one region/attempt) — without any
annotation living in the function signature itself? Is that distinction pushed
entirely into the shape of the values passed to `solve`, or does it disappear
because "only version choices are decision points" (per decision iii) already
answers it?

### (ii) The solve RESULT is pinned in the journal and is the authority; recompute-from-pins is an audit

**What March explored.** No discussion of a journal or pinning in this
document at all. The closest analogue is the **distributed fact cache /
"Replayable" trust model**: learned no-goods (negative facts about *excluded*
regions) are cached and reused across machines, but only after the consumer
re-verifies them by replaying the trace (lines 2185, 2251–2256: "'Replayable'
is the right word... you do not need blind trust in somebody else's cache
artifact"). This is a cache of *pruning knowledge*, not a pin of the *positive
result*. March's caching story was walked back from "Registry-Universal" to
"Snapshot-Scoped and Assumption-Scoped" (lines 574–617, 1937–1940) — i.e.
even the negative-fact cache needed a stated trust/scope boundary, which is at
least structurally similar to "pin is authority, recompute is audit."

**What today says.** The solve result itself (the resolved graph / version
choices) is pinned in the journal and treated as authoritative; recomputing
from pins is only an audit path, not the source of truth.

**Question a reconciliation must answer.** March never needed to ask "is a
cached *positive* result trustworthy on its own, or must it always be
re-derived and merely checked against the cache?" — it only ever cached
*negative* exclusion facts, always meant to accelerate a from-scratch search,
never to replace it. Under today's model, does the "pin is authority" rule mean
the whole Proven/Heuristic and Manifest-Proven/Build-Proven exclusion-cache
apparatus (lines 1719–1725, 2014–2029) becomes unnecessary — because the
journal pins the *outcome* directly and audit-recompute either reproduces it or
raises an alarm, with no need for a separate no-good cache to accelerate that
recompute? Or does the no-good cache still have a role *inside* the audit
recompute, and if so, does IT also get pinned, or does it stay
scoped/replayable the way March described?

### (iii) Feature unification happens INSIDE solve, not two-phase; PubGrub-style: feature/optional-dep activations as propagated terms, only version choices are decision points

**What March explored.** March never explicitly proposed literal two-phase
resolution (resolve-versions-then-separately-unify-features) — features were
always accumulated *inline*, in the same recursive `resolve_pkg` call, via
`#[join(union)] features` alongside `#[join(intersect)] req` (lines 282–288,
2684–2686, 2970–2971, 3202–3204). So on the narrow question of "one phase vs.
two," March's model is not in direct conflict with today's decision. The gap is
in the *formalization*: March treated feature accumulation as a monotone
Set-union join exactly like version-requirement intersection — never as
PubGrub-style propagated terms/Horn-clause implications derived from manifests,
and never distinguished "decision points" from "propagated consequences" for
anything other than the version choice itself (`find_best_fit` in the original
sketch, later `#[choice]`/`choose` in message 22).

**What today says.** Feature and optional-dependency activation are propagated
terms (Horn-clause implications sourced from manifests) inside the same
PubGrub-style solve; version choices are the *only* actual decision points —
everything else, including feature activation, is a forced consequence of
propagation, not a separate accumulation phase or a separate kind of join.

**Question a reconciliation must answer.** March's `#[join(union)] features`
model assumed feature activation is *purely monotone and never retracted* —
once a feature is demanded, it stays demanded, forever, for the life of the
apex (this is explicit in the "features: join=union, an upward-growing
lattice" framing, lines 456–459). PubGrub's unit propagation, in contrast, can
derive both positive *and* negative literals from Horn-clause implications, and
those derivations get undone on backtrack past the decision level that
justified them. Does treating feature activation as "propagated terms" mean
March's assumption of pure monotonicity for features no longer holds — i.e.
can a feature's activation now be *retracted* on backtrack the way a version
choice can, and if so, does that reopen the Tier-1/Tier-2 proven-vs-heuristic
distinction (§2b) for feature facts too, not just version no-goods?

---

## 4. Verbatim quotes worth preserving

> "dependency resolution is not a pre-processing script, but a convergent
> query over a demand graph" (line 332, quoted back approvingly at line 426)

> "not 'no backtracking,' but 'no blind backtracking.'" (line 265)

> "reactive incompatibility learning with propagated domains and explicit
> decisions." (line 307)

> "So the solver's knowledge may converge monotonically, while the current
> witness does not." (line 522)

> "at all times, Vixen maintains one known-valid solution and a frontier of
> not-yet-disproven better solutions." (line 1086)

> "the same metadata scanner that widens no-goods should also define split
> boundaries." (line 1333)

> "The frontier splitter should be a lexicographic branch-and-bound region
> scheduler, not a naïve multidimensional partitioner." ... "best-first
> exploration of disjoint version volumes with lazy exclusion carving."
> (lines 1488, 1492)

> "assumption-scoped learned incompatibility cache" (line 615 — the proposed
> rename away from "global fact store")

> "every committed result and every globally shared pruning step is backed by
> a formal Provenance Object; local Tier 1 heuristics are explicitly
> non-proven and non-persistent." (line 2108)

The seven-step proof skeleton (lines 1843–1851), offered as "now a recognizable
formal program":
> 1. Every emitted incumbent is valid.
> 2. Every live frontier volume contains only assignments consistent with
>    current proven exclusions and joins.
> 3. Every split partitions a parent volume into disjoint children whose union
>    equals the unexplored admissible part of the parent.
> 4. Every proven exclusion removes only inadmissible assignments.
> 5. Every dominance prune removes only assignments that cannot beat the
>    incumbent under `≻`.
> 6. Termination occurs when no live volume remains.
> 7. Therefore the final incumbent is optimal.

> "If you only tighten one thing before prototyping, make it this: remove
> floor-derived package priority from the formal ordering... That one can
> quietly poison determinism if it stays." (lines 2313–2318)

> "Could a user implement a Cargo-like resolver and a different exact package
> solver in the same runtime by swapping only Vixen code?" (line 3044 — the
> proposed test for whether the language is expressive enough)

> "Rust owns only generic machinery. Vixen owns domain semantics... If you
> violate this once, you'll keep violating it." (lines 3881, 3899)

> "The key milestone to watch... is Milestone 5: can the runtime stay generic
> once compare/split/learn are all supplied in Vixen? If yes, you've avoided
> the 'Cargo logic in Rust' trap. If no, stop and redesign there before adding
> more machinery." (lines 4249–4254)

> "a small extension of the existing demand-driven model, not a second
> declarative subsystem." (line 3488)

> "what is the smallest runtime interface that lets ordinary Vixen code
> describe those pieces?" (line 3494)

> "Vixen is for justified demand-driven planning problems." (line 3798)

> "Because 'good design' and 'not NP-hard anymore' are completely different
> claims." (line 2411)

> "an exact NP-hard solver with a plausible plan for making many real
> instances much faster and much more understandable." (line 2501)

> "ASP is a solver formalism; Vixen is a runtime model." (line 5045)

> "HyperRes gives you a better package-universe IR than Vixen currently has;
> Vixen is trying to give you a richer execution model than HyperRes is trying
> to provide." (lines 5274–5275)

> "PACSOLVE is closer to 'a customizable exact dependency solver.' Vixen is
> closer to 'a host runtime where customizable exact solvers are just one kind
> of program.'" (lines 5285–5286)

> "HyperRes challenges Vixen's front-end novelty. PACSOLVE challenges Vixen's
> solver-definition novelty. Neither really eliminates Vixen's runtime-level
> novelty claim." (lines 5340–5342)

> "A demand-driven incremental runtime can be extended with exact search and
> provenance without collapsing into an opaque external solver." (line 5417)

> "the best tool in the world for understanding and optimizing Cargo
> dependency resolution." (line 5592)
