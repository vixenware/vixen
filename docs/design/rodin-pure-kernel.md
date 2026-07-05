# Rodin pure kernel: a mutation-site ledger for vix hosting

Spike B groundwork. Every mutable structure in today's `rodin-core`
(crates/rodin-core/src/lib.rs, crates/rodin-facts/src/*), named with its
access pattern, measured op count on the known workload, its persistent
reformulation, and its lowering story. Companion to
docs/design/rodin-in-vix.md (the position) and
docs/design/rodin-fact-validity/adjudication.md (why the fact layer moves
first). House rule: every claim below is either a line citation or a
number from rodin-c-evidence.md / rodin-churn-taxonomy.md — no
restatement of the position doc's language where the code says something
more specific.

## 0. Correction to the position doc's vocabulary

rodin-in-vix.md talks about "trail" and "watch lists" — standard CDCL
vocabulary. **Neither term occurs in rodin-core** (`grep -i
"watch\|trail" crates/rodin-core/src/lib.rs` — zero hits). The actual
architecture is a CSP-style domain-narrowing search over per-package
`VersionSet` intervals, with conflict-driven **region** learning (not
clause learning over literals) and a fixpoint clause-application loop
scanned to quiescence — not incremental two-watched-literal propagation.
This is a materially different, and for the pure-kernel argument a more
favorable, starting point: the "trail" is already `State`, and it is
already **cloned wholesale, not mutated in place**, on every decision
branch (`Solver::search`, lib.rs:2098, `let mut branch = state.clone();`).
The recursive-DFS-over-owned-clones shape is closer to a persistent
search tree today than the CDCL framing suggested. The genuine mutation
lives elsewhere: in the `Solver` struct's fields, which are threaded
imperatively via `&mut self` across the whole recursion (learned-fact
store, activity map, counters), and in the O(state) fixpoint-detection
fingerprint recomputed every propagation round. Sections 1–3 retarget the
ledger at the real structures.

The workload cited throughout: the lock-union/dev projection solve after
the activity+restart fix (rodin-c-evidence.md:3597,4086,4102) —
**457 decisions, 69 conflicts, 1 restart, 24 active learned facts,
13,331 learned-fact scans, 39 prunes, 42 subsumed, 3 replaced, 1,209
candidate observations, 274 versions killed by widening vs 24 point
versions, 3,464 ms wall**. Where a structure's op count isn't broken out
in the evidence log, it's marked "not separately measured" rather than
guessed.

## 1. Mutable-structure ledger

### 1.1 `State` (lib.rs:756–762) — the search node

```rust
struct State {
    domains: BTreeMap<PkgIx, Domain>,             // per-package allowed VersionSet + selection
    features: BTreeSet<(PkgIx, FeatIx)>,
    feature_reasons: BTreeMap<(PkgIx, FeatIx), Vec<Premise>>,
    hypotheses: Vec<Hypothesis>,                  // decision stack — THIS is the trail
    applied_clauses: BTreeSet<ClauseId>,           // which compiled clauses fired already
}
```

**Access pattern**: single-owner per recursion frame, but the "ownership
transfer" is a **deep clone**, not a move — `search` clones `state` once
per candidate version at lib.rs:2098, then recurses with the clone owned
by the child frame. Every `Domain` (its `VersionSet`, itself a
`Vec<Interval>`), every `BTreeSet`/`BTreeMap` entry, is copied. This is
the single largest allocation source in the loop: **457 decisions ⇒ at
least 457 full `State` clones** (one per successful branch descent; more
per exhausted-then-backtracked candidate, since the `for version in
candidates.into_iter().rev()` loop clones once per attempted candidate,
not once per accepted one — `candidates_existing` = 1,209 is the actual
clone-attempt upper bound, since each candidate observation is preceded
by a `state.clone()` at lib.rs:2098 before the recursive call).

Within one `State`, fields mutate at different granularities:
- `domains: BTreeMap<PkgIx, Domain>` — random-write, keyed by package;
  `intersect_domain` (lib.rs:2415) does read-modify-write on one entry
  per call. Not separately counted, but every one of the 13,331 learned
  scans that reaches `intersect_domain`, plus every one of the (index
  size) compiled-clause fixpoint checks per `propagate` round, is a
  candidate write.
- `applied_clauses: BTreeSet<ClauseId>` — write-once-per-clause-per-node
  (a clause fires at most once per state, lib.rs:2182,2190), monotone
  growing set within a branch, reset (via clone divergence) on backtrack.
- `hypotheses: Vec<Hypothesis>` — push-only within a branch (line 2153),
  the actual decision trail; length bounded by decision depth, not total
  decisions (backtracking discards the popped suffix by simply not
  carrying it into sibling clones — there is no explicit pop).

**Functional formulation**: `State` is *already* the persistent value
Perceus/vix wants — it just isn't implemented as one. Swap `BTreeMap`/
`BTreeSet`/`Vec` for immutable, structurally-shared collections (HAMT or
size-balanced persistent tree; vix's own canonical `Map` — see
machine-lowering.md:287, "canonical Maps, sorted pairs, last-wins,
history-independent hashes" — is exactly this shape already) and the
`state.clone()` at line 2098 becomes O(1) (pointer/root copy) instead of
O(|domains| + |features| + |hypotheses| + |applied_clauses|). No
algorithm change is required — `search`'s recursive-clone-per-branch
structure is already expressed as "take an immutable value, produce a
derived immutable value, recurse." This is the cleanest port in the
whole kernel: the Rust code's *shape* already matches the target
semantics; only the collection *representation* is wrong.

**Lowering story**: `State` is **shared** in the persistent sense (`Rc`/
`Arc`-style structural sharing across sibling branches) but each branch's
*view* of it is single-owner from creation to either (a) becoming a
sibling's input via the next `state.clone()` or (b) being the terminal
returned solution/failure. Under Perceus-style reuse analysis, sibling
branches genuinely alias the same tree nodes (no copy), and only the path
from root to the mutated leaf pays a copy — this is exactly what
structural sharing is for and requires **no reuse analysis at all**,
just persistent collections. Reuse analysis only buys something on the
*single-threaded* portion — the sequence of `intersect_domain` calls
*within* one `propagate` fixpoint loop, where each call's input `Domain`
is dead after producing the next one (`domain.allowed = next;` at
lib.rs:2502 is a last-use of `domain.allowed`'s old value). That in-place
mutation is a legitimate reuse-analysis target; the tree-level branching
is not — don't conflate the two.

### 1.2 `Solver<'a>` — the imperative spine across the whole recursion

lib.rs:779–808. This struct is **not** cloned; it is threaded by `&mut
self` through every recursive call to `search`/`propagate`/`learn`. It
is the actual "global mutable state" of the algorithm, and everything in
it survives backtracking (this is intentional — learned facts and
activity scores are cross-branch knowledge, the entire point of CDCL).

| Field | Access pattern | Measured op count |
|---|---|---|
| `learned: Vec<LearnedNoGood>` | append-only push (`push_learned_fact`, lib.rs:2776), **soft-delete via `active: bool`** flip (never truncated) | 24 active at solve end; subsumption flips `active=false` on 3 (replaced) |
| `learned_by_package: BTreeMap<PkgIx, BTreeSet<usize>>` | derived index, rebuilt wholesale on subsumption (`rebuild_learned_index`, lib.rs:2782, O(learned.len()) BTreeSet clears + full reinsert) | rebuilt whenever `removed > 0` in `install_learned_fact` — 3 times (= `learned_replaced`) on this workload |
| `learned_without_packages: BTreeSet<usize>` | same rebuild cadence as above | same |
| `activity: BTreeMap<PkgIx, f64>` | **every entry decayed on every conflict** (lib.rs:2808 `for value in self.activity.values_mut() { *value *= 0.95; }`) — full-map scan, not lazy | 69 conflicts × |packages with nonzero activity| multiplications; NOT the lazy-global-multiplier trick the position doc assumed — see §1.2.1 |
| `conflicts, decisions, restarts, …` counters | scalar increment | 69, 457, 1 respectively |
| `derivations: DerivationStore`, `exclusions: ExclusionStore`, `clauses: ClauseStore` (on `CompiledIndex`, immutable per solve) | **append-only Vec, already persistent** — see §1.3 | derivations: 1 per conflict + 1 per widen (≤ 69 + widen count); exclusions: 1 per installed learned fact (24) |
| `steps: Vec<SolveStep>`, `trace: Vec<String>` | append-only, diagnostic/replay trace | one entry per decision/conflict/intersect/learn event — not separately counted, order of decisions+conflicts+intersections |

**1.2.1 — the activity decay is not the "lazy trick" the position doc
claimed.** rodin-in-vix.md:32–33 asserts "VSIDS decay: the standard impl
is ALREADY the functional trick (lazy global multiplier)." The actual
code at lib.rs:2804–2813 does the opposite: it eagerly multiplies *every*
entry in the `activity` map by 0.95 on *every* conflict, and only bumps
the packages actually involved. This is O(|active packages|) per
conflict, not O(1). The classical MiniSat trick — keep a global
`var_inc` multiplier and only scale it, dividing individual bumps by the
accumulated decay lazily on read — is not implemented here. **This is a
real finding, not a nitpick**: it means the activity map is a genuine
per-conflict full-scan mutation site (bounded by package count, not
propagation count, so cheap at this workload's scale, but the "already
functional" claim in the position doc is false for this field and should
not be repeated uncorrected). Functionally, this becomes either (a) an
honest persistent map rebuilt every conflict (same asymptotic cost,
correct semantics, no gap), or (b) if the decay rate matters at scale,
the actual MiniSat lazy-multiplier reformulation — which is *more* work
than "port as-is," not less. Recommend (a) for the port; (b) is a
separate, optional optimization identical in Rust or vix.

**Functional formulation of `Solver`'s fields**:
- `learned: Vec<LearnedNoGood>` with soft-delete → append-only persistent
  vector plus an `active` flag in the (immutable) element, exactly as
  written today; no change needed beyond the collection type.
- `learned_by_package` / `learned_without_packages` → **derived
  incremental maps**, not independently-mutated state. They are a pure
  function of `learned` (`index_learned_fact`, lib.rs:2790, computes them
  from one fact). In vix terms this is exactly the "projection-narrowed
  memo" / two-tier fine-grained-dependency shape already ruled for the
  language (machine-lowering.md:333–347): a query `learned_by_package(pkg)
  = {ix | learned[ix].active && pkg ∈ mentioned_packages(learned[ix])}`
  that memoizes per-package and invalidates only on writes to `learned`
  that touch that package's membership — the machine maintains this
  automatically once `learned` is a traced value; today's Rust hand-rolls
  the identical incrementality (`rebuild_learned_index` is a full
  recompute standing in for what a real incremental index would do
  online).
- `activity: BTreeMap<PkgIx, f64>` → persistent map, rebuilt per
  conflict as above (§1.2.1).

**Lowering story**: `Solver`'s fields are **uniquely owned for the
duration of one `solve()` call** — there is exactly one `Solver` per
top-level `solve()` invocation (lib.rs:1859), never shared, never
aliased, never read by a sibling branch. This is the strongest
uniqueness case in the kernel: reuse analysis can turn `learned.push` and
`activity` rebuilds into genuine in-place mutation with zero copying,
because there is provably (by construction, not by inference) only one
reference to `Solver` alive at any point in the recursion — `&mut self`
in Rust already proves this; a vix compiler doing whole-program
uniqueness inference on a single linear thread through `search` would
derive the same fact from the call graph shape (every call site either
consumes and returns the value or holds the only live reference).

### 1.3 The append-only stores: `DerivationStore`, `ClauseStore`, `ExclusionStore`

crates/rodin-facts/src/{derivation,clause,exclusion}.rs. **These are
already exactly the target shape** — no gap to close:

```rust
pub struct ClauseStore { clauses: Vec<Clause>, by_package: BTreeMap<..>, by_feature: BTreeMap<..> }
impl ClauseStore {
    pub fn append(&mut self, clause: Clause) -> ClauseId { /* push, return index */ }
}
```

`append` never mutates an existing entry; `ClauseId`/`DerivationId`/
`ExclusionId` are stable indices into a Vec that only grows. This is
arena allocation with a content-independent (positional) identity today.
**Subsumption as content-address dedupe** (position doc line 35–37,
citing the 9,390-duplicates-of-2 finding at rodin-c-evidence.md:3596):
the fix2 row shows subsumption collapsing 9,392 conflicts' worth of
learned facts down to 2 surviving active regions after 92,398 scans — the
other 9,390 were *subset* duplicates, not merely *equal* duplicates
(`region.contains_region`, region.rs:78, is a subset test, not equality).
That means naive hash-consing on the `Region` value (giving free
equality-dedupe for free under vix's V10/V13 content-identity discipline)
would only catch the equal-region subset of that 9,390, not the
subset-containment subsumption `install_learned_fact` performs at
lib.rs:2727–2764. **Content-addressing gets you dedupe of exact repeats
for free; it does not subsume subsumption** — the `contains_region`
subset scan (O(active learned facts) per install, today a linear scan at
lib.rs:2732) remains necessary logic, just over a store whose element
identity is now a hash instead of an index. This is worth stating
precisely because the position doc's phrasing ("subsumption becomes
hash-dedupe natively") overclaims: hash-dedupe handles the *identical*
slice of the pathology; the *containment* slice — which subset relation
was the actual majority of the 9,390 per the table's `subsumed`/
`replaced` split — still needs the comparison logic.

**Lowering story**: keep as append-only vix values (a `Store<T>` machine
primitive backed by the content-hash dedup store already built per
machine-lowering.md:286, "content-hash dedup store, STORE hosts, memo-key
handle canonicalization" — DONE as of that log entry). `by_package`/
`by_feature` are derived indices exactly like §1.2's `learned_by_package`
— same incremental-map treatment.

### 1.4 `VersionSet` and `Region` — already pure

crates/rodin-facts/src/version_set.rs, region.rs. Every method
(`union`, `intersect`, `complement`, `is_subset_of`, `contains_region`,
`subtract`) takes `&self` and returns a new value. **Zero mutation
anywhere in these two files** — confirmed by reading both in full; no
`&mut self` method exists on either type. This is the strongest possible
starting point: these are not "structures that need a functional
reformulation," they are already the functional reformulation, sitting
inside a host language that happens to also allow mutation elsewhere.
Porting them to vix is a value-kind mapping problem (§4), not an
algorithm problem.

### 1.5 `StateFingerprint` — the fixpoint-detection tax

lib.rs:3903–3924. `propagate`'s `loop { ... }` (lib.rs:2176–2200) detects
quiescence by computing `state_fingerprint(state)` **before and after**
every fixpoint round and comparing for equality. `state_fingerprint`
clones every domain's `(PkgIx, bool, Vec<Interval>, Option<Version>)`
tuple and the whole `features` set (lib.rs:3908–3924) — an O(|state|)
allocation on *every propagation round*, not just every decision. This
is not broken out separately in the evidence log's counters (no
"fixpoint rounds" column), so it is marked **not separately measured**
here; it scales with `propagate` call count (≥ 457, one at minimum per
decision, plus however many rounds each fixpoint takes before matching
fingerprints — likely the actual dominant allocation source given it is
O(state) and happens more often than decisions, but the current
instrumentation doesn't expose the round count to confirm this
quantitatively).

**Functional formulation**: this entire structure disappears under
persistent collections with structural/hash equality. If `State`'s
fields are hash-consed persistent maps (vix's canonical-Map discipline,
V12/V13), then "did propagation change anything" is `new_state == 
old_state` on the *root hashes*, O(1) — not O(|state|) clone-then-compare.
This is a pure win with no reuse-analysis dependency at all; it falls
out of switching representations. Recommend flagging fixpoint-round
count as a new perf-diagnostics counter in rodin-core *before* the vix
port, purely to get a real number here instead of "not separately
measured."

## 2. Summary ledger table

| Structure | Rust location | Access pattern | Persistent shape | Lowering class |
|---|---|---|---|---|
| `State` | lib.rs:756 | clone-per-branch (not in-place mutation) | persistent map/set/vec, structural sharing | shared across siblings by construction; in-place reuse only within one propagate fixpoint chain |
| `Solver` (learned/activity/counters) | lib.rs:779 | `&mut self`, single global owner per `solve()` | append-only vec + derived maps + persistent scalar map | uniquely owned for the whole solve call → provable in-place reuse |
| `learned_by_package`/`learned_without_packages` | lib.rs:787-788 | full rebuild on subsumption event | derived incremental index (query over `learned`) | machine-maintained, not solver-maintained |
| `DerivationStore`/`ClauseStore`/`ExclusionStore` | rodin-facts/src/{derivation,clause,exclusion}.rs | append-only `Vec`, index-stable ids | **already** persistent arena | direct 1:1 port to a content-addressed `Store<T>` |
| `VersionSet`/`Region` | rodin-facts/src/{version_set,region}.rs | **zero mutation, already pure** | value kind, no reformulation needed | value-kind mapping only (§4) |
| `StateFingerprint` | lib.rs:3903 | recomputed+compared every fixpoint round | eliminated — replaced by hash/structural equality on `State` itself | falls out of the `State` port for free |

## 3. Granularity: flesh-not-spine, confirmed against the actual loop shape

`solve()` (lib.rs:1859) is the single seam: premises (a `Problem` +
`CompiledIndex`, both built from traced `versions_of`/`deps_of` facts)
in, a `SolveResult` (selected versions + `LearnedNoGood`s + derivations)
out. Everything between — `search`'s recursive descent, `propagate`'s
fixpoint loop, `learn`'s region-widening — is **ordinary function calls
over a value already inside one demand node's scope**. Confirmed
concretely: `search` calls itself recursively (lib.rs:2167,
`self.search(branch)`) with no `await`/demand point inside the loop; the
fixpoint `loop` in `propagate` iterates `self.index.compiled` (a fixed
slice known at solve-entry) with no external demand either. This matches
V4 exactly ("nothing forces locally... demand backpropagates from
selected outputs") applied at the *whole-solve* granularity rather than
per-propagation-step: the 13,331 learned-fact scans and however many
fixpoint rounds `propagate` takes on this workload are weavy-JIT'd native
calls inside one node, never individually traced. This is the "flesh"
half of the position doc's claim, and it holds without qualification —
nothing found while reading the loop bodies requires a demand point
mid-search.

### What crosses the seam, and in what representation

- **In**: `Problem` (lib.rs:336, a `Vec<RootReq>` — root package/version-req
  pairs, `Decree`s), `CompiledIndex` (lib.rs:666 — packages, versions,
  compiled clauses; itself built from `versions_of`/`deps_of` observed
  facts, i.e. from pinned vix observations per V14–V17).
- **Out**: `SolveResult` (lib.rs:364 — `FinalDomain` per package, `Vec<
  LearnedNoGood>`, the `DerivationStore`/`ClauseStore` needed to replay
  proofs).
- **The value kind that's missing**: `VersionSet` (rodin-facts/src/
  version_set.rs:34, `Vec<Interval>` of `Option<Version>` bounds) and
  `Region` (region.rs:14, `BTreeMap<PkgIx, VersionSet>` + feature
  polarity set) are **not** currently expressible as vix values.
  `Version` itself already is (`vix/src/machine/version.rs` in the facet
  repo — parse/canonicalize/total-order, wired into the machine's value
  representation and `RenderedValue` — confirmed present as of this
  writing). `VersionSet`/`Region` are not: they'd need (a) a value kind
  wrapping `Vec<Interval>` with the existing `Version` value as bound
  type, canonical-order-normalized on construction exactly as
  `VersionSet::from_intervals` already does (version_set.rs:69–89, sort +
  merge-touching-intervals — this normalization is precisely what makes
  two `VersionSet`s content-hash-equal iff semantically equal, which is
  the V12/V13 discipline vix already demands of every value kind); (b) a
  `Region` composite value (map from `PkgIx`-equivalent package identity
  to `VersionSet`, plus a feature-polarity set) with the same
  canonicalization requirement. Both are finite, hashable, immutable by
  construction in the Rust code today — the only missing piece is
  registering them as vix value kinds with canonical-byte-encoding and
  total-order rules (mirroring `version.rs`'s `canonical_bytes`/
  `cmp_total`), not a semantic redesign.

## 4. VersionSet/Region as vix value kinds — the spec gap named precisely

To make §3's "in what representation" concrete, here is the minimum
value-kind surface needed, keeping to what the Rust code already proves
is sufficient (no speculative generalization):

- **`Interval` value**: `{ lower: Option<Version>, upper: Option<Version> }`,
  half-open `[lower, upper)`, `None` = unbounded. Total order: by
  `(lower, upper)` lexicographically with `None` sorting as documented in
  `compare_lower` (version_set.rs:266–273).
- **`VersionSet` value**: a canonicalized, non-overlapping, sorted `Array<
  Interval>` — canonicalization is exactly `from_intervals`'s
  sort-then-merge-touching-or-overlapping pass (version_set.rs:69–89).
  This is the load-bearing invariant: without it, `intersect`/`union`
  aren't confluent under hashing (two constructions of the "same" set
  could produce different interval orderings and hash unequal — the code
  avoids this today by *always* routing construction through
  `from_intervals`; a vix value kind must make that routing mandatory,
  i.e. `VersionSet` should not be publicly constructible except through
  the canonicalizing constructor, exactly as the Rust type already
  enforces via module privacy of the `intervals` field).
- **`Region` value**: `Map<PkgIx-equivalent, VersionSet>` (vix's existing
  canonical `Map` — sorted pairs, last-wins, history-independent hash,
  per machine-lowering.md:287 — already satisfies the ordering
  requirement) plus a `Set<(pkg, feat, bool)>` for feature polarity.
  Package identity itself needs a stable, hashable, totally-ordered vix
  value — today `PkgIx` is an interned index into `CompiledIndex`
  (lib.rs, `PackageInterner`); porting this means either (a) making
  `CompiledIndex`'s interning tables themselves vix values the kernel
  closes over, or (b) using package *names* (already a vix string) as
  the map key and dropping the interning layer inside the kernel
  entirely, trading an index-comparison for a string-comparison. (b) is
  simpler and consistent with V13 (identity = canonicalized parse); the
  index layer in Rust exists for cache-locality reasons that may not
  transfer to the vix representation anyway — flagged as an open
  question, not resolved here, since it's an engineering tradeoff
  internal to the port rather than a semantics question.

## 5. The honest cost model

Baseline unit: **one propagation step** = one pass through the
`propagate` fixpoint body applying one `intersect_domain` or
`activate` write (the smallest unit that mutates in current Rust).

| | Rust today (in-place `&mut`) | Naive functional (persistent collections, no reuse analysis) | Persistent + reuse analysis |
|---|---|---|---|
| `State` per decision | O(1) amortized field write; the `state.clone()` at branch time is O(\|state\|) (already, in Rust, not vix's fault) | same O(\|state\|) clone cost as Rust already pays at branch points, **plus** O(log n) allocation per intermediate persistent-map write instead of O(1) `BTreeMap` in-place insert, for every `intersect_domain`/`activate` call between branch points | O(log n) collapses to O(1) amortized for the linear chain of writes within one branch (each write's input structurally proven dead) — matches Rust's `&mut` cost exactly for the single-owner segment; the branch-boundary clone stays O(1) root-copy instead of O(\|state\|), which is *cheaper* than Rust today |
| `Solver.learned` push | O(1) amortized `Vec::push` | O(1) amortized persistent-vector push (structural sharing, no full copy — this is standard even without reuse analysis, since it's append-only) | same — no reuse-analysis dependency, append-only stores are free under persistence already |
| `Solver.activity` decay | O(\|packages\|) full-map scan, in place | O(\|packages\| log \|packages\|) rebuild (persistent map, no in-place mutation) | O(\|packages\|) amortized (uniquely-owned `Solver`, proven single-reference — matches Rust cost) |
| Fixpoint quiescence check (`StateFingerprint`) | O(\|state\|) clone + compare, every round | O(1) — root hash/pointer equality on the persistent `State`, no clone needed at all | same as naive-functional column — this row is a pure win independent of reuse analysis |
| Subsumption scan (`install_learned_fact`) | O(active learned) linear scan, in place | same O(active learned) scan cost — the algorithm doesn't change, only the store's element type does | same |

**Order-of-magnitude summary**: for the structures that dominate op
count on this workload (`State` mutation inside `propagate`, ~13,331
learned-scans' worth of `intersect_domain` calls plus however many
fixpoint rounds), naive-functional costs an extra **O(log n) factor per
write** relative to Rust's O(1) in-place `BTreeMap` mutation — call it a
constant few-x slowdown per write, not an order of magnitude, since `n`
here is package/feature count (hundreds, not millions) and `log n` is
single digits. Reuse analysis recovers that factor entirely for the
uniquely-owned `Solver` fields and the linear-chain portion of `State`
mutation within one branch; it buys **nothing** for the branch-point
clone (which persistence already made O(1) instead of Rust's current
O(\|state\|) — a win vix gets for free that Rust doesn't have today) and
**nothing** for the append-only stores (already free). Net honest
estimate for the *kernel alone*, cold, no fact transport: **low
single-digit-x slower than rodin-core** for the dominant write-heavy
paths under reuse analysis, potentially **faster** than rodin-core on
the branch-clone path specifically (persistent O(1) vs Rust's current
O(\|state\|) full clone) — this second claim is falsifiable and should
be the first thing Spike B measures, since it inverts the usual
"functional is slower" prior for this specific kernel.

### The reframed bar, computed

Position doc: "fact transport makes routine edits memo hits... kernel
runs only on novel conflicts." From rodin-churn-taxonomy.md:25,39: of
1,051 real block-breaking churns, 708 are req-narrowed + 125 required-add
≈ 833/1,051 ≈ **79%** monotone-safe under entailment transport (subject
to the closed-world audit in adjudication.md — negative-feature-literal
supports are excluded from this count until audited kind-by-kind). If
79% of edits become memo hits (zero kernel invocations) and the
remaining 21% pay the *full* kernel cost, the break-even slowdown factor
`k` for the pure kernel to match today's Rust cost **averaged over an
edit session** solves:

```
0.79 × (memo-hit cost ≈ 0) + 0.21 × k × (rodin-core cold-solve cost)
  ≤ 1.0 × (rodin-core cold-solve cost)   [today: every edit is a cold solve]

