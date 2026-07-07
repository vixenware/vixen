+++
title = "Redesign · Layout/ABI + the store on weavy"
+++

# Redesign · Layout/ABI + the store on weavy

Scope: V1 only — layout/ABI and the value store. Read-only analysis; no source
edited. All line numbers verified against the current tree.

## 0. Correcting the premise, first

The brief for this doc says "vix reimplements memory/layout that weavy already
provides" and that the store is "hand-rolled ... instead of standing on
weavy's runtime buffers." Both are half right, and the half that's wrong
matters for scoping the fix:

- **Layout computation itself is already fully delegated.** `driver.rs` never
  computes a field offset, a struct size, or a tag width. Every place that
  needs one reads `descriptor.layout.size` / `.align` or `field.offset` off a
  `weavy::mem::Descriptor` it was handed (`driver.rs:1900, 2476, 4541-4542,
  6118, 7019, 7048, 7070, 7516, 7896, 9014/9021/9028, 9210/9225/9237, 9318,
  9809/9819`). All of vix's descriptors — for builtin schemas *and* every
  user-declared `struct`/`enum` — are built exclusively through
  `weavy::mem::declared::{scalar helpers, declared_struct, declared_enum,
  handle}` (`module.rs:308-393` for user types via the AST; `driver.rs`'s
  `add_builtin_descriptors` at `lower.rs:1895-1990` for `Os`/`Arch`/`Doc`/etc).
  Grepped the whole crate for a literal `Descriptor { ... }` construction
  outside `declared_mem::*` helpers: **zero hits.** So "vix re-derives
  layout" is not what's happening; `use weavy::mem::{Access, Descriptor,
  Tag}` (`driver.rs:39`) is already the load-bearing import.
- **What's actually hand-rolled is everything *around* a `Descriptor`**: the
  physical storage backing a `StoreEntry`, and — this is the sharper finding —
  a family of vix-level container shapes (`Option<T>`, `Array`, `Map`,
  `Pending`, `Tree`) that never go through a `Descriptor` at all. They're
  bespoke byte formats with magic offsets baked directly into `driver.rs`
  functions. This is a **third, undeclared ABI**, coexisting with the two V1
  names — see §3.

So the redesign isn't "stop recomputing layout" (already done); it's "stop
having a shadow layout system for containers, and stop backing the store with
a plain `Vec<u8>` when weavy already has typed allocation primitives for
exactly this job."

## 1. (e) How facet discovers Rust layout — grounding ABI (a)

`vix/Cargo.toml:16` pulls in `facet = { path = "../facet" }` (workspace
`facet-core` underneath). The mechanism, read from `facet-core/src/types/`:

- `Shape` (`facet-core/src/types/shape.rs:104`) carries `pub layout:
  ShapeLayout` — for sized types this is a `core::alloc::Layout` (size +
  align), computed by `core::mem::size_of`/`align_of` at the point the
  `#[derive(Facet)]` macro expands, i.e. by rustc itself, not by facet
  guessing.
- `Field` (`facet-core/src/types/ty/field.rs:86-110`) carries `pub offset:
  usize`, and the doc comment is explicit: *"offset of the field in the
  struct (obtained through `core::mem::offset_of`)."* `offset_of!` is a
  **stable, compiler-verified** macro — it doesn't compute an offset
  independently and risk being wrong, it asks rustc for the real one, for
  *this* compilation. That's the literal meaning of "discover at runtime" in
  V1(a): vix/weavy never picks or assumes a Rust field's position; the derive
  macro bakes in whatever the compiler actually chose, and that's frozen into
  the static `Shape` returned by `T::SHAPE`.
