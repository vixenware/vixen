# Research prompt: fact validity beyond "the read-set changed at all"

*Self-contained prompt for external deep-research agents (paste whole). You have
no access to this repository — everything you need is embedded below. House
style: claims must be falsifiable and cite checkable sources; no grandiosity,
no "revolutionary," no marketing language. If you can't find prior art for a
direction, say so plainly rather than padding. Where you're not sure, say
"unverified" and hedge — do not confabulate citations (a prior internal scrub
caught one adversarial reviewer inventing a nonexistent "Vesta manual" quote;
don't be that reviewer).*

## 0. What this is for

Rodin is a from-scratch Cargo-ecosystem dependency resolver built as a CDCL
(conflict-driven clause learning) SAT-style solver over *version regions*
rather than single version assignments. It is meant to eventually replace
PubGrub inside a real build tool. Its distinguishing bet (already prototyped
and measured against real crates.io data, evidence below) is that a learned
conflict fact ("these version regions of these packages are jointly
unsatisfiable") should be **widened** — proven valid across a *range* of
consecutive package versions, not just the one version pair where it was
derived — so that re-solving after a routine dependency bump doesn't have to
re-derive facts from scratch.

The mechanism as built draws a fact's validity boundary at: *the fact stays
valid across consecutive versions of its supporting packages for as long as
the observed metadata "read-set" those versions expose is byte-identical*.
The moment ANY field in that read-set changes — even a field the derivation
never actually used — the fact's validity range ends and must be re-derived
from scratch at that version boundary.

We have now measured, on real crates.io metadata, that this boundary is far
more conservative than it needs to be: most metadata changes are provably
*safe* for a large class of previously-learned facts (formal argument below),
yet the current mechanism throws them away anyway. Quantifying exactly how
much validity range is being discarded, and finding sound techniques to keep
it, is the question this prompt is for.

## 1. The mechanism as built

### 1.1 The algebra

- **`VersionSet`**: a normalized union of disjoint, non-touching half-open
  `Interval { lower: Option<Version>, upper: Option<Version> }` ranges over
  semver `Version`. Supports `intersect`, `union`, `complement` (De Morgan
  laws hold, tested), `contains`, construction `from_req` (parses a
  `semver::VersionReq` into the equivalent interval union) and
  `from_intervals` (point-set unions, used to build a set from a list of
  known-good versions).
- **`Region`**: `{ versions: Map<PkgIx, VersionSet>, features: Set<(PkgIx,
  FeatIx, bool)> }` — a hyper-rectangle in per-package version space, plus
  feature literals. `Region::intersect` conjoins per-package `VersionSet`s and
  returns `None` if any package's set becomes empty (region is provably
  unreachable).
- **Facts are learned no-goods**: a `Region` (or a boolean disjunction reduced
  to one, in the CDCL sense) that the solver has proven cannot be part of any
  solution, plus a `support` set (which requirement edges/clauses were
  actually consulted to derive it) and a `derivation` (`Premise::{Clause,
  Exclusion, Pin, Hypothesis, Derived}` chained into a replayable proof DAG —
  every learned fact must replay from its premises or it's rejected).
- **Widening** is the operation that takes a point fact (proven for one exact
  parent-package version) and produces a **`WidenCert`**:
  ```rust
  struct WidenCert {
      package: PkgIx,
      checked: VersionSet,       // the range the widening claims to cover
      evidence: WidenEvidence,
  }
  enum WidenEvidence {
      DeclaredStructure { manifest_hash: String },
      ObservedReadSet { read_set_digest: String, snapshot_id: String },
  }
  ```
  Two widening strategies exist today:
  1. **Declared-structure widening**: the fact is re-checked to still hold as
     long as the FULL declared manifest signature (every dependency row's
     name/req/features/optional/default_features/target/kind, plus merged
     `features`/`features2`) is byte-identical across the version range. This
     is the strict, whole-row equality check.
  2. **`R2` observed-read-set widening**: instead of the full declared
     signature, only the fields the derivation's support set actually
     *touched* (`actual`, `req`, `kind`, `optional` per edge — the "read-set")
     must be identical across the range. This is already strictly finer than
     (1) — it ignores fields the derivation never read — and is proven, on a
     committed real-crates.io fixture, to widen further than declared-
     structure widening on the same data (`rodin-core`'s Round 10
     demonstration). It is the current best mechanism, and it is what this
     prompt is trying to improve on.
- Both strategies still operate on the **whole row** at a given package
  version: a version boundary either preserves the read-set-relevant state of
  ALL edges, or the fact's validity range ends there — even if only one edge
  (dependency) actually changed and the rest of the row's read-set fields are
  byte-identical to what the derivation consumed.

### 1.2 What "the read-set changed at all" costs, quantified

We ran a field-by-field taxonomy of every REAL (non-artifact, non-cosmetic)
block-break in six real crates.io dependency histories — clap, openssl,
clippy, hyper, serde, async-graphql (1,743 non-yanked versions, 1,051 REAL
breaks total; see §2 for the full data) — classifying exactly what changed at
each break and how many distinct dependency edges it touched. The headline
number:

**Aggregate "fineness" ratio = 7.29x** (range 1.17x–12.20x per crate). This is
`(average consecutive versions a single edge's own state holds steady) /
(average consecutive versions the WHOLE manifest row holds steady)`. In
other words: if Rodin tracked validity **per dependency edge** instead of
**per whole manifest row**, a fact keyed to one specific edge would on
average stay valid across ~7x more consecutive versions than the current
row-level mechanism allows — because 66.7% of REAL breaks (aggregate; range
37.5%–100.0% per crate) touch exactly ONE dependency edge, and the other
edges' state is completely unaffected by that break.

Separately, narrowing the *field set* consumed (the existing R2 read-set
mechanism) barely helps on its own: the ratio of whole-row blocks to R2's
narrowed-field blocks is only ~1.02x aggregate — almost every version bump
touches *some* edge's req, so restricting which fields you look at, while
still comparing the WHOLE row, buys almost nothing. The win identified here
is orthogonal: it's about restricting *which rows* (which package-version
pairs) a fact's validity boundary depends on, not which fields within a row.

## 2. The taxonomy data (embedded — you have no repo access)

### 2.1 Per-crate REAL-break field classification

A REAL break = block boundary that survives BOTH the fixed-target runtime
projection (drop non-workspace dev deps, drop target-false deps for
`aarch64-apple-darwin`) AND the R2 observed-read-set narrowing — i.e. it is
never a projection artifact or a read-set-narrowing candidate; the resolver
genuinely must treat the boundary as real. Requirement-string changes are
classified by sampling: build a probe set of `Version`s from every numeric
token in either requirement string (± 1 at each semver component, plus
`0.0.0`/`9999.0.0` sentinels), then compare which probes each requirement
string's `VersionReq::matches` accepts.

- **req-widened**: new requirement accepts everything old accepted, plus more
  (strict superset).
- **req-narrowed**: new requirement accepts a strict subset of what old
  accepted.
- **req-bumped-incompatibly**: neither is a subset of the other (some probes
  flip both ways) — e.g. `^1 -> ^2`.
- **req-reshaped**: syntactically different strings, identical accepted set
  (e.g. `^1.4.0` rewritten as `>=1.4.0, <2.0.0`).

| crate | non-yanked versions | REAL breaks | req-widened | req-narrowed | req-incompatible-bump | req-reshaped | dep-added (required) | dep-added (optional) | dep-removed (required) | dep-removed (optional) | feature-map co-occurring |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| async-graphql | 440 | 347 | 5 | 461 | 218 | 158 | 42 | 30 | 28 | 14 | 42 |
| clap | 364 | 194 | 4 | 17 | 195 | 21 | 10 | 18 | 9 | 17 | 30 |
| clippy | 210 | 151 | 0 | 3 | 145 | 0 | 15 | 1 | 14 | 1 | 4 |
| hyper | 247 | 96 | 14 | 63 | 40 | 24 | 45 | 18 | 38 | 15 | 34 |
| openssl | 170 | 156 | 2 | 161 | 18 | 0 | 10 | 0 | 3 | 1 | 17 |
| serde | 312 | 107 | 3 | 3 | 93 | 0 | 3 | 2 | 2 | 1 | 7 |
| **total** | **1743** | **1051** | **28** | **708** | **709** | **203** | **125** | **69** | **94** | **49** | **134** |

(Counts are per REQUIREMENT-CHANGE INSTANCE or per DEP-ADD/REMOVE INSTANCE,
not per break — a single break can touch several edges at once, so rows sum
to more than the break count.)

Representative quoted examples (5 per kind, verbatim from the data):

**req-widened:**
- `async-graphql 3.0.0-alpha.3 -> 3.0.0: deps[async-graphql-derive].req: =3.0.0-alpha.3 -> =3.0.0; deps[async-graphql-parser].req: =3.0.0-alpha.3 -> =3.0.0; deps[async-graphql-value].req: =3.0.0-alpha.3 -> =3.0.0`
- `async-graphql 3.0.15 -> 3.0.16: deps[futures-util].req: ^0.3.18 -> ^0.3.0`
- `async-graphql 7.0.1 -> 7.0.2: deps[base64].req: ^0.13.0 -> ^0.21.0; deps[bigdecimal].req: ^0.3.0 -> >=0.3.0, <0.5.0` (+ 3 lockstep sibling req bumps)
- `clap 2.34.0 -> 3.0.0-beta.1: deps[atty].req: ^0.2.2 -> ^0.2; deps[bitflags].req: ^1.0 -> ^1.2; …` (7-edge widening + 6 dep add/removes, a major rewrite)
- `clap 3.0.0-rc.13 -> 3.0.0: deps[clap_derive].req: =3.0.0-rc.13 -> ^3.0.0` (RC exact-pin relaxing to a caret range on final release)

**req-narrowed:**
- `async-graphql 0.8.0 -> 0.8.1: deps[async-graphql-derive].req: ^0.8.0 -> ^0.8.1`
- `async-graphql 0.9.0 -> 0.9.1: deps[async-graphql-derive].req: ^0.9.0 -> ^0.9.1`
- `async-graphql 0.9.1 -> 0.9.2: deps[async-graphql-derive].req: ^0.9.1 -> ^0.9.2`
- `async-graphql 0.9.2 -> 0.9.3: deps[async-graphql-derive].req: ^0.9.2 -> ^0.9.3`
- `async-graphql 0.9.3 -> 0.9.4: deps[async-graphql-derive].req: ^0.9.3 -> ^0.9.4`

  (417 occurrences aggregate — every one of these is `^0.x.y -> ^0.x.(y+1)`,
  which under Cargo caret semantics for a `0.x` series is `>=0.x.y,<0.(x+1).0
  -> >=0.x.(y+1),<0.(x+1).0`: the ceiling is unchanged, only the floor rises.
  This is the single most common "real churn" pattern in the dataset and it
  is a NARROWING, not the vague "routine bump" it looks like.)

**req-bumped-incompatibly:**
- `async-graphql 0.5.0 -> 0.6.0: deps[async-graphql-derive].req: ^0.5.0 -> ^0.6.0`
- `async-graphql 0.6.0 -> 0.7.0: deps[async-graphql-derive].req: ^0.6.0 -> ^0.7.0`
- `async-graphql 0.7.0 -> 0.8.0: deps[async-graphql-derive].req: ^0.7.0 -> ^0.8.0`
- `async-graphql 0.8.1 -> 0.9.0: deps[async-graphql-derive].req: ^0.8.1 -> ^0.9.0`
- `async-graphql 0.9.4 -> 0.10.0: deps[async-graphql-derive].req: ^0.9.4 -> ^0.10.0`

  (553 occurrences aggregate — every `^0.x.y -> ^0.(x+1).0` minor bump: floor
  AND ceiling both shift, old and new accepted ranges become disjoint at the
  boundary. Neither superset nor subset of the other.)

**req-reshaped (syntax changed, accepted set didn't):**
- `async-graphql 2.0.0-alpha.0 -> 2.0.0-alpha.1: deps[async-graphql-derive].req: ^2.0.0-alpha.0 -> ^2.0.0-alpha.1; …`
- `openssl 0.7.6 -> 0.7.7: deps[bitflags].req: >= 0.2, < 0.4 -> ^0.4` (explicit bounds rewritten as caret, same set)
- 69 occurrences aggregate, mostly caret ⟷ explicit-bounds rewrites and
  prerelease-component caret bumps (`^X.Y.Z-alpha.N -> ^X.Y.Z-alpha.N+1` where
  the caret rule for a full prerelease triple degenerates to exact-match
  semantics either way).

**dep-added / dep-removed (194 add-instances / 143 remove-instances total):**
- `async-graphql 1.10.12 -> 1.11.0: deps[async-graphql-parser]: added; deps[graphql-parser]: removed` — a same-version RENAME disguised as simultaneous add+remove. Real break (any fact keyed to the old edge name is genuinely gone), not an artifact.
- `async-graphql 1.5.0 -> 1.5.1: deps[futures]: added; deps[itoa]: removed; deps[ryu]: removed`
- `openssl 0.7.14 -> 0.8.0: deps[gcc].optional: false -> true; deps[openssl-sys-extras]: removed`
- `clap 2.28.0's sibling: deps[clap_derive]: added (optional); deps[indexmap]: added; …` (crate splitting itself into a derive-macro companion, several edges at once)
- Most optional dep-adds coincide with a features/features2 map change (134
  co-occurring instances aggregate) — the crate is wiring a brand-new
  optional integration behind a brand-new feature flag in the same release.

### 2.2 Edge locality — how much of a whole-row block is "wasted"

| crate | single-edge-break ratio | avg distinct edges touched per break | edge versions/block | whole-row versions/block | fineness (edge/whole-row) |
|---|---:|---:|---:|---:|---:|
| async-graphql | 0.375 | 2.75 | 15.39 | 1.26 | 12.20 |
| clap | 0.758 | 1.50 | 8.81 | 1.82 | 4.84 |
| clippy | 0.914 | 1.19 | 3.26 | 1.36 | 2.39 |
| hyper | 0.573 | 2.69 | 16.55 | 2.47 | 6.70 |
| openssl | 0.795 | 1.25 | 4.23 | 1.08 | 3.93 |
| serde | 1.000 | 1.00 | 3.08 | 2.62 | 1.17 |
| **aggregate** | **0.667** | **1.89** | **11.76** | **1.61** | **7.29** |

`single-edge-break ratio` = fraction of REAL breaks that touch exactly one
dependency name (add/remove/req-change; a features-map-only co-occurrence
doesn't count as touching an edge). `edge versions/block` = averaged over
every dependency edge that ever appears, how many consecutive versions that
ONE edge's own (req, features, optional, default_features, target) state
holds steady before it changes. `whole-row versions/block` = non-yanked
versions ÷ whole-manifest blocks (the existing forensics-pass number).
`fineness` = their ratio — directly answers "how much finer is per-edge
stability than whole-row" for this dataset. This is consistent in direction
(though this taxonomy's crates are a 6-crate, worst-and-typical-case sample,
not the full 400-crate survey) with an independent internal whole-ecosystem
measurement (400 crates, 15,309 versions) that found per-edge stability 13.4x
finer than whole-manifest, median 11 versions/edge-block, 77.3% of edges
≤5 blocks — same phenomenon, different sample, same order of magnitude.

### 2.3 async-graphql: the worst case, explained

async-graphql was the one crate the prior forensics pass couldn't classify
(missing cached row). Fetched fresh from `index.crates.io` (441 JSONL rows,
now committed as a repo fixture), it resolves to 347 REAL breaks / 440
non-yanked versions — confirming "350 blocks, worst case." Two drivers, both
visible in the numbers above:

1. It is split into sibling crates (`async-graphql-derive`,
   `async-graphql-parser`, `async-graphql-value`) that get version-pinned
   from the main crate and bumped in lockstep on every release, whether or
   not anything relevant changed — hence its outlier avg-edges/break (2.75)
   and lowest single-edge ratio (0.375) in the dataset.
2. It continuously accumulates and drops OPTIONAL integrations with the
   wider ecosystem (chrono, bson, uuid, url, im, parking_lot, tempfile, …) as
   it adds/removes support for external types, each usually paired with a
   features-map change.

Both are unavoidable churn under ANY whole-row blocking scheme, including the
current R2 field-narrowing: one lockstep sibling bump or one new optional
integration anywhere in the manifest re-shatters the block for every OTHER,
completely unrelated edge too.

## 3. A monotonicity argument, already partly worked out (verify or kill it)

Treat a manifest version as a constraint set over the resolver's boolean
search variables (roughly: one implication clause per dependency edge — "if
this crate-version is chosen, this dependency's requirement must hold").
UNSAT-ness is monotone under adding constraints: if constraint set `S` is
unsatisfiable, every superset `S' ⊇ S` is unsatisfiable too (a stronger
formula can't become satisfiable by adding more clauses). That gives a
candidate asymmetry, ALREADY reasoned out internally but NOT verified against
prior art or a formal soundness proof — this is exactly what direction (c)
below is for:

- **Constraint-tightening moves** — dep-added (a wholly new edge) and
  req-narrowed (a strict-subset accepted range on an existing edge) — can, in
  this framing, only ADD constraints relative to the old manifest. A conflict
  fact derivable from the old (looser) manifest should remain derivable from
  the new (stricter or larger) one. In this dataset, that's 708 req-narrowed
  + 125 dep-added-required instances (dep-added-optional needs separate
  treatment — an optional dependency isn't a constraint unless the feature
  that gates it is enabled, so "added but optional and not enabled" may not
  tighten anything at all).
- **Constraint-loosening moves** — req-widened, req-bumped-incompatibly (an
  interval SHIFT, which drops old floor versions but gains new ceiling ones —
  loosening on the new side even though it's simultaneously narrowing on the
  old side), and dep-removed (deletes an edge a fact's derivation may have
  depended on) — can invalidate a previously-derived conflict fact and must
  be treated as unsafe.

If this argument is sound, the current mechanism ("read-set changed at all
⇒ invalidate") is needlessly conservative on the ENTIRE constraint-tightening
side — which, by the counts above, is the majority of REAL churn (708
narrowed + 125 required-dep-added ≈ 79% of the ~1,051 REAL breaks' dominant
field changes, though a single break can mix tightening and loosening moves
on different edges simultaneously, which the current data doesn't yet
separate cleanly break-by-break).

**This argument has NOT been checked against the literature and may have a
hole** — e.g. optional/feature-gated deps, weak (`dep?`) dependencies, and
the fact that Rodin resolves FEATURES as propagated literals rather than
search variables (see §1.1) may break the clean "adding a clause only
tightens" story in ways not accounted for above. Finding the hole (or
confirming there isn't one) is the job.

## 4. The open question

**Today, a fact's validity range ends the instant the read-set changes at
all — even along a single edge irrelevant to the fact's own derivation, and
even when the change is provably constraint-tightening in the sense above.**

What techniques exist, or could be built, for facts that survive
compatibly-changing metadata? For each direction below: find prior art (cite
real, checkable sources — papers, RFCs, production system docs, not
recollections), identify the strongest candidate technique for Rodin's
specific setting (CDCL-over-regions, immutable per-version metadata rows,
proof-carrying learned facts that must replay from premises), state the
soundness conditions under which it's safe, and give a FALSIFIER — a concrete
data pattern from real package metadata that would break the technique if it
exists (you may use the taxonomy data in §2 or invent a plausible pattern;
say which).

**(a) Semver-aware fact lifting.** If a requirement bump doesn't intersect
the version range the fact's conflict actually concerns, the fact should lift
across the bump untouched — e.g. if a learned fact is about
`Y-versions-below-2.0.0` conflicting with something, and a requirement change
elsewhere only adds/narrows within `Y>=2.0.0`, the fact shouldn't care. Is
there a general algebra for "does this requirement edit intersect this
fact's region" that's cheaper than literally re-deriving? Look at semver
range-arithmetic implementations across ecosystems (npm's `semver` package
`intersects`/`subset`, Python's `packaging.specifiers`, `pubgrub-rs`'s
`Range` type) for prior art on requirement-interval algebra, and whether any
resolver actually reuses that algebra to SKIP re-derivation rather than just
to compute intersections during search.

**(b) Per-edge validity instead of per-row.** The §2.2 data suggests the
biggest available win is tracking a fact's validity range against only the
specific edges its `support` set actually mentions, not the whole manifest
row. What's the right data structure and update-propagation cost model for
this — e.g. is this equivalent to (or should it borrow from) incremental
dataflow/differential-dataflow "if this specific input didn't change, don't
recompute this specific derived fact" tracking? Is per-edge fact indexing
(facts stored keyed by which edges they read, not which manifest rows) a
known technique in incremental build systems or datalog engines
(differential datalog / DDlog, Datomic, incremental Datalog materialization)?
What's the failure mode if a fact's support set is UNDER-recorded (too narrow
— the classic soundness bug for this class of technique)?

**(c) Monotonicity / one-sided-change arguments.** Verify or kill §3's
dep-added/dep-removed and req-narrowed/req-widened asymmetry. Is "adding a
clause to a CNF formula preserves UNSAT-ness of any already-derived
resolvent" a standard enough fact in the SAT/SMT literature that it has a
name and a citable proof, and does the *proof technique* (not just the
statement) transfer cleanly to Rodin's region/interval representation rather
than boolean CNF? Are there existing systems (dependency resolvers, build
systems, or SAT solvers doing incremental re-solving) that explicitly exploit
this asymmetry to skip re-derivation after a one-sided edit, rather than
just after "no edit"? Is there a symmetric argument for req-narrowed beyond
the CNF analogy given that VersionSet intervals, not boolean clauses, are the
native currency here — does interval narrowing actually correspond to clause
addition, or is that analogy doing too much work?

**(d) Fact weakening instead of invalidation.** Rather than a binary
valid/invalid fact, could a widening-unsafe change WEAKEN a fact to a
smaller, still-true region rather than discarding it outright — e.g. if
`req-widened` on edge `Y` invalidates "region R conflicts because Y<2.0",
could the solver instead learn the strictly-narrower-but-still-true "region R
∩ (still-excluded sub-range of Y) conflicts", keeping partial value instead of
throwing the whole fact away? Is this a known move in incremental SAT
(learned-clause "shrinking" or "clause strengthening" on formula edits) or
truth maintenance systems (JTMS/ATMS literature — is fact weakening on
axiom-retraction a solved problem there, and does that solution generalize
past boolean truth values to region algebras)?

**(e) Interval/lattice algebras over requirement space.** Is there a
principled lattice/interval-arithmetic formulation where a fact's validity is
itself an interval (or region) in "manifest-edit space," not just "package
version space" — so that widening becomes lattice-join/meet reasoning instead
of an ad hoc read-set-equality check? Look at abstract interpretation
(interval/lattice domains), and constraint-based version-solving papers
(e.g. any formal treatment of PubGrub-family algorithms, OPAM's dose3/Dogen
work, `resolvo`) for whether anyone has already formalized "the set of
manifest edits under which a derived incompatibility remains sound" as a
lattice object with computable operations.

**(f) Incremental-SAT clause reuse across formula edits.** Rodin's problem —
"a manifest edit adds/removes/tightens/loosens a small number of clauses;
which previously-learned clauses survive?" — is structurally close to
incremental SAT/SMT solving under formula edits (assumption-based solving,
incremental clause databases, MiniSat's/CaDiCaL's incremental APIs, Z3's
push/pop and incremental tactics) and to SMT-based program verification's
"incremental verification after a code edit" literature. What's the
state of the art for "which learned clauses remain valid after a formula
edit, without re-running the whole solve," and does any of it transfer to a
region/interval representation rather than boolean CNF? This is probably the
single most mature adjacent field — prioritize depth here over breadth.

## 5. What to hand back

For EACH direction (a)–(f):
1. Prior art found, with real citable sources (title/author/venue/year or a
   URL you actually visited — flag anything you're not fully sure exists).
2. The strongest candidate technique for Rodin's specific setting, stated
   precisely enough that an engineer could start implementing it.
3. Soundness conditions — under what assumptions is it actually safe, and
   what does it require Rodin to track that it doesn't already (e.g. tighter
   support-set minimization, feature-enablement state, etc.)?
4. A falsifier: a concrete metadata pattern (from §2's data, or a plausible
   invented one — say which) that would break the technique, or that shows
   it doesn't apply as broadly as it first looks.

Then rank the six directions by (expected soundness-per-engineering-cost),
not by novelty. If two directions turn out to be the same idea from different
angles (plausible for (a)/(e), and for (c)/(f)), say so and merge them.