⇒ k ≤ 1 / 0.21 ≈ 4.76
```

So a pure kernel up to **~4.8x slower** than rodin-core, *purely from the
21% novel-conflict share*, breaks even on this taxonomy's edit mix — before
counting the 7.29x edge-locality multiplier (rodin-churn-taxonomy.md:53,
"1080 blocks... 1057 blocks... 1.022" fixed-target vs the per-edge
7.29x figure) which shrinks the *cost* of each of those remaining 21%
solves too (smaller invalidated read-set → fewer facts re-derived per
solve, not just fewer solves). The position doc's "5-10x slower... wins
on every real workload" claim (rodin-in-vix.md:52) is therefore
**conservative relative to this specific taxonomy's numbers** — 4.8x is
the bound from cache-hit-rate alone, with the edge-locality multiplier as
unquantified additional headroom on top (it changes per-solve cost, not
solve *count*, so it doesn't fold into the same ratio without knowing the
per-solve cost distribution — flagged as the next number to actually
measure, not computed here since rodin-c-evidence.md doesn't break out
per-solve cost by taxonomy category).

## 6. Vix pseudocode: propagate + learn (as rodin-core actually implements them)

Named accurately per §0 — this is domain-narrowing propagation with
region-based conflict learning, **not** literal 2-watched-literal unit
propagation with 1st-UIP resolution. Vix surface gaps are flagged inline
with `-- GAP:`.

```
-- One demand node's worth of "flesh": ordinary recursive functions,
-- no traced sub-calls. Only `solve` itself is a node boundary (§3).