- **Enums are the sharp edge.** `EnumType::enum_repr: EnumRepr`
  (`facet-core/src/types/ty/enum_.rs:156-186`) has a `Rust` variant whose doc
  says: *"The compiler chooses the discriminant size and layout as an
  implementation detail. Reflection operations that require knowing the
  discriminant layout will fail for enums with this repr."* So a plain
  `#[derive(Facet)]` enum with **no** `#[repr(u8/u16/u32/u64/...)]` gives you a
  `Shape` you can walk through vtable-driven get/set (facet-reflect's
  `Peek`/`Poke`), but you **cannot** extract a raw `{offset, width}`
  discriminant the way `weavy::mem::Tag::Direct` wants. Any facet→`Descriptor`
  bridge can only produce `Tag::Direct`/`Tag::Niche` for `#[repr(int)]` (or
  niche-eligible) Rust enums; for everything else it has to fall back to
  `Tag::Thunk { read, write }` — i.e. route through facet's own vtable calls,
  not direct byte offsets. This is exactly the class of problem
  `weavy::mem::Access`'s thunk-based variants (`OptionThunks`, `SeqThunks`,
  etc., `weavy/src/mem.rs:37-225`) already exist to express — the vocabulary
  is there, a bridge is not.

**Grounded conclusion: ABI (a) has no implementation anywhere in this tree
today.** I grepped for `facet_reflect`, `Peek::`, `Poke::`, `T::SHAPE` calls
feeding a `Descriptor` construction, across `vix/src` and `weavy/src` — zero
hits. `use facet::Facet;` in `driver.rs:35` is used for exactly one thing: the
`#[derive(..., Facet)]` on `StructuralTaint` (`driver.rs:723`), which is
Rust-side bookkeeping serialized by the daemon layer, not a machine value
routed through the store. Recent history corroborates this is intentional,
not an oversight: commit `e9305bf71` ("Version is a plain vix struct; kill the
VERSION_FIELD intrinsic") *removed* the one place where a Rust-native type's
fields were exposed as a vix value via a hand-written intrinsic, replacing it
with a vix-declared struct (ABI b) — moving *away* from needing (a), not
toward it. So today: 100% ABI (b). ABI (a) is a real, named, but currently
*empty* slot in the design — build it when (if) something forces a genuine
Rust type across the value-store boundary without a vix-side redeclaration.

## 2. (b) The two ABIs: representation and dispatch, concretely

Both ABIs share one representation — `weavy::mem::Descriptor<SchemaRef>` —
which is the point of V1: there is one descriptor vocabulary, and the
authority that *produced* a given descriptor is invisible to every consumer
(`weavy/src/mem/declared.rs:1-14`'s module doc says this outright: "Consumers
never know which authority produced a descriptor; nesting works in both
directions by construction"). Concretely:

- **Dispatch today** is 100% static, at compile time, in `module.rs:308-393`
  (`declared_descriptors`): walk the parsed AST's `Item::Struct`/`Item::Enum`,
  call `declared_mem::declared_struct`/`declared_enum` per declaration, insert
  into a `HashMap<String, Descriptor<String>>`. There is no runtime "is this
  facet or declared?" branch anywhere, because only one authority is wired
  up. The **hook point** for ABI (a), when it exists, is this same function:
  for a hypothetical `extern` or `use`-imported Rust type, `descriptor_for_type`
  (`module.rs:374-383`) would need a third arm that calls into a
  `facet_shape_to_descriptor(shape: &'static Shape) -> Descriptor<SchemaRef>`
  bridge instead of `declared_mem::declared_struct`. Everything downstream
  (`ValueStore::alloc`, `hash_value_into`, `field_offset`, render/compare
  code) already dispatches purely on `descriptor.access` (`Access::Scalar`,
  `Access::Record`, `Access::Enum`, ...) — it does not care which authority
  built the `Descriptor`, by construction. **That's the one part of V1
  already correctly built**; nothing downstream needs to change to add ABI
  (a) later.
- **`SchemaRef` today is `String`** (V3/V6 territory, not mine to redesign,
  but it's the type parameter every `Descriptor` in vix is instantiated
  with). Swapping it for a real `vix::SchemaId`-shaped ref is orthogonal to
  the ABI question — `phon-ir/src/descriptor.rs:20-31` shows the pattern
  vix should mirror (`pub type Descriptor = weavy::mem::Descriptor<SchemaRef>`
  with phon's own `SchemaRef`), and vix's ABI dispatch above works identically
  regardless of what `SchemaRef` is, since `declared_struct`/`declared_enum`
  are generic over it.

## 3. The store today, and the third undeclared ABI

`ValueStore` (`driver.rs:738-741`):
```rust
pub struct ValueStore {
    entries: Vec<StoreEntry>,
    by_content: HashMap<(String, ContentHash), i64>,
}
pub struct StoreEntry {
    pub schema: String,
    pub bytes: Vec<u8>,
    pub content_hash: ContentHash,
    pub taint: Option<StructuralTaint>,
}
```
For `Access::Record`/`Access::Enum`/`Access::Scalar`/`Access::Handle` shapes —
i.e. anything that came out of `declared_struct`/`declared_enum`/`scalar`/
`handle` — `bytes` genuinely is "the descriptor's bytes," and every read/write
against it (`field_offset`, `hash_value_into`, `write_alloc_fields`,
`read_variant_tag`) goes through `descriptor.access`. This part is sound,
modulo the storage-primitive question in §4.

But four of the store's own "schemas" — `Option<T>` (generic over any `T`),
`Array`, `Map<K,V>`, `Tree`, and `Pending<T>` — have **no descriptor at all**.
Grep confirms no `Descriptor` is ever looked up for them; instead each has a
hand-written byte format with magic offsets:

- `alloc_option_none`/`alloc_option_some`/`option_payload`
  (`driver.rs:1149-1240`): tag word at `0`, schema-ref word at `8`, value word
  at `16`, optional realization word at `24`. 24 or 32 bytes, chosen by an
  `if entry.bytes.len() != 24 && != 32` check.
- `alloc_array_words`/`array_entry` (`driver.rs:1272-1368`): kind word at `0`,
  elem-schema-ref word at `8`, count word at `16`, then `24 + i*8` per
  element.
- `alloc_tree_concrete`/`alloc_tree_merge`/`alloc_tree_exec`/`tree_entry`
  (`driver.rs:1369-1438`): kind word at `0`, then kind-specific payload.
- `encode_pending_invocation`/`decode_pending_invocation`, `encode_map_pairs`/
  `decode_map_pairs`, `encode_handle_list`/`decode_handle_list` — same pattern
  elsewhere in the file (e.g. the `64 + argc*8` / `16 + count*8` layouts at
  `driver.rs:8754-8800`).

This is precisely the job `weavy::mem::Access` already has variants for:
`Access::Option` (`OptionAccess` with `Presence::Tag{offset,width,none_value}`
or `Niche`), `Access::Sequence` (`SequenceAccess` with
`SequenceStorage::Owned{ptr_offset,len_offset,cap_offset,allocate}`),
`Access::Map` (`MapAccess`). Every one of these hand-rolled vix formats is a
special case of a shape weavy's declared-ABI vocabulary can already express
generically — vix just never asked it to, and instead wrote a fourth, fifth,
sixth bespoke integer-offset format by hand. **This, not `Vec<u8>` per se, is
the bulk of "reimplementing what weavy provides."**

## 4. (a) Standing the store on weavy's runtime memory model

Two different concerns are getting conflated by calling both "the store":

- **The flesh/scratch tier** (`FleshStore`/`FleshValue`, `driver.rs:813-878`,
  1439-1524): mutable, growing, staging storage for a value *before* it
  freezes into the content-addressed store. This is exactly the lifecycle
  `weavy::mem::runtime` was built for — its module doc says it plainly:
  *"Engine-owned raw temporary storage; callers decide what values live there
  and which thunks move those values into final handles"*
  (`weavy/src/mem/runtime.rs:1-5`). `RawScratch` (build one value's scratch),
  `ScratchSession` (reuse released buffers across a run — directly relevant to
  the doc's measured "86% of solver CPU is re-hashing," since a
  `ScratchSession` amortizes allocation the same way flesh interning avoids
  hashing), `RawArrayBuilder` (grow-and-mark-initialized, with automatic drop
  of only the initialized prefix on early abort), and `HandleGuard` (drop
  unless disarmed after adoption) are the *precise* vocabulary for `FleshValue
  ::Record{bytes: Vec<u8>}`, `FleshValue::Map{pairs: Vec<MapPair>}`, and
  `FleshValue::ArrayWords{words: Vec<i64>}`. Right now flesh reimplements
  "grow a buffer, know how much is initialized, free the rest on drop" with
  plain `Vec`s and hand-tracked state; weavy's runtime types give the same
  thing with drop-safety built in and, for `ScratchSession` specifically,
  buffer reuse across the hot solver loop — which is exactly what flesh's
  measured job is (doc `90`/inventory §2: avoid per-step re-hash, not
  copying).
- **The frozen store tier** (`ValueStore`/`StoreEntry`): once interned, a
  value is immutable and lives for the arena's lifetime, keyed by content
  hash. This is a *different* lifecycle than "scratch that gets adopted or
  dropped" — there's no adoption/ownership-transfer dance, no drop-on-abort
  concern, values just sit there until the store itself goes away. A `Vec<u8>`
  is not *unsafe* here (every read goes through `copy_from_slice`/
  `from_le_bytes` on byte slices, never a typed pointer cast, so misalignment
  is not UB) — but it throws away two things weavy's runtime model tracks
  explicitly: (1) **alignment** (`vec![0u8; size]` at `driver.rs:1900, 2476,
  6118, 9014, 9021, 9028, 9809, 9819` allocates to align 1, discarding
  `descriptor.layout.align`; harmless only because every access is
  byte-copy-based, but it means the store's bytes could never be borrowed as
  `&T` even if that were ever wanted), and (2) **variable-length element
  storage with a real stride**, which is exactly `RawArrayBuffer`'s job
  (`ptr/cap/stride`, `slot(index)`) and is precisely what `Array`'s hand-coded
  "count word + N words" format in §3 is reinventing badly (fixed 8-byte
  stride, no support for non-word-sized elements, offsets hand-added
  everywhere they're read).

**Recommended shape**, not a full spec:
1. Give `Array`/`Map`/`Option<T>` real `Descriptor`s built through
   `declared_mem` (an `Array` becomes `declared_struct` of a tag/kind field
   plus a `Sequence` access over elements; `Option<T>` becomes a two-variant
   `declared_enum`; `Map<K,V>` becomes a `Sequence`/`Map` access over sorted
   pairs) instead of the bespoke formats in §3. Once they have descriptors,
   `alloc`, `hash_value_into`, `field_offset` etc. handle them **for free** —
   no new store code, because those functions already dispatch on
   `descriptor.access` generically. The magic-offset functions
   (`alloc_option_*`, `alloc_array_words`, `array_entry`, the `Tree`/
   `Pending` codecs) collapse into calls to the *existing* generic alloc path.
2. Back `StoreEntry`'s bytes with an allocation that at minimum honors
   `descriptor.layout.align` — a `Box<[u8]>` allocated via
   `std::alloc::Layout::from_size_align` (which is literally what
   `RawScratch::from_layout` already wraps) rather than `vec![0u8; size]`.
   Whether that's "call `RawScratch` and never dealloc it early" or a
   store-owned arena using the same `Layout` type is an implementation
   choice; the point is the store should not be blind to alignment when
   weavy already computed it.
3. Use `RawArrayBuffer`/`RawArrayBuilder` for the actual element storage of
   `Sequence`/`Array`-shaped values, once (1) makes them real `Access::
   Sequence` values with a real `stride` — this replaces the fixed "8 bytes
   per element" assumption baked into `alloc_array_words`.
4. Flesh moves onto `ScratchSession` for its buffers; `FleshStore::alloc`
   keeps its own indexing (negative-handle space) since that's vix's
   dedup/staging bookkeeping, not a weavy concern — see §5.

## 5. (c) What to remove from driver.rs

Concretely, in favor of weavy vocabulary already available:

- **Remove entirely** (become dead code once (1) above lands): `alloc_option_
  none`, `alloc_option_some`, `option_payload`, `alloc_array_words`,
  `array_entry`, `encode_pending_invocation`/`decode_pending_invocation`,
  `encode_map_pairs`/`decode_map_pairs`, `encode_handle_list`/
  `decode_handle_list`, `alloc_tree_concrete`/`alloc_tree_merge`/
  `alloc_tree_exec`/`tree_entry`'s hand-rolled tag/offset logic. All of it
  replaced by: build the right `Descriptor` once via `declared_mem`, then call
  the *existing* generic `ValueStore::alloc`/`hash_value_bytes` path that
  already walks `descriptor.access`.
- **Keep, unchanged**: `field_offset`, `field_descriptor`, `read_variant_tag`,
  `write_variant_tag`, `hash_value_into`, `write_alloc_fields` — these
  already correctly delegate to `Descriptor`/`Access`/`Tag` and are the
  pattern the removed functions should have followed from the start.
- **Keep, but note the tension**: `descriptor_word_schema`
  (`driver.rs:7617-7622`) special-cases `Access::Scalar` by *prefix-matching
  the schema string* (`schema.starts_with("Int")` etc.) to recover a
  canonical word-schema — this is V6 disease (string dispatch standing in for
  a real type tag) leaking into otherwise-clean descriptor-driven code, not a
  layout bug per se. Flag for V3/V6, not this doc's fix.
- **`vec![0u8; descriptor.layout.size]` call sites** (`1900, 2476, 6118, 9014,
  9021, 9028, 9809, 9819`): mechanical replacement with an alignment-honoring
  allocation once §4.2 is decided — small, uniform diff, no semantic change.

## 6. (d) The vix↔weavy boundary

**weavy's job**, already true today and unchanged by this redesign:
- The `Descriptor<SchemaRef>`/`Access` vocabulary and its two producers
  (`declared::*` for language-declared types; a not-yet-written
  `facet_shape_to_descriptor` for Rust-native types) — layout, tags, byte
  ownership proofs, all of it.
- The runtime allocation primitives (`RawScratch`, `ScratchSession`,
  `RawArrayBuffer`, `RawArrayBuilder`, `HandleGuard`, `InitializedLedger`) —
  generic, engine-agnostic, no vix concept in sight.

**vix's job**, unchanged in kind, changed in how much code it takes:
- **Content-addressing**: `hash_value_into` walking `descriptor.access` to
  produce a `ContentHash`, and the Blake3/incremental-hash rework tracked
  under V2 (separate doc territory, but it consumes the same `Descriptor`
  walk this doc is about).
- **Dedup**: `by_content: HashMap<(SchemaRef, ContentHash), i64>` — this is a
  vix-specific index over vix's own handle space; weavy has no notion of a
  "value store" or handle interning at all, so this stays entirely vix's.
  (V9's "should also be indexable by handle alone" is a real but separate,
  deprioritized concern per the inventory.)
- **Taint** (`StructuralTaint`, `combine_taints`, `hash_with_taint`) — a vix/
  substrate concept with no weavy analog; stays vix's, unaffected by this
  doc.
- **Flesh's negative-handle dedup/refcount bookkeeping** (`flesh_word`,
  `FleshEntry.refs`) — this is vix's demand-driven intern-avoidance
  invariant (doc 90's measured finding), not something weavy has an opinion
  on; only the *raw buffer* underneath a `FleshValue` moves to weavy's
  scratch primitives (§4), the indexing/refcounting logic stays.
- **The schema→descriptor compiler** (`module.rs:308-393`): walking vix's own
  AST and calling into weavy's `declared_mem` builders is inherently vix's
  job (weavy doesn't know vix syntax); this doesn't change.

The boundary, in one line: **weavy decides what a value's bytes mean and how
to allocate/adopt raw storage for them; vix decides which bytes exist at all
(content-addressing, dedup, taint, demand/intern semantics) and is the only
caller of weavy's layout/runtime APIs.**

## 7. Blast radius

- **Direct edits**: `driver.rs` (`StoreEntry`, `ValueStore::alloc*`,
  `FleshStore`/`FleshValue`, the option/array/map/tree/pending codecs listed
  in §5 — call sites throughout the 700-9845 line range, since these
  functions are called from dozens of host-op closures in both `driver.rs`
  and `lower.rs`). `module.rs` is untouched (already correct) except for
  whatever new `Descriptor` shapes `Array`/`Option`/`Map` need registered
  alongside the builtins.
- **Callers who see no change**: everything downstream of `descriptor.access`
  dispatch (render/compare/hash code in `driver.rs:6994-7622`, `7828-9350`) —
  this is the whole point of routing containers through real descriptors:
  they stop being special cases.
- **`vix/tests/` (the green guardrail)**: behavior must stay identical —
  same handles, same content hashes for the same logical values, same corpus
  results. Since `content_hash` for `Array`/`Option`/`Map`/`Tree` currently
  comes from bespoke hashers (`hash_map_pairs`, `hash_handle_list`, ad hoc
  `Sha256` calls in `alloc_option_*`/`alloc_array_words`) rather than
  `hash_value_into`, switching them onto descriptor-driven hashing **will
  change their content hashes** unless the new descriptor-driven hash is
  deliberately made bit-for-bit compatible with the old bespoke one (same
  domain-separation bytes, same field order). That compatibility is very
  unlikely to fall out for free — treat every one of these four container
  kinds as its own migration with an explicit "hash changed, is that OK"
  checkpoint, not a silent refactor. (This is somewhat moot if V2's Blake3
  rework lands first/alongside, since *all* content hashes change then
  anyway — sequencing question below.)
- **lower.rs**: the closures that build `MapPair`/array words/pending
  invocations by hand (the ~57 host-op table, V4/V7 territory) are the
  callers of the doomed `alloc_*` functions; each one needs its call site
  updated when its target function's signature changes shape (e.g. "build
  words vec" → "build via declared descriptor + RawArrayBuilder"). This is
  the largest count of touch points, though each is mechanical once the new
  `Descriptor` shapes exist.
- **No phon changes** (off-limits, and none needed — vix only reuses the
  `Descriptor<SchemaRef>` *type*, already true).
- **No weavy API changes required.** Everything in §4's recommendation uses
  types that already exist in `weavy::mem::{declared, runtime}`. If ABI (a)
  (facet bridge) is ever built, that's new weavy (or vix) code, additive, not
  a change to existing weavy surface.

## 8. Incremental, corpus-green sequence

Each step independently testable against `vix/tests/`, in an order that
isolates hash-changing steps from pure-refactor steps:

1. **Alignment-honoring store allocation** (§4.2/§5 last bullet): swap
   `vec![0u8; size]` for a `Layout`-aware allocation at the 8 call sites.
   Pure internal representation change, same bytes, same hashes. Corpus
   should be a no-op; if it isn't, that's a real bug surfaced, not a
   regression from the change itself.
2. **`Option<T>` onto a real `Descriptor`** (declared two-variant enum).
   Smallest of the four container kinds, touches the fewest call sites
   (`alloc_option_none/some`, `option_payload`). Decide up front whether the
   new content hash must match the old bespoke `Sha256` one bit-for-bit (if
   corpus fixtures pin hashes) or whether the corpus tolerates a hash bump
   (if it only checks structural equality/round-tripping). Land, run full
   corpus, confirm before moving on.
3. **`Array` onto `Descriptor` + `RawArrayBuffer`/`RawArrayBuilder`**
   (`alloc_array_words`/`array_entry`). Same hash-compatibility decision.
   This is the one most likely to interact with flesh
   (`FleshValue::ArrayWords`), so do it before touching flesh's own storage.
4. **`Map<K,V>`** — same pattern, larger blast radius (`map_get`,
   `canonical_map_pairs`, `hash_map_pairs` all involved).
5. **`Tree`/`Pending<T>`** — same pattern; lowest priority since these are
   closer to vix-specific exec/demand plumbing than general container shapes,
   do them once the pattern is proven on 2-4.
6. **Flesh onto `ScratchSession`/`RawArrayBuilder`** (§4, bullet 1): purely
   an allocation-strategy change under `FleshStore`; no schema/hash
   implications at all since flesh values aren't hashed until they intern
   (doc 90's whole point) — safe to do any time, but doing it *after* 2-5
   means flesh's `FleshValue` variants can mirror whatever shape the frozen
   store settled on for arrays/maps, instead of needing a second migration.
7. **Remove the now-dead bespoke codec functions** listed in §5, once nothing
   calls them.
8. **(Only if/when forced) ABI (a) bridge**: `facet_shape_to_descriptor`.
   Not sequenced here because nothing in the current corpus needs it — do
   this when a concrete Rust-native type actually needs to cross the value
   store boundary without a vix-side redeclaration, and scope it then against
   real requirements (in particular the `#[repr(Rust)]` enum discriminant
   problem from §1, which will dictate how much of `Access::Enum` can be
   `Tag::Direct` versus needs `Tag::Thunk`).

## 9. Risks

- **Hash compatibility, per §7**: the single biggest correctness risk. Four
  container kinds currently hash via bespoke domain-separated `Sha256` calls
  outside `hash_value_into`; moving them onto descriptor-driven hashing is
  not guaranteed to reproduce the same bytes. If any corpus fixture encodes
  an expected content hash literally (rather than checking behavior), it will
  break, correctly, and needs a deliberate call on whether that's acceptable
  now or should wait to bundle with V2's Blake3 migration (which invalidates
  *all* hashes anyway, arguably a better time to also fix container hashing
  so there's exactly one hash-format break instead of two).
- **Frame-word vs. descriptor-byte width mismatch**: today, record/enum
  fields in the *frame* (host-call argument passing) are always read/written
  8 bytes at a time (`read_frame_word`/`write_frame_word`, `i*8` throughout),
  while the *descriptor*'s field size can be `<8` (`write_alloc_fields`
  asserts `field_size <= 8` then copies only that many bytes into the
  store's `bytes`). This split is intentional and already resolved by Amos's
  R1 (Open Decision #1 in the inventory doc: the frame is weavy's declared-
  record calling convention, separate from the store's byte layout) — flagged
  here only so the container migration doesn't accidentally conflate "how
  many bytes a field occupies in the frame" with "how many bytes it occupies
  in the store."
- **`RawArrayBuilder`/`RawArrayBuffer` are `unsafe`-heavy raw-pointer APIs**
  (`weavy/src/mem/runtime.rs:172-409`); adopting them into `driver.rs` means
  vix's store code inherits real invariants to uphold (initialize before
  `mark_initialized`, `adopt()` before drop if ownership transferred) that
  `Vec<u8>` never asked of it. Not a reason not to do it, but the migration
  should be reviewed with that in mind, not treated as a mechanical rename.
- **`#[repr(Rust)]` enums block ABI (a) enum support** (§1) — if ABI (a) is
  ever needed for a type with a plain Rust enum inside it, expect `Tag::Thunk`
  (facet vtable calls) rather than `Tag::Direct`, which is slower and more
  code than the declared-ABI path enjoys today. Worth knowing before anyone
  assumes "just bridge facet in" is a small job.
- **Sequencing against V2 (Blake3/incremental hashing) and V4 (host-op
  registry)**: this doc's container migration touches many of the same
  `lower.rs` host-op closures V4 wants to re-register, and every hash call
  site V2 wants to move to Blake3. Doing all three independently risks
  three separate breaking migrations touching overlapping code; doing them
  together risks an unreviewable diff. Recommend sequencing V1's container
  migration (steps 2-5 above) **before** V2's Blake3 swap, so hash-shape
  changes land once per container kind instead of twice.

## 10. Open questions for Amos

1. For the four container kinds (§3), is bit-for-bit content-hash
   compatibility with the *current* bespoke hashers a hard requirement during
   migration, or does the corpus only check structural/behavioral equality
   (making a hash bump acceptable mid-migration, before V2 lands anyway)? This
   gates whether steps 2-5 in §8 can be done independently of V2 or must wait
   to be bundled with it.
2. Should `Array`/`Map`/`Option<T>`/`Tree`/`Pending<T>` get *real*
   `SchemaRef`-generic descriptors registered once (parametric over element
   schema, the way `phon_schema::SchemaKind::List{element}`/`Map{key,value}`
   model it), or per-instantiation concrete descriptors keyed by e.g.
   `"Array<Int>"` the way builtins are keyed today (`descriptor_for_type`
   currently produces one concrete `Descriptor` per named type, no generics)?
   This is really a V3/V6 question (`SchemaRef` design) bleeding into V1's
   scope because the container shapes are generic and today's `String`-keyed
   `HashMap<String, Descriptor<String>>` has no notion of type parameters at
   all.
3. Is there an actual near-term need for ABI (a) (facet bridge), or should it
   stay an intentionally-empty slot in the design until something concrete
   forces it? Nothing in the current corpus or inventory doc names a
   candidate Rust type that needs to cross the boundary — worth confirming
   before spending effort on `facet_shape_to_descriptor` speculatively.
4. `descriptor_word_schema`'s string-prefix dispatch (§5) is small but is
   exactly the V6 pattern this whole redesign exists to kill — worth a
   one-line ticket under V3/V6 rather than silently perpetuating it while
   fixing the container-ABI issue next to it?
