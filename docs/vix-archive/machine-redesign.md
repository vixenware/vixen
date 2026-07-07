+++
title = "vix machine — redesign inventory"
+++

# vix machine — redesign inventory

Status: **living doc.** Opened during a read-through of `driver.rs` (9,843 lines)
that Amos judged slop. Method (Amos's framing): *understand everything the current
machine is supposed to do → decide what each part should look like, in what form,
and when it must be functional.*

Round 1 of Amos's review (21 annotations, 2026-07-06) is folded in below. Verdicts
that were Amos's from the start are **V**; clarifications and new decisions from the
review are marked **[R1]**.

Column meanings:
- **Is** — what the code does today, cited by symbol (`vix/src/machine/…`).

<!-- note
+++
author = "Amos Wenger"
kind = "note"
id = "a6040d6fd88bd3d22e0a10c3e040f9da"
created = "2026-07-06T09:48:20.066360+00:00"
quote = "what the code does today, cited by symbol"
+++
HELLO I'M INLINE
-->

- **Should** — **[DECIDED]**, **[OPEN]** (a real, pointed question), or
  **[DISCUSS]** (Amos wants to decide this together, in person).

---

## Design verdicts (constraints)

- **V1 — layout/ABI is weavy's, and there are *two* ABIs. [R1]** Not one. **(a)**
  The **Rust/facet ABI**: for types defined in Rust, we do *not* control the
  layout (the Rust ABI is unstable), so we abide by the layout we **discover at
  runtime** via facet. **(b)** The **default weavy ABI**: for types *not* defined
  in Rust, *we* pick the ABI — and then any Rust code that touches such a value
  **must go through our layout and our accessors**, never assume Rust layout. Not
  extensible for now (we don't need it to be). Both are weavy's concern; the
  machine must stop re-deriving layout on top of `weavy::mem`.

- **V2 — hashing: Blake3, and either a weavy builtin or lowered to weavy IR — not
  a reflective Rust intrinsic. [R1]** `hash_value_into` (`Sha256` over
  descriptor+bytes) is wrong three ways: **(1)** SHA256 → must be **Blake3**
  (this is not cosmetic — see vx-store below); **(2)** a reflective Rust host
  intrinsic walking the `Descriptor` at runtime → should be a **weavy builtin**
  (Blake3 is useful enough to earn one) *or* lowered weavy IR; **(3)** owned by
  **weavy, never phon** (phon is done — leave it alone).
  - *Why walk fields at all instead of hashing the whole byte range?* (Amos's
    question, note 2.) Two reasons, and only two: **padding** bytes are
    indeterminate, so hashing raw memory breaks "equal value ⇒ equal hash"; and
    **handle fields** must be hashed by their *referent's content hash*, not the
    raw handle integer (which is an allocation-order artifact). So: a value that
    is POD-flat (no padding, no handles) *can* be hashed as one range; otherwise
    the walk is forced. That's the rule — not "always per-field."

- **V3 — schemas are not strings; that's one symptom of a disease (see V6). [R1]**
  weavy made `Descriptor<SchemaRef>` generic over the schema ref; phon already
  ships the right instantiation (`phon-ir/src/descriptor.rs:32`:
  `Descriptor = weavy::mem::Descriptor<SchemaRef>`). vix chose `String` and
  bypassed it.

- **V4 — capabilities are *registered* with stable identifiers, not a fixed
  position-indexed array. [R1]** A codegen context set up **once** at machine
  startup registers the host fns (call them capabilities or host fns — the name
  doesn't matter); generated code references them. Position index is acceptable
  *only* as an efficiency detail derived from registration order — but each
  capability needs a **stable identifier**, because when we cache
  lowering-to-weavy to disk for a warm start, a position index would not survive.
  The current 57 `*_HOST: u32` constants (`driver.rs:112–168`) have neither
  registration nor stable identity.

- **V5 — format parsers are not machine primitives; and the best home is probably
  Snark. [R1]** `ELF_DOC`/`AST_DOC`/`OCI_DOC`/`DOC_PARSE`/`CRATE_ARCHIVE` are
  baked into the host table. Three ways to do them right, in Amos's order of
  preference:
  1. **Snark extended to binary formats** (favored). Snark is the tree-sitter
     reboot, currently for string inputs — the same technique should work for
     binary. Declarative, debuggable, visualizable, lowered to weavy, fully
     tracked. "A detour, I don't care, we have months."
  2. **Parsers written in vix directly** — memoization etc. for free.
  3. **A registered Rust capability** — only if we solve exposure; **never** built
     into the language.

  The anti-pattern to avoid: turning "a build step emits an executable, another
  step reads a property of it (e.g. version)" into 5,000 lines of Rust.

- **V6 — the master disease: `String`/`u64`/`i64` for everything instead of
  strongly-typed newtypes. [R1]** `Descriptor<String>`, handles as bare `i64`
  (`< 0` ⇒ flesh), `hash: u64`, `store_tag_region: u32`, `fn_ref: usize` — all the
  same reflex. V3 and half the store's opacity are instances of this.

- **V7 — host ops lower through a typed, checked path; the worst offender is
  `path_with_ext`. [R1]** Concretely: `PATH_WITH_EXT_HOST = 14`, closure at
  `driver.rs:3935` — it reads raw frame words (`read_frame_word`, `as usize`),
  does silent fallbacks (`rsplit_once('.') … unwrap_or`), hand-allocates bytes,
  type-checks nothing. weavy *already* defines the host-call frame as a declared
  record with typed offsets the lowering knows (`Op::HostCall`, `task.rs`); this
  throws that away. Fix: read/write the frame through the declared-record
  accessors, or write the op as vix stdlib code — not raw pokes.

- **V8 — the language must not know domain concepts. [R1]** `cc_capability_hash`
  building a reflective `Value::Struct { name: "Cc", … }` is a *lesser* sin (that
  level of hashing could be fine); the real crime is that **the language knows
  what a `Cc` (C compiler) is at all.** Vixen-the-build-system cares about C
  compilers; Vixen-the-language must never. No domain vocabulary in the
  machine/language layer — same root as V5.

- **V9 — the store should be indexable, but this is the *least* of the store's
  problems. [R1, deprioritized]** `by_content: HashMap<(String, ContentHash),
  i64>` lets you look up by neither content hash nor handle alone. Real, but far
  down the list; the store's untyped opacity (V6) is the actual blocker to even
  *describing* its target form.

- **V10 — `RenderedValue` is the banned god-enum; delete it, expose *pull*
  accessors. [R1, corrected]** `pub enum RenderedValue` (`driver.rs:345`,
  `#[facet(tag="type")]`, ~20 variants incl. `Version`/`VersionSet`/`Sealed`) is
  exactly the god-Value-enum vix exists to forbid: one materialized enum that
  eagerly represents *any* value, serialized to JSON for a UI, with domain
  concepts baked into variants (V5/V8 again). "Reflection interface" was a
  euphemism — reflection is **pull** (given handle + schema, ask for tag / field /
  next element, materialize nothing); this is **push** (walk once, build the whole
  tree). The target is pull accessors over frozen weavy-schema'd bytes; the
  god-enum does not get renamed, it gets removed. Any UI that wants a tree builds
  its own by pulling.

---

## What weavy + phon + vx-store provide (grounded)

The Cargo comment is explicit — *"vix lowers to weavy (portability is weavy's
concern)."*

**weavy** (`/Users/amos/oss/facet-cc/weavy`)
- `mem::Descriptor<SchemaRef>`; `mem::declared::{scalar, handle, array_of,
  declared_struct, declared_enum, tag_width_for}` — **the struct/enum ABI already
  lives here.** V1's two ABIs both ride weavy.
- `mem::runtime::{Layout, RawScratch, RawArrayBuffer, RawArrayBuilder,
  InitializedLedger, HandleGuard}` — the runtime memory model. The `Vec<u8>` store
  + flesh are hand-rolled over this.
- `task::{Program, Fn, FnId, Op, Task, TaskStep, HostFn}` — the task VM itself.
  `HostFn = &mut dyn FnMut(&mut [u8])`; `Task::run_hosted(hosts: &mut [HostFn])`
  invokes hosts by index into a slice. Index calls are weavy's calling
  convention; the rot is the hand-maintained fixed array (V4).
- Candidate home for the **Blake3 content-hash builtin** (V2).

**phon** (`/Users/amos/oss/facet-cc/phon`) — **off-limits; leave it alone.**
- `phon-schema`: `SchemaId(u64)`, `Schema`, `SchemaKind`, `SchemaRef`.
- `phon-ir/src/descriptor.rs:32`: the correct `Descriptor<SchemaRef>` vix should
  have used. That's all we take from phon: the type. We don't extend phon.

**vx-store** (in **vixenware/vixen**, the private repo — the persistence answer)
- Content-defined-chunking, content-addressable store keyed on **Blake3**;
  "incremental for free, ~7× faster." This is why V2 insists on Blake3: the
  machine's content hash and vx-store must agree. doc 90's "no cross-process
  persistence" finding is resolved here — persistence *was about to join* via
  vx-store; it's a store feature (consistent with the open/proprietary split).

---

## Inventory — what the machine does today

### 1. Value store
- **Is.** `ValueStore { entries, by_content: HashMap<(String, ContentHash), i64> }`;
  `StoreEntry { schema: String, bytes, content_hash, taint }`.
- **Should. [OPEN]** — blocked on V6: while every field is `String`/`i64`, the
  store's target form can't even be *stated* (note 13). Newtypes + real schema
  refs first; indexability (V9) later.

### 2. "Flesh" — mutable staging **[RENAME NEEDED]**
- **What it actually is.** A mutable, refcounted, copy-on-write working
  representation for aggregate values (records/maps/arrays) *before* they freeze
  into the content-addressed store. Flesh handles are **negative** words
  (`flesh_word = -1 - index`); store handles are positive; `FLESH_INTERN_HOST`
  freezes flesh → store; `force_flesh_copy` toggles COW vs eager copy.
  (`driver.rs:813–878`, `1439`.)
- **Correction (I was wrong).** I called the negative-`i64` encoding "the rot." It
  is not — a tagged handle space (negatives = unfrozen) may be a perfectly good
  encoding. The genuine problems are only: **(a)** the name "flesh" is terrifying
  and opaque — **needs a better word** (freeze metaphor suggests *molten* /
  *draft* / *wip* / weavy already has *scratch*); **(b)** zero docs on the
  lifecycle.
- **Should. [ANSWERED — flesh is load-bearing, but not for the reason it looks].**
  Read Rodin's reuse model (`vixenware/vixen/docs/design/rodin-in-vix.md`,
  stax-profiled 2026-07-05). Verdict: the flesh tier **is required**, and the
  refcount tier already exists (`FleshEntry { refs: usize }`, `driver.rs:819`).
  But the load-bearing reason is **intern-avoidance, not copy-avoidance**:
  - Measured: **86% of solver CPU was SHA-256** in
    `intern_flesh_word → intern_value_bytes_children → alloc_array_words`. A
    monotonically-growing solver structure interned *every step* re-hashes the
    whole O(N) tree → **O(N²) hashing**. That's the "50×".
  - The COW/reuse stencils (mutate-vs-copy) measured **only ~5% faster** (14.6 vs
    15.3 ms) — because copy was never the bottleneck; per-step re-hash was.
  - So flesh's real job is to keep the **solve interior un-interned and un-hashed**
    across the inner loop, interning once at the demand boundary. That's the
    load-bearing property; the reuse optimization is secondary.
  - Name still wants fixing; **existence is now justified by measurement**, and my
    "encoding is rot" was doubly wrong — it's a measured-necessary perf tier.

### 3. Content hashing
- **Is.** `hash_value_into` (`Sha256`), `hash_map_pairs`, `hash_with_taint`, plus
  `Value::canon_hash`. Measured (stax, `rodin-in-vix.md`): this is **86% of solver
  CPU** — a growing structure re-hashed in full every intern = O(N²).
- **Should. [DECIDED: V2 + sharpened].** Blake3, weavy builtin (or lowered IR),
  field-walk only where padding/handles force it, aligned with vx-store. **But
  Blake3 alone is only a constant-factor win** — the O(N²) killer needs
  **incremental / Merkle hashing**: appending one element updates the digest in
  O(log N), not a full re-hash. This is the actual perf fix, paired with flesh
  (subsystem 2) keeping the hot loop un-interned. (Refines doc 90: content-address
  is free at the *fact/demand-graph* level, ruinous in the *solve interior* — which
  is precisely why flesh exists.)

### 4. Schemas / ABI
- **Is.** `weavy::mem::Descriptor<String>`, string-keyed; `Access::Scalar if
  descriptor.schema == "Float"` string dispatch.
- **Should. [DECIDED: V1/V3]** — the two ABIs (facet-discovered / weavy-default);
  `Descriptor<SchemaRef>`; no string dispatch.

### 5. Host-op table
- **Is.** 57 `*_HOST: u32` constants → `[HostFn; 57]`, dispatched by index; mixes
  true primitives with parsers/semver/capabilities.
- **Should. [DECIDED: V4]** — startup registry, stable identifiers, index as an
  impl detail.

### 6. Lowering (`lower.rs`, 11,760 lines)
- **Is.** AST → `LoweredFn` → host-op calls over a frame-word ABI; plus the
  reflection/render path and method/operator dispatch. Worst offender: the
  `path_with_ext` closure (V7).
- **Should. [OPEN]** — typed lowering IR. Whether the frame ABI itself survives is
  **[DISCUSS]** (see decisions).

### 7. Demand / memo / read-set
- **Is.** Scheduler + projection memo (`MemoEntry`, `MemoProjectionHit`),
  field-granular read-sets (`ProjectionPath`). Grounded in doc 90.
- **Should. [OPEN]** — expose read-sets to resolver code (needed by doc-50
  read-set widening; currently internal)? Persistence is answered: vx-store.

### 8. Projections (artifact parsing)
- **Is.** `elf`/`oci`/`ast_probe`/`tar`/`crate_archive`, glued via
  `PendingPrimitiveKind::{to_word,from_word}` (`driver.rs:783`) — which packs the
  three parser kinds into disjoint `i64` ranges: elf `< 1000`, ast `1000–1999`,
  oci `2000+`. That's a hand-rolled tagged union encoded as an integer (the
  `+ 2_000`). Understood now — but **understand fully before nuking** (note 19).