fn solve(problem: Problem, index: CompiledIndex) -> SolveResult =
    let state0 = seed_problem(problem, index)   -- pure, returns State or fails
    match search(index, empty_solver(index, problem.kind), state0) with
    | Ok(state, solver) -> finish(solver, state)
    | Err(failure, solver) -> no_solution(solver, failure)

-- search: recursive descent, NOT loop-identity territory (§ below) —
-- this is a plain algebraic recursion over an explicit candidate list,
-- structurally a fold-with-early-return over `candidates`.
fn search(index: CompiledIndex, solver: Solver, state: State)
    -> Result<(State, Solver), (SearchFailure, Solver)> =
  match propagate(index, solver, state) with
  | Err(conflict, solver') ->
      let solver'' = learn(index, solver', conflict)
      conflict_failure(index, solver'', state)          -- always Backtrack or Restart
  | Ok(state') ->
      match next_package(index, solver, state') with
      | None -> Ok(assert_solution(index, state'), solver)   -- solution
      | Some(pkg) ->
          let candidates = candidates_of(index, state', pkg)
          if candidates.is_empty() then
              let solver' = learn(index, solver, conflict_from_empty(pkg, state'))
              conflict_failure(index, solver', state')
          else
              try_candidates(index, solver, state', pkg, reverse(candidates))

-- try_candidates: this IS a loop-identity-relevant fold — each element
-- (a candidate Version) spawns a recursive search call; the FIRST
-- success short-circuits, later candidates are never touched (this
-- is decision-order, not collection identity: no react-key problem,
-- because candidates aren't collected/joined, they're tried in strict
-- preference order with early exit).
-- GAP: vix's ruled loop-identity rules (machine-lowering.md:308-331)
-- are about COLLECTING results with content-keyed sharing (duplicates
-- join, position explicit). Early-exit-on-first-success over an
-- ORDERED list is a different primitive: a short-circuiting fold, not
-- a collect. Does vix have a first-class "try in order, stop at first
-- Ok" combinator, or does this lower to a hand-written recursive
-- helper? If the latter (likely, since it's just recursion — see next
-- line), no new machinery is needed, but it should be named so it's
-- not mistaken for the loop-identity case.
fn try_candidates(index, solver, state, pkg, candidates) =
  match candidates with
  | [] -> Err(Backtrack, solver)
  | version :: rest ->
      let branch = with_hypothesis(state, pkg, version)   -- persistent "clone": O(1) root copy
      match search(index, solver, branch) with
      | Ok(result) -> Ok(result)
      | Err(Restart, solver') -> Err(Restart, solver')
      | Err(Backtrack, solver') -> try_candidates(index, solver', state, pkg, rest)

-- propagate: fixpoint recursion, replacing the `loop { ...; if
-- fingerprint == before break }` shape (lib.rs:2176-2200).
-- GAP: this is exactly the shape V4 disallows for LOCAL forcing but
-- explicitly allows for flesh-internal recursion (no demand point
-- crosses it) — confirmed fine per §3. The gap is mechanical: does the
-- vix surface have a "recurse with structural-equality base case"
-- idiom, or does it need writing as generic explicit recursion with a
-- passed-in previous-value argument? The latter is what's pseudocoded
-- below; it requires no new construct, just confirms `State` values
-- support cheap equality (root-hash compare under persistence, §1.5)
-- rather than the deep structural walk `state_fingerprint` does today.
fn propagate(index, solver, state) -> Result<State, (Conflict, Solver)> =
  match apply_exclusions(index, solver, state) with
  | Err(c) -> Err(c, solver)
  | Ok(state1) ->
    match force_singletons(index, state1) with
    | Err(c) -> Err(c, solver)
    | Ok(state2) ->
      match fold_clauses(index, state2, index.compiled) with   -- see below
      | Err(c) -> Err(c, solver)
      | Ok(state3) ->
        match check_feature_exclusions(state3) with
        | Err(c) -> Err(c, solver)
        | Ok(_) ->
            if state3 == state then Ok(state3)          -- O(1) under persistence, was O(|state|)
            else propagate(index, solver, state3)

-- fold_clauses: a genuine fold-with-early-exit over a fixed Array
-- (index.compiled, known at solve-entry, never mutated) — the
-- clearest "ordinary weavy-compiled flesh" loop in the kernel. No
-- loop-identity concern (not collecting/keying results, just
-- accumulating one State value through a Result-short-circuiting
-- reduction).
-- GAP: Rust expresses this with `?` inside a `for` loop over a
-- borrowed slice. Vix's early-return shape for a fold that must abort
-- the WHOLE traversal (not just skip one element) on first Err needs
-- naming precisely: is there sugar equivalent to `?`/try-fold, or does
-- every such site lower to explicit recursion (as pseudocoded)? If
-- it's always explicit recursion, that's fine — it's just verbose, not
-- blocked — but 4+ call sites in this kernel alone (apply_exclusions,
-- fold_clauses, force_singletons, check_feature_exclusions) want the
-- same shape, so a `try_fold`-equivalent combinator is worth having
-- rather than hand-rolling four times.
fn fold_clauses(index, state, clauses) -> Result<State, Conflict> =
  match clauses with
  | [] -> Ok(state)
  | clause :: rest ->
      if state.applied_clauses.contains(clause.id) then
          fold_clauses(index, state, rest)
      else if all(guard_true(state, g) for g in clause.antecedents) then   -- lazy && per V4/A1
          match apply_consequent(index, state.with_applied(clause.id), clause.consequent) with
          | Err(c) -> Err(c)
          | Ok(state') -> fold_clauses(index, state', rest)
      else
          fold_clauses(index, state, rest)

-- learn: NOT 1st-UIP resolution on an implication graph — the actual
-- algorithm is support collection + region widening (lib.rs:2578-2725).
-- Pseudocoded faithfully, correcting the task brief's "first-UIP"
-- framing to what the code does.
fn learn(index, solver, conflict) -> Solver =
  let solver1 = bump_activity(solver, conflict)     -- full-map decay, §1.2.1 — honest cost, no lazy trick
  let premises = dedup(conflict.premises ++ conflict.decision.map(Hypothesis))
  let (solver2, conflict_deriv) = append_derivation(solver1, Conflict, premises)
  assert replay(solver2.derivations, conflict_deriv) == Ok(_)   -- V16-style: proof must replay before install
  let support = support_from_premises(index, premises)
  let point_region = region_from_selected_support(index, conflict.selected, support)
  let (region, point_n, widened_n) = index.widen_region(conflict.selected, support, point_region)
  let (solver3, derivation, witness) =
      if widened_n > point_n then
          append_widen_derivation(solver2, conflict_deriv, index, conflict.selected, support, point_region, region)
      else (solver2, conflict_deriv, None)
  let fact = LearnedNoGood { region, derivation, support, point_n, widened_n, witness, active: true, ... }
  install_learned_fact(solver3, fact)

-- install_learned_fact: subset-scan subsumption, NOT hash-dedupe
-- (§1.3's precise correction to the position doc). A pure function
-- returning a new Solver with `learned` extended/mutated-by-flag.
fn install_learned_fact(solver, fact) -> Solver =
  if any(existing.active && existing.region.contains_region(fact.region) for existing in solver.learned) then
      solver   -- suppressed, no-op
  else
      let solver1 = deactivate_all(solver, |existing| existing.active
                                            && existing.status == LocallyLearned
                                            && fact.region.contains_region(existing.region))
      let exclusion = append_exclusion(solver1, fact.region, fact.derivation)
      push_learned_fact(solver1, fact.with_exclusion(exclusion))   -- append-only, O(1) amortized under persistence
```

### Gaps found, ranked by how much they'd actually block a transcription

1. **Try-fold / early-abort-fold combinator** (fold_clauses,
   apply_exclusions, force_singletons, check_feature_exclusions all want
   it) — not blocking, since explicit recursion works, but four
   hand-rolled instances in one kernel is a signal the surface wants
   this as sugar. Lowest-risk gap: purely a combinator library question,
   no semantics decision needed.
2. **Cheap equality on large persistent values for fixpoint detection**
   — not a vix gap at all, actually; it's a consequence of choosing
   hash-consed/canonical persistent collections for `State`'s fields
   (which V12/V13 already mandate for every vix value). Listed here only
   to flag that `propagate`'s termination check is the concrete
   mechanism that would silently regress to `state_fingerprint`'s O(n)
   cost if the port used non-canonical/non-hash-consed collections.
3. **Short-circuit-first-success over an explicitly ordered list**
   (try_candidates) — confirmed NOT a loop-identity case (§ inline gap
   note above); flagged only to prevent a future spike from
   mis-applying the collect/join loop-identity rules (machine-lowering.md
   §"Loop identity RULED") to a construct they don't govern.
4. **`Region`/`VersionSet` as vix value kinds** (§4) — the one gap that
   is genuinely new language-design work, not a missing combinator.
   Everything else in this pseudocode already type-checks against
   existing vix values (`Version`, `Map`, `Set`, `Array`) once these two
   are added.

No gap found required inventing a new control-flow primitive beyond
what's already ruled (match/if laziness, recursion, demand-free flesh).
The kernel's algorithm, as actually written in Rust — not the classical
CDCL textbook version — maps onto vix's existing surface with the four
items above as the complete gap list.
