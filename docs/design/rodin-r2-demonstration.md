# R2 demonstration gap: read-set-stability widening vs. what fires today

*Companion to `docs/design/prior-art-adjudication.md` (R2, "SURVIVED 3/3,
deeply novel") and `docs/design/rodin-c-evidence.md` (rounds 1–8). This
document confirms what the widening code actually does today, and designs
the demonstration that would exercise the mechanism the R2 novelty claim
describes.*

## STEP 0 — verdict: DECLARED-structure equality, not observed read-set stability

**Widening today fires on declared-manifest equality across neighbor
versions, computed offline. There is no metadata-fetch layer, no observed
read-set, and no `ReadSet`/`ReadSetStability` type anywhere in
`rodin-core` or `rodin-facts`.** `grep -rn "ReadSet"` across every crate in
the workspace returns zero matches.

The widening decision function, `crates/rodin-core/src/lib.rs:1430-1460`:

```rust
pub fn widen_region(
    &self,
    selected: &BTreeMap<PkgIx, Version>,
    support: &BTreeSet<EdgeKey>,
    base: &Region,
) -> (Region, usize, usize) {
    let mut region = base.clone();
    let mut point = 1_usize;
    let mut widened = 1_usize;
    for edge in support {
        let Some(parent_pkg) = self.package(&edge.parent) else { continue; };
        let Some(selected_version) = selected.get(&parent_pkg) else { continue; };
        let selected_state = self.edge_state(&edge.parent, selected_version, &edge.dep);
        let mut versions = Vec::new();
        for version in self.versions(parent_pkg) {
            if self.edge_state(&edge.parent, version, &edge.dep) == selected_state {
                versions.push(version.clone());
            }
        }
        point = point.saturating_mul(1);
        widened = widened.saturating_mul(versions.len().max(1));
        region.versions.insert(parent_pkg, version_set_from_points(&versions));
    }
    (region, point, widened)
}
```

The comparison it calls, `edge_state` (`crates/rodin-core/src/lib.rs:1401-1427`),
pulls an already-parsed `Manifest` out of `self.manifests: BTreeMap<(PkgIx,
Version), Manifest>` and compares the declared dependency line's fields:

```rust
pub fn edge_state(&self, parent: &str, version: &Version, dep: &str) -> Vec<EdgeSignature> {
    let Some(pkg) = self.package(parent) else { return Vec::new(); };
    let Some(manifest) = self.manifests.get(&(pkg, version.clone())) else { return Vec::new(); };
    let dep_actual = self.package(dep).and_then(|pkg| self.base_name(pkg)).unwrap_or(dep);
    let mut out: Vec<_> = manifest.deps.iter()
        .filter(|line| line.actual == dep_actual)
        .map(|line| EdgeSignature {
            req: line.req.clone(), features: line.features.clone(), optional: line.optional,
            default_features: line.default_features, target: line.target.clone(), kind: line.kind.clone(),
        })
        .collect();
    out.sort();
    out
}
```

A neighbor version is folded into the widened region because its declared
`Cargo.toml` dependency line — req string, feature set, optional/default-features
flags, target, kind — is byte-for-byte equal to the selected version's. Nothing
here asks "which bytes of registry metadata did the resolver actually
consume." It asks "are these two already-parsed manifests' relevant lines
equal." That is a **static/declared** comparison, not an **observed/read-set**
one.

The certificate that gets attached to the derivation, `WidenCert`
(`crates/rodin-facts/src/derivation.rs:11-16`):

```rust
pub struct WidenCert {
    pub package: crate::PkgIx,
    pub checked: VersionSet,
    pub manifest_hash: String,
}
```

is constructed in `widen_cert_from_witness`
(`crates/rodin-core/src/lib.rs:3326-3351`) as a hash of the canonicalized
witness text (`manifest_hash: digest_str(&canonical_widen_witness(witness))`)
— again, a hash over the declared-equality witness, not a record of
observed reads.

**Data source for the evidence rounds (`rodin-c-evidence.md` rounds 4–8):
confirmed offline.** `crates/rodin-core/src/cargo_evidence.rs:283` states
it plainly: *"The production extractor is offline-only, reads local
Cargo.toml files and `.crate` archives directly, indexes cached registry
alternatives for locked package names..."* Verified in code: it reads
`Cargo.lock` from disk (`fs::read_to_string`), walks
`~/.cargo/registry/src/...` and `.../cache/...`, and extracts manifests
from local `.crate` archives. No HTTP client exists in `rodin-core` or
`rodin-facts` — `rg` for `reqwest|ureq|http` in those crates returns
nothing relevant. Rounds 4–8 all measure **resolution fidelity against
cargo's own selection**, from a static local snapshot; they never fetch,
and never exercise the widening path against anything but declared
manifest equality.

**One adjacent wrinkle worth being precise about, so the gap isn't overstated
in the other direction:** `crates/rodin-survey` (a separate, standalone
corpus tool that produced the block-count statistics in
`rodin-c-evidence.md`'s aggregate/per-crate sections) *does* perform real
HTTP fetches of crates.io's sparse index
(`INDEX_BASE = "https://index.crates.io"`), cached to disk for
reproducibility. But its "blocks" measurement (contiguous runs of versions
with identical normalized dependency metadata) is *still* a declared-equality
check over the full fetched manifest — it is not a record of which subset
of that metadata the *resolver* actually consumed while deciding a version.
It's disconnected from `rodin-core`'s solve/widening path entirely (no
call site links `rodin-survey`'s output into `widen_region`). So even the
one place in this codebase that talks to the network computes the wrong
thing for R2's claim, and doesn't feed the solver regardless.

**`rodin.md:112-114`'s own framing is metaphor, not implementation.** It
describes the intended invariant as *"a fact's validity range is its
read-set's stability range"* — but the design note itself glosses this as
"the parent-version range where the derivation's EDGE-SUPPORT is
invariant," i.e., the same declared per-edge `edge_state` comparison
already implemented. The phrase "read-set" is being used in `rodin.md` as
a conceptual label for "the manifest fields this edge's derivation
depends on," not as a literal trace of consumed metadata bytes. The
prior-art doc's R2 line — *"justified by VERIFIED read-set stability of
metadata fetches"* — is a stronger, different claim: it implies an
observation of what the resolver reads from a live/fetched source, not a
label for a static equality check.

**Conclusion: R2-as-scrubbed (verified read-set stability of metadata
fetches) is not what fires in `rodin-core` today.** What fires is
declared-dependency-line equality across neighbor versions, computed
entirely offline from parsed manifests already on disk. This is real,
implemented, unit-tested, and empirically evaluated (rounds 1–8) — but it
is a *weaker and differently justified* mechanism than the one the
adversarial scrub rated "deeply novel." The novel mechanism (widening
justified by observing that the resolver-relevant subset of registry
metadata is stable across a range, even when the full declared manifest
text differs) has never been built or exercised.

## STEP 1 — designing the demonstration

To show the real R2 mechanism firing, four pieces are needed, in
dependency order.

### (a) A metadata-fetch layer with observed read-sets, offline-reproducible

Extend (or add alongside) the existing offline extractor
(`cargo_evidence.rs`) a layer that:

1. Loads a **crates.io sparse-index snapshot** — reuse `rodin-survey`'s
   existing fetch-and-cache mechanism (`target/rodin-survey-cache/`,
   already fetches and caches JSONL rows keyed by crate name) as the
   snapshot source, so this stays reproducible without live network
   calls in tests. The snapshot is checked into a fixtures directory (or
   referenced by content hash) so CI never fetches.
2. Wraps manifest access behind an accessor that **records which fields
   of a version's index row were actually read** while the solver
   computed `edge_state` for a given `(parent, dep)` edge — e.g., a
   `TrackedManifest` that logs field accesses (`deps[i].req`,
   `deps[i].features`, `cksum`, `yanked`, ...) into a `ReadSet` the first
   time `widen_region` touches that version's manifest for that edge.
   This is the one genuinely new data structure: something like
   ```rust
   pub struct ReadSet {
       pub version: Version,
       pub fields_read: BTreeSet<FieldPath>,
   }
   ```
   where `FieldPath` identifies exactly the subset of the row consulted
   (e.g. `deps[serde].req`, `deps[serde].optional`) — deliberately
   narrower than "the whole manifest," so it can differ from
   declared-equality's "the whole relevant dependency line."
3. Crucially: the read-set must be scoped to **what the resolver's
   edge-decision logic actually consults for this specific edge**, not
   "everything in the manifest." Today's `edge_state` already only reads
   the `deps` entries matching one dependency name — so the read-set for
   an edge is naturally narrow. The demonstration's value comes from
   showing that two versions can differ in fields *outside* that
   read-set (changelog links, unrelated dep bumps, doc comments,
   unrelated feature gates) while being read-set-identical *for this
   edge*.

### (b) The read-set-stability check across a version range

Given a candidate widening range `[v_lo, v_hi]` for parent package `P` and
edge `(P, dep)`:

- For each version `v` in the range, compute `ReadSet(P, v, dep)` via (a).
- The range is **read-set-stable** iff every version's `ReadSet` selects
  the *same field paths*, AND the *values* at those field paths are
  identical across the range (this second condition is what today's
  `edge_state` already checks — the difference is that read-set-stability
  additionally tolerates and ignores differences **outside** the read
  set, whereas declared-equality's `EdgeSignature` struct bundles fields
  together and would reject on any difference not already excluded from
  that struct by construction).
- Encode this as `read_set_stable(range, edge) -> bool`, parallel to the
  existing `versions.push(...)` loop in `widen_region`, but sourced from
  fetched-snapshot `ReadSet`s instead of local `Manifest`s.

### (c) The widening certificate tied to observation

Add a new `RuleKind`/`WidenCert` variant (or a new field on the existing
`WidenCert`) that records *which* mechanism justified the widen:

```rust
pub struct ObservedWidenCert {
    pub package: PkgIx,
    pub checked: VersionSet,
    pub read_set_digest: String,   // hash of the stable field-path set + values
    pub snapshot_id: String,       // which index snapshot this was checked against
}
```

distinct from the existing `manifest_hash`-based `WidenCert`, so evidence
output can report *which* certificate type actually fired for a given
widen — this is what lets the evidence table distinguish "widened because
declared-equal" from "widened because read-set-stable but declared-unequal."

### (d) The killer demonstration scenario

The scenario that proves this does something declared-structure widening
cannot: find (or construct, if none exists at sufficient scale in the
snapshot) a crate + dependency-edge + version-range where:

- Two or more adjacent versions' **declared** manifest dependency lines
  for the target edge differ **cosmetically** — e.g., `serde = "1"` vs.
  `serde = { version = "1", features = [] }` (same effective requirement,
  syntactically different manifest entry), or an unrelated dependency
  bump in the same manifest that doesn't touch the edge in question, or a
  reordering of the `[dependencies]` table that changes `EdgeSignature`
  derivation incidentally — anything that makes `edge_state` return
  *unequal* `Vec<EdgeSignature>` for the two versions even though the
  actual resolver-relevant read set is identical.
- Read-set-stability *can* widen across this boundary (because the
  observed fields — the one dependency's req/features/optional actually
  consulted — are unchanged) while declared-equality *cannot* (because
  `edge_state`'s output differs).
- Measure the pruning win: report widened-range size (declared-equality
  mechanism) vs. widened-range size (read-set-stability mechanism) for
  that edge, across the full snapshot — the same `widening factor`
  metric already used in rounds 1–3 of `rodin-c-evidence.md`, run twice,
  side by side.

If the snapshot search turns up zero real-world instances of this pattern
at meaningful scale, that itself is a finding worth reporting honestly
(see gap ledger) — it would mean the "cosmetic-diff" case R2 is designed
to rescue is rare in practice, which bears directly on whether R2 is worth
building beyond the unit-test level.

## STEP 2 — honest gap ledger

**Built:**
- Declared-structure widening (`widen_region` + `edge_state`), fully
  implemented, unit-tested, and empirically measured across rounds 1–8
  (7.5x aggregate widening factor from round 3, 13.4x per-edge factor
  from `rodin.md`'s round 2 note).
- A real network-fetch capability exists in the codebase
  (`rodin-survey`), proven able to fetch and cache crates.io sparse-index
  data offline-reproducibly — but it's disconnected from the solver and
  computes declared-equality blocks over the *full* fetched manifest, not
  a per-edge observed read-set.
- `WidenCert` as a certificate type — but its payload (`manifest_hash`)
  is a hash of the declared-equality witness, not an observation record.

**Not built, needed for the R2 demonstration:**
- A `ReadSet`/`FieldPath` type recording which subset of a version's
  metadata a solver decision actually consulted. Zero lines of this
  exist today (confirmed by full-workspace grep).
- Any linkage from `rodin-survey`'s fetch/cache layer into
  `rodin-core`'s solve or widen path. Today they are entirely separate
  programs; the survey tool produces markdown tables for evidence docs
  and nothing else.
- The stability check comparing *read sets* (narrower, tolerant of
  out-of-read-set diffs) as opposed to the existing check comparing
  *declared dependency lines* (all-or-nothing on the bundled struct).
- A distinguishing certificate variant so evidence tooling can report
  which mechanism fired for a given widen (today there is only one
  mechanism, so no such distinction is possible or needed yet).
- The killer-scenario mining pass itself: searching the snapshot (or
  `rodin-survey`'s existing 400-crate closure) for edges where declared
  lines differ but the per-edge-relevant fields don't, and measuring the
  pruning delta.

**Is today's "widening" mislabeled relative to the R2 claim?** Yes, in the
strength/mechanism sense, not in the sense of being fraudulent — the
*language* in `rodin.md` ("read-set's stability range") is a metaphor the
implementation matches only loosely (per-edge scoping is real and
implemented; "read-set" as an observed-consumption trace is not). The
adversarial scrub in `prior-art-adjudication.md` rated the literal claim
*"justified by VERIFIED read-set stability of metadata fetches"* as
deeply novel and unmatched in prior art — correctly, as far as prior art
goes, but the claim describes a mechanism one tier stronger than what
`rodin-core` runs today. The honest framing: **rodin-core v0 implements
and validates the *per-edge, declared-structure* special case of the R2
idea; the *observed, fetch-based* general case that earned the "deeply
novel" verdict remains a design, not code.**

**What real crates.io metadata shape must be captured for the
demonstration:** per-version sparse-index JSONL rows (`rodin-survey`
already fetches and parses these with `facet-json`) — specifically the
`deps` array's `name`/`req`/`features`/`optional`/`default_features`/
`target`/`kind` fields per row, which is exactly what `edge_state` already
extracts from local manifests. The only new capture is *provenance*: which
of those fields a given solver decision actually touched, tagged
per-decision rather than computed uniformly.

**How to keep it offline-reproducible:** reuse `rodin-survey`'s existing
`target/rodin-survey-cache/` JSONL cache as the snapshot source (already
proven to make repeat runs offline), and have the new evidence-round
extractor read from that cache path exactly the way `cargo_evidence.rs`
today reads from `~/.cargo/registry/...` — same shape of "fetch once,
cache to disk, tests/evidence runs never touch the network again"
discipline already established in this codebase.