- **Should. [DECIDED: V5]** — Snark-for-binary (favored) / vix / registered Rust
  capability.

### 9. Exec / fetch backends
- **Is.** `MachineExecBackend` (`driver.rs:103`) + `FetchBackend`, trait-based;
  real impls `RealProcessBackend` (`real_process.rs:47`) and mock
  `DeferredExecBackend` (`lower.rs:6842`). **Note:** this is *not* the legacy
  interpreter-oracle — that's `oracle.rs`/`engine.rs`, already frozen and slated
  to "die at corpus parity" (`machine/mod.rs:7`). The exec backend is the
  run-a-tool abstraction.
- **Should. [DECIDED-ish, R1].** Kill the trait-divergence. **Fetch** becomes a
  **configurable primitive** — mock URLs, record & replay — not a swappable trait.
  **Exec**: Amos wants to know *why* we even have a machine exec backend; likely
  entangled with the dying oracle. Investigate its real callers, then most likely
  collapse to the same configurable-primitive shape (or delete).

### 10. Value inspection — the `RenderedValue` god-enum
- **Is.** `render_word` → `render_doc`/`render_map`/… eagerly materialize
  `pub enum RenderedValue` (`driver.rs:345`) — the god-Value-enum, ~20 variants,
  Facet-tagged, serialized to JSON for a UI. Push, not pull; domain-aware
  (`Version`/`VersionSet`/`Sealed` variants). Produced at every result boundary
  (`render_result`: `vix-daemon/src/lib.rs:332`, `crate_lock.rs:162`, ~all tests).
