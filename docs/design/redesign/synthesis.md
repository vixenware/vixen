+++
title = "Synthesis · what connects the six, the keystone, and the forks"
+++

# Synthesis

Read the six per-problem docs for depth. This is the connective tissue: what the
independent analyses converged on, the one keystone under four of them, the
sequencing/dependency constraints between them, and the decisions that are yours.
Nothing here is implemented — it's the decision surface.

Where a claim comes from one problem doc I cite it (`doc N`); the docs are on this
site with the grounding. I read docs 4 and 5 line-by-line; docs 2/3/6 I'm relaying
from their (cited, on-site) analyses.

## 1. The reframe: it's far more surgical than "slop"

The loudest early take was "bin the driver." The grounded finding is the
opposite, and it vindicates the *considered* call (fix in place, it's
demand-driven and does a lot right). A striking amount of the supposed
"reimplementation" is **already delegated, or already migrated, or not broken**:

- **Layout is already on weavy** (`doc 5`): zero literal `Descriptor {…}`
  construction outside `declared_mem::*`; every offset/size/tag comes from a weavy
  `Descriptor`. "vix re-derives layout" is false.
- **The facet-discovered ABI (a) has no implementation at all** (`doc 5`) — V1's
  "two ABIs" is really *one* today (weavy-declared); recent history moved *away*
  from needing (a) (Version went Rust-intrinsic → vix struct).
- **AST parsing is already Snark** (`doc 6`) — the `AST_*` host ops are a
  pull/reflection (V10) problem, not a parsing (V5) problem. **Version is already
  migrated** to `vix/std/version.vix`.
- **The pull primitives already exist** (`doc 3`): `store.array_entry`,
  `map_pairs`, `option_payload`, `tree_entry`, `string_value` are already
  single-step and non-recursive — `RenderedValue` is a redundant push-wrapper on
  top. The hard part of V10 is mostly done.
- **Flesh's intern-deferral is not broken** (`doc 2`): the O(N²) isn't flesh
  failing to defer; it's `alloc_array_words` recomputing the container hash from
  scratch every intern. Flesh already does its job.

Consequence: the work is smaller and more targeted than the inventory's tone
implied. Good news, and it changes the order — you fix a few keystones, not
everything.

## 2. The keystone: containers are hand-encoded instead of declared

The single highest-leverage finding, hit independently from four directions: the
**container/aggregate kinds — `Option<T>`, `Array`, `Map<K,V>`, tuples, and the
vix-specific `Tree`/`Pending<T>` — are hand-encoded** (magic byte offsets,
`format!`-built type-name strings re-parsed with `starts_with`) instead of being
real `weavy::mem::Access` / `phon_schema::SchemaKind` nodes. It is the shared root
of four separate verdicts:

- **V1** (`doc 5`): this *is* the "third undeclared ABI" — `alloc_option_*`,
  `alloc_array_words`, the map/tree/pending codecs, bespoke byte formats that
  never touch a `Descriptor`, despite `Access::{Option, Sequence, Map}` existing
  for exactly this.
- **V2** (`doc 2`, `doc 5`): those same containers hash via bespoke hashers /
  from-scratch recomputation — the O(N²), and the thing that must move to
  descriptor-driven hashing.
- **V3** (`doc 4`): generics are `format!("Map<{},{}>")` at the front door and
  `starts_with("Map<")` at runtime, exactly what `SchemaKind::{List, Map, Option,
  Tuple}` + `SchemaRef::Concrete{id, args}` already model as real ADT nodes.
- **V10** (`doc 3`): the pull accessors ride `weavy::mem::Access`, whose
  Record/Enum/Option/Sequence/Map variants already describe these shapes.

**Give the container kinds real descriptors** (via `declared_mem` + phon
`SchemaKind`) and the generic store/hash/reflect paths — which already dispatch on
`descriptor.access` — handle them *for free*, and four fixes get simpler at once.
This is the thing I'd do first if the goal is leverage. (`Pending<T>` is the
exception — see the forks.)

## 3. Dependency + sequencing constraints

Not independent; the safe order is constrained:

- **V3 is foundational.** `Descriptor<String>` → `Descriptor<SchemaRef>` (`doc 4`)
  unblocks V10's `RenderNames` side-table (`doc 3`: it exists *only* because names
  live in `String` not in a real `Schema`) and V1's container generics (`doc 5`
  open Q2: the container shapes are generic, and today's `String`-keyed map has no
  notion of type parameters). Multiple agents said "this is really a V3 question
  bleeding into my scope." V3 goes early.
- **Container-descriptor migration before Blake3** (`doc 5` risk, `doc 2`). Moving
  containers to descriptor-driven hashing changes their hashes; V2's Blake3
  changes *all* hashes. Do containers first (one hash break per kind) *or* bundle
  with V2 (one break total) — but not two independent breaks. `doc 5` recommends
  containers-before-V2.
- **V4 (host registration) overlaps V1 + V2 in `lower.rs`.** The ~57 host-op
  closures are the callers of the doomed `alloc_*` (V1) and of the hash sites
  (V2). Sequence so the same closures aren't rewritten three times.