- **Observable tax (evidence).** Because it's push, you must build the *whole* tree
  before you can look at any of it. A failed workspace resolve therefore **vomits
  ~20k lines** of materialized god-enum instead of a targeted diagnostic; and even
  tests pay it — `let RenderedValue::Map { entries, .. } = render_result(...)`
  materializes the entire tree to read one field.
- **Should. [DECIDED: V10]** — delete the god-enum; expose **pull accessors** over
  frozen weavy-schema'd bytes (tag / field / element). Not a formatter, not a
  materialized tree.

### 11. Sealed / taint (capability / info-flow)
- **Is.** `Value::Sealed { taint, recipient }`, `StructuralTaint`,
  `combine_taints`. Undocumented.
- **Should. [OPEN]** — staying in the on-device substrate? documented model?

### 12. Version / VersionSet (semver)
- **Is.** host primitives (`version.rs`, `version_set.rs`).
- **Should. [DECIDED, in progress]** — live in vix (`vix/std/version.vix`); remove
  the host ops (V5 again: semver is not a machine primitive).

---

## Open decisions

1. **Frame ABI survival. [DISSOLVED — was a non-question.]** Reading
   `weavy/src/task.rs` (ruled `tooth-2-frames-abi.md`): a frame is a **declared
   record** (args/locals/spills via `mem::declared`, byte offsets, per-task arena),
   and *every* call — weavy→weavy (`Op::Call`) and weavy→host (`Op::HostCall`) —
   writes typed args frame-direct at known offsets. Frames are the VM's universal
   calling convention; there is no roof to remove. The only real issue is V7: host
   ops ignore that the frame is a declared record and poke raw words. No
   discussion needed — I manufactured the fork by not reading (note 17 was right).
2. **A better name for "flesh."** Options: *molten* / *draft* / *wip* / *scratch*.
3. **COW staging necessity** — answerable only against Rodin's reuse model; I read
   that next.
4. **Registry identity scheme** (V4) — what the stable identifier *is* (name?
   content hash of the capability's signature?).
5. **Sealed/taint** — in or out of the on-device substrate.

*(Retired as answered by review: "owned by weavy or phon" → weavy; "persistence
as a store feature" → vx-store; "expose read-sets" → real, tracked under
subsystem 7. And: my earlier open questions were too vague — note 18 — so they're
either pointed above or deleted.)*