- **Demand-driven (`doc 0`) is the invariant over all of it**, enforced by
  `DriveEvent` trace assertions (spawn/memo-hit/forcing counts), not by memory —
  because the corpus passing does *not* prove laziness. The one-time investment is
  confirming those trace assertions cover each demand-sensitive fix before it
  starts.

A defensible spine: **V3 (typed schemas) → containers-as-descriptors (V1) →
Blake3+incremental (V2) → reflection pull (V10) → host registry (V4) → parsers
(V5)**, with V6 newtypes as mechanical side-tracks throughout and V4's HostCtx
bundle landing whenever (it's largely orthogonal). This is a *proposal*, not a
decision — the forks below gate it.

## 4. The forks — your calls

1. **Where `Pending<T>` lives** (`doc 4`, the one real design fork). Every other
   generic wrapper maps onto a `SchemaKind`; `Pending` doesn't (it's a
   demand-machine "not realized yet" state, and phon is off-limits). Options: a
   vix-local `enum VixSchemaRef { Value(SchemaRef), Pending(SchemaId) }`, or move
   pending-ness *out of the schema* onto the `Handle` (it's a scheduling state,
   not a value shape — arguably cleaner per V8). Gates V3's stage 4.
2. **Hash-compatibility + V2 sequencing** (`doc 5` Q1). During the container
   migration, must content hashes stay bit-for-bit compatible with the current
   bespoke hashers, or is a bump acceptable (bundling with V2's inevitable
   break)? Gates whether V1's container steps can run independently of V2.
3. **Full vs minimal host registration** (`doc 1`). Full = ops become
   ctx-taking free fns (kills the inline-in-`burst` horror, ~57-op change);
   minimal = registry owns names+order, closures stay inline. I recommend full,
   staged — but flag the **reentrancy audit** (HostCtx bundles `&mut` borrows of
   five disjoint `self` fields; if `invoke` re-demands it panics) as the gate.
4. **Build facet ABI (a) now, or leave the slot empty?** (`doc 5` Q3). Nothing in
   the corpus needs a Rust-native type to cross the store boundary today. I'd
   leave it empty until something forces it (and note `#[repr(Rust)]` enums make
   it non-trivial when it comes).
5. **Snark-for-binary scope** (`doc 6`). Honest feasibility: yes for AST (done),
   plausibly for tar (experiment), **no for ELF** (data-dependent pointer-chasing
   is Kaitai-shaped, not tree-sitter-shaped). ELF likely stays a registered
   capability. Do you want the tar experiment run to settle the boundary?
6. **flesh rename** — docs 1 and 2 both land on **"molten"** ("scratch" collides
   with `weavy::mem::runtime::RawScratch`). Confirm and it's a mechanical rename.

## 5. Hazards found in passing (real, not the main fixes)

- `lower.rs:118` `assert_eq!` forcing two independent interning passes to
  reproduce identical allocation order — fragility a content-derived `SchemaId`
  deletes by construction (`doc 4`).
- A **second god-enum**: `crate::value::Value` (`src/value.rs:8`), belonging to
  the dying legacy interpreter — out of scope, flagged so V10 doesn't conflate it
  with `RenderedValue` (`doc 3`).
- `descriptor_word_schema` recovering primitive kind by **string prefix**
  (`schema.starts_with("Int")`) — V6 disease leaking into otherwise-clean
  descriptor code (`docs 4, 5`).
- A **superseded vixenware doc** that recommended the *opposite* of V2 (keep
  SHA-256, make vx-store multi-algorithm) — worth knowing it's now overridden
  (`doc 2`).
- A taint-check behavior fix hiding inside the reflection refactor (`doc 3`).

## Status of the set

| Doc | Status |
|---|---|
| [0 · Demand-driven](/redesign/0-demand-driven/) | mine, done |
| [1 · Host registration](/redesign/1-host-registration/) | mine, done (catalog + reentrancy grounded) |
| [2 · Hashing + flesh](/redesign/2-hashing-flesh/) | agent, done |
| [3 · Reflection](/redesign/3-reflection/) | agent, done |
| [4 · Typed schemas](/redesign/4-typed-schemas/) | agent, done |
| [5 · Layout/ABI](/redesign/5-layout-abi/) | agent, done |
| [6 · Parsers/Snark](/redesign/6-parsers-snark/) | agent, done |

Nothing is being implemented. This set is for your approval, doc by doc — the
site is annotatable (`--dev-editor amos`), so mark them up and I react.

Three more docs, [7](/redesign/7-molten-reuse/), [8](/redesign/8-intra-demand-loops/),
[9](/redesign/9-road-2-evidence/), landed 2026-07-07 — molten reuse, tail-call
loop lowering, and the road-2 fork measurements. These are retrospective
(code + tests already on `rodin`), not proposals; they're outside this
synthesis's scope (which predates them) and are listed here only so this
status table stays a complete index of the `redesign/` set.
