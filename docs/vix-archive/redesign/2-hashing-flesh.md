+++
title = "Redesign · Hashing + flesh (the intern / perf axis)"
+++

# Hashing + flesh

Verdict **V2** (Blake3, weavy builtin or lowered IR, field-walk only where
padding/handles force it) plus the flesh chapter of the inventory (subsystem 2 +
3). These two are one axis, not two: flesh exists *because* interning is
expensive (measured, not assumed — `rodin-in-vix.md`, stax-profiled
2026-07-05), and the fix to "expensive" is a property of the hashing scheme, not
of flesh's copy/COW machinery. Get the hash scheme right and flesh's job gets
simpler; get it wrong and no amount of COW cleverness saves you (this was
already proven once — see below).

## How it works today (grounded)

**The store.** `ValueStore { entries: Vec<StoreEntry>, by_content:
HashMap<(String, ContentHash), i64> }` (`driver.rs:738–741`), `StoreEntry {
schema: String, bytes, content_hash, taint }` (`driver.rs:716–721`),
`ContentHash = [u8; 32]` (`driver.rs:613`). Every `alloc*` method computes a
hash and dedupes through `by_content` before pushing a new entry — this part is
sound and stays.

**The hashing.** `use sha2::{Digest, Sha256}` (`driver.rs:36`) is used at ~10
independent call sites, each hand-rolling `Sha256::new()` /
`.update(domain_str)` / `.update(fields...)` / `.finalize()`:
`hash_value_bytes`/`hash_value_into` (`9174–9247`, the general
Descriptor-walking hasher), `alloc_raw_tainted` (`1022`), `alloc_option_none`
(`1160`), `alloc_option_some` (`1191`), `alloc_array_words` (`1289`),
`hash_map_pairs` (`8649`), `hash_with_taint`/`hash_taint_into`/`combine_taints`
(`7828–7880`), `canonical_word_hash_in_store` (`7953`), `hash_handle_list`
(`8804`), `hash_concrete_tree` (`8878`). Ten-plus independent `Sha256::new()`
sites sharing one pattern (domain-separator string + typed field writes) is
itself a symptom: there is no single hashing module, so "swap the algorithm" or
"make hashing incremental" today means editing ten call sites by hand instead
of one.

`hash_value_into` (`driver.rs:9184–9247`) is the general case: given a
`Descriptor<String>` and a byte range, it **unconditionally walks**:
`Access::Record` → hash `b"record"` then recurse per field
(`9206–9213`); `Access::Enum` → hash the selector then recurse per active
variant's fields (`9214–9228`); `Access::Array` (fixed-count inline array) →
recurse per element (`9229–9240`); `Access::Handle` → look up
`store.entry(handle)` and hash **its cached `content_hash`**, not the raw
handle int (`9198–9205`, correctly implementing V2's handle rule already);
`Access::Scalar` → hash the raw bytes directly (`9197`, already the POD-flat
fast path — just not extended to Record/Array).

**Flesh.** `FleshStore { entries: Vec<FleshEntry> }`, `FleshEntry { refs:
usize, value: FleshValue }`, `FleshValue::{Record, Map, ArrayWords, Interned}`
(`driver.rs:813–838`). Negative `i64` = flesh handle (`flesh_word`/`flesh_index`,
`873–879`); positive = store handle. `intern_flesh_word`
(`1439–1524`) is the freeze operation: if already `Interned`, return the cached
store handle (O(1) — the *leaf* level already memoizes correctly); otherwise
recurse into `intern_value_bytes_children` (`1536–1574`, replaces each
field/element flesh-word with its interned store handle **in place** in the
byte buffer) and then calls `store.alloc`/`alloc_map`/`alloc_array_words` to
actually hash and dedupe the now-fully-interned aggregate, caching the result
as `FleshValue::Interned(handle)` (`1522`).

`array_push` (`driver.rs:4282–4345`) shows the design working *as intended* at
the leaf level: it interns only the **newly pushed element**
(`intern_flesh_word(..., value)`, `4298` / `4326`) — O(1) if the element is
already a store handle — and the **array container itself stays flesh**
(`FleshValue::ArrayWords`, un-interned) whether the push takes the reuse path
(`refs == 1`, mutate `words` in place, same flesh index, `4307–4315`) or the
COW path (clone `words`, new flesh index, `4335–4338`). So the "don't intern
in the hot loop" discipline is *already correctly implemented* for the array's
own identity — pushing ten times does not intern ten times.

**Where it breaks: the container hash has no memory.** When that array
*does* eventually cross an intern boundary (`FLESH_INTERN_HOST`, host id 43,
`driver.rs:155`, dispatched through the `flesh_intern` closure at
`4251–4280` — invoked at a demand boundary, see below), `alloc_array_words`
(`1272–1307`) computes the container's own hash **from scratch**: domain
string, elem schema, length, then `for word in &words { hasher.update(
canonical_word_hash_in_store(...)) }` (`1290–1298`) — one `update()` per
element, *every single time this array is interned*, regardless of how many of
those elements were already hashed the previous time a same-shaped array of
length k−1 was interned. `canonical_word_hash_in_store` (`7953–7964`) gives
O(1) access to each *element's own* cached hash — that memoization is real —
but the *aggregate*'s hash has no analogous cache. `hash_map_pairs`
(`8649–8668`) and `hash_handle_list` (`8804–8820`) have the identical shape.

**Why this is O(N²) in practice.** `intern_flesh_word`'s own leaf-level
memoization (`1456–1458`) is keyed on the flesh *entry index*, not on the
array's logical contents. A COW push allocates a **new** flesh index for the
grown array (`4336–4338`); even a reuse push keeps the same index but the
*previous* interned hash (if there ever was one) is not attached to that index
in any form the next `alloc_array_words` call can reuse. So: intern a
monotonically-growing array at lengths 1, 2, …, N (once per CDCL trail step, in
the Rodin-in-vix spike) and total hashing work is `1 + 2 + … + N = O(N²)`
`hasher.update()` calls, even though no individual element is ever rehashed at
the leaf. Measured: **86% of solver CPU was `sha2::compress256`** inside
`intern_flesh_word → intern_value_bytes_children → alloc_array_words`
(`rodin-in-vix.md`, stax profile, 2026-07-05); the COW-vs-reuse *copy*
optimization (flesh's headline feature) moved the needle **5%** (14.6ms vs.
15.3ms) because copying was never the cost — full-tree rehash was. This is the
single most important fact grounding this whole document: **flesh's COW tier
already does its job; the thing that needed fixing was never in flesh, it's in
the aggregate hash function's lack of incrementality.**

**The demand boundary that forces interning.** `memo_key`
(`driver.rs:4732–4740`) is the actual forcing function: it computes
`canonical_word_hash(schema, word)` (`4958–4961` → `canonical_word_hash_in_store`,
`7953`) for every INVOKE argument, and that function only finds a cached hash
via `store.entry(word)` — a flesh handle is not in `ValueStore` at all, so it
falls through to `canonical_scalar_hash`, i.e. it would hash the *flesh index*
as if it were scalar data, silently wrong. In practice lowering must intern any
flesh-resident argument before an INVOKE crosses this boundary. So "intern at
the demand boundary" is not aspirational — it's already how `memo_key` is
wired. The O(N²) profile result says: **when the demand boundary is crossed
once per solve step** (one INVOKE per CDCL decision, carrying the whole
growing trail as an argument — which is how the granularity discussion in
`rodin-in-vix.md` Part 2 frames "solve interior is flesh, demand sits at the
fact level"), the boundary is correctly placed but each crossing's hash cost is
still O(current length). Fewer crossings would reduce the *count*; incremental
hashing reduces the *cost per crossing*. Both matter; only one is a machine
concern (see (d)).

## What is FORCED vs. what is gratuitous

**Forced:**
- Handles must hash by referent content hash, not raw int (V2) — already
  correct (`9198–9205`).
- Padding bytes must not enter the hash (equal value ⇒ equal hash) — this
  requires *some* proof of which bytes are padding; it cannot be inferred from
  raw bytes alone.
- Map content-identity must be insertion-order-independent (a `Map` is a set of
  pairs, not a sequence) — canonicalization (today: `canonical_map_pairs`,
  sorting) is forced regardless of hash algorithm.
- The demand boundary (INVOKE / `memo_key`) must see fully-interned arguments —
  forced by `canonical_word_hash_in_store` only knowing about `ValueStore`
  entries, not flesh.

**Gratuitous (this is what we fix):**
- SHA-256 instead of Blake3 (V2(1)) — cosmetic in isolation, load-bearing for
  the vx-store alignment and for making incrementality free (below).
- Ten independent inline `Sha256::new()`/`.finalize()` call sites instead of one
  hashing module — pure duplication, makes every future change (algorithm,
  incrementality, domain strings) an N-site edit.
- `hash_value_into`'s **unconditional field-walk** for `Access::Record`/`Enum`/
  `Array` even when the value is POD-flat (scalar fields, no handles, no
  padding gaps) — the walk is only forced when padding or handle fields are
  actually present, and weavy *already proves* which bytes are padding
  (`RecordAccess::byte_ownership: RecordByteOwnership`, `weavy/src/mem.rs:288–296`,
  with `is_padding_range` at `mem.rs:414` — unused by `hash_value_into` today).
- The aggregate hash having **no memory between intern crossings** — this is
  the actual O(N²) source, and it is a data-structure gap, not an algorithm
  gap: even Blake3 dropped in as a 1:1 SHA-256 replacement at today's call
  sites is *still* O(N) per intern, just with a smaller constant.

---

## (a) SHA-256 → Blake3 swap: what changes, what breaks

**Mechanical scope.** `blake3 = "^1"` is **already a workspace dependency**
(root `Cargo.toml:392`), used today by `phon-schema` (`identity.rs`, schema
content-identity), `vox-types`/`vox-schema` (method/schema identity), `dibs`
(trigger identity). `vix/Cargo.toml:24` only has `sha2.workspace = true` —
adding `blake3.workspace = true` to `vix/Cargo.toml` is the only manifest
change. `weavy/Cargo.toml` has **zero** non-optional deps beside `copypatch`;
if hashing moves to weavy (see (c)) it becomes weavy's first hard dependency —
worth a beat of hesitation, but `blake3` is a small, no_std-friendly, widely
vendored crate and matches vx-store's own choice, so it's a reasonable one to
take. `ContentHash = [u8; 32]` is unchanged (Blake3's default output is also 32
bytes); every `.finalize().into()` becomes `.finalize().as_bytes()`-shaped
(`blake3::Hash: AsRef<[u8; 32]>` / `Into<[u8; 32]>`), and `blake3::Hasher` has
the same `new()`/`update()`/`finalize()` shape as `sha2::Sha256`, so each of
the ten call sites is a near-mechanical `s/Sha256/blake3::Hasher/` — **if** they
stay ten independent sites. Centralizing first (see below) means doing this
swap exactly once.

**What actually breaks.** Grepped the corpus (`tests/`) for hardcoded
`content_hash`/hex-hash literals: **none exist.** Every hash-sensitive
assertion in the corpus is *differential* — `assert_eq!(ab_entry.content_hash,
ba_entry.content_hash, ...)` (`lower.rs:7783`), `assert_eq!(lr_entry.content_hash,
rl_entry.content_hash, ...)` (`lower.rs:9791`), `fn_hash` equality checks
(`lower.rs:9081,9448,9599,9600` — these hash *lowered function bodies* via a
separate `hash_u64`/`DefaultHasher` path, untouched by this change), and one
length check (`lower.rs:6946`: `identity_hash.len() == 64` — a **hex string**
length, 32 bytes either way, survives unchanged). **Blast radius on the test
corpus: zero.** The swap is safe to land as a pure refactor, corpus-green by
construction, *provided* the domain-separation strings and field order stay
byte-identical (so the hash of a given value is deterministic and stable
across a process, which is all any test checks — nothing in-repo checks
cross-process/cross-version stability yet, because there is no persistence
layer yet; see (e)).

**Why it's not "just" cosmetic.** `vx-store` (`vixenware/vixen`) addresses
chunks and nodes by Blake3 (`chunks/blake3/{xx}/{hash}`,
`nodes/blake3/{xx}/{hash}`, `crates/vx-store/src/storage.rs:1–5,31–47`;
`Blake3Hash`/`ChunkHash`/`NodeHash`/`ContentHash` types,
`crates/vx-services/src/store.rs:77–85,157–213`). An **earlier** analysis
(`vixenware/vixen/docs/design/vx-store-as-vix-memo.md`, "Hard questions for
Amos" §1) recommended the *opposite* of V2 — keep vix on SHA-256 and make
`vx-store` grow multi-algorithm addressing. **That recommendation is
superseded**: the redesign inventory's V2 ruling is explicit that vix moves to
Blake3 *because of* vx-store, not in spite of it — "a vix value IS a vx-store
object by digest" (the thesis of that same doc, `vx-store-as-vix-memo.md:3–7`)
only holds if the digest families actually match. Worth flagging explicitly
since it's a reversal of a standing written recommendation, not a fresh
decision — Amos should know the old doc is stale on this one point (rest of
it — the persistence-trait design in "The design: one persistent memo
substrate" — stands, see (e)).

---

## (b) Incremental / Merkle hashing over the aggregate representation

This is the actual O(N²) fix; Blake3 alone is a constant-factor win (part (a)),
this is the asymptotic one.

**The core idea: don't recompute, extend.** The container hash must become a
function that can be **updated by one element in O(1)-amortized-to-O(log N)
work**, not recomputed by rehashing all N cached child hashes. Two shapes,
because arrays and maps have different canonicalization requirements:

**Arrays — Blake3's own incremental hasher is the mechanism, no new data
structure needed.** An array's content identity is already, correctly, order-
dependent (arrays are sequences) — which is exactly what a streaming hash
wants. Concretely: attach an `Option<blake3::Hasher>` (the hash-so-far,
already seeded with the domain string + elem schema) to `FleshValue::
ArrayWords` alongside `words: Vec<i64>`. On `array_push`
(`driver.rs:4282–4345`), after interning the pushed element (unchanged), feed
its cached content hash into the carried hasher — O(1) amortized (Blake3
chunks input in 1024-byte blocks and maintains an internal CV stack of depth
O(log N); appending 32 bytes at a time either fills the current chunk buffer
(no compression call) or triggers one compression call plus, occasionally, an
O(log N) carry-merge up the stack — there is no path that touches all N prior
bytes). At intern time (`alloc_array_words`), **finalize the carried hasher**
instead of rebuilding one from `words` — turns the O(N) rehash into an O(1)
finalize of state that was built incrementally over the array's whole
lifetime. On the COW push path (`refs != 1`, `4318–4338`), clone the hasher
alongside the `words` clone that already happens there — `blake3::Hasher::
clone` is documented as cheap (it duplicates the CV stack + partial chunk
buffer, both O(log N)-ish, **not** the consumed input), so this rides along
with the existing O(N) words-clone (the "5%" cost) without adding a new
asymptotic tier. `hash_handle_list` (Array-of-pending, Tree-merge — same
domain+per-item-hash shape, `8804–8820`) gets the identical treatment for
free once it goes through the same carried-hasher path.

**Critical correctness invariant — this must hold or content-addressing
breaks:** the finalized hash of an array built via N incremental pushes must
be **bit-identical** to the hash of the same final array built via one bulk
call. This holds *because* Blake3 (like SHA-256) is a pure function of the
byte stream fed to it, independent of how many `update()` calls delivered
those bytes — chunking history is invisible in the output. This is a
**testable, corpus-addable invariant** (same shape as the existing A/B
ordering-invariance tests, `lower.rs:7783`/`9791`): build one array via N
pushes, another via a single bulk alloc with the same N elements, assert equal
`content_hash`. Add this test *before* touching `alloc_array_words`'s
internals, so it's red-then-green evidence the incremental version is
observably identical to today's.

**Maps — genuinely harder, and likely out of scope for the first cut.** A
map's content identity must be **insertion-order-independent** (it's a set of
pairs), so there is no "append at the end" structure to stream into — a
naive persistent BST keyed by insertion order would make the hash depend on
tree shape, which depends on insertion history, which breaks "equal value ⇒
equal hash." A correct incremental map hash needs a structure whose **shape is
a pure function of the key set**, not of insertion order — e.g. a Merkle trie
keyed by bits of the (already-canonical) key content-hash (a Merkle-Patricia/
HAMT shape), where inserting one pair touches O(log N) trie nodes and each
touched node's hash is `Blake3(children...)`. This is real, self-contained
machinery someone has to design and build — it does not fall out of "use
Blake3" the way the array case does. **Recommendation: don't build it in this
pass.** The measured hot path (`rodin-in-vix.md`) is a CDCL trail — an
*array*, not a map. Land the array fix, leave `hash_map_pairs`
(`8649–8668`) hashing from scratch exactly as today, and revisit only if a
future stax profile shows map-heavy interning as a bottleneck ("measure,
don't guess" is the doc's own doctrine, restated verbatim in
`rodin-in-vix.md`'s closing line). Flag this explicitly as an open question
for Amos (below) rather than silently scoping it out.

**Records — not part of the O(N²) story, minor optimization only.** A
record's field count is static (declared by its descriptor), so hashing one is
already O(fields), bounded by the *type's* shape, not by solver-loop growth. No
incremental structure needed; only the POD-flat fast path (part (c)) applies.

---

## (c) Where hashing should live: weavy builtin, not lowered IR — and why

**The two things V2 is choosing between are not actually in tension once you
separate two different hashing *use sites*:**

1. **Store-internal content-addressing** (what this whole document has been
   about: `ValueStore`/`FleshStore` computing a `content_hash` as a side effect
   of `alloc`/intern). This is never invoked by vix *user* code — it's the
   store's own invariant enforcement, called from Rust (`driver.rs`) on every
   allocation. There is no vix-level "hash of a value" expression triggering
   it; it fires whether or not any vix program asks for it.
2. **A user-facing "content hash of this value" builtin**, if/when vix
   programs need to *compute and use* a hash as a value (e.g. a memoization
   key exposed to user code, a `contenthash(x)` stdlib function). No evidence
   this exists yet in the corpus — but V2's phrasing ("earn one... or lowered
   weavy IR") anticipates it.

**For (1): a weavy-owned Rust function, not lowered IR, and not a `HostCall`.**
This is forced by V1/V3, independent of V2: weavy owns `Descriptor<SchemaRef>`
and the layout/ABI (`mem.rs:230–284`), and `hash_value_into` already walks that
exact type (just instantiated `Descriptor<String>` — confirmed, `driver.rs:39`
imports the real `weavy::mem::{Access, Descriptor, Tag}`, this is not a vix-
local copy). A function that walks `Descriptor<SchemaRef>::access` to decide
POD-flat-vs-walk and to find handle targets is squarely weavy's domain — it's
the same kind of code as `weavy::mem::declared`'s existing accessors, and
critically, **the padding proof it needs already exists there and is unused**:
`RecordAccess::byte_ownership: RecordByteOwnership` with `is_padding_range`
(`weavy/src/mem.rs:288–296,410–414`) is *exactly* the "prove a gap is padding"
primitive V2's POD-flat rule needs, and weavy even has a working example of
consuming it for a related purpose — `group_record_scalars`
(`weavy/src/mem.rs:1900–1930`) already groups adjacent scalar fields into one
wire-copy op *when the byte-ownership proof says the gap between them is
padding* — same predicate, different consumer (wire compaction, not hashing).
**Concretely add:** `weavy::hash` (or similar), exposing something like
`fn hash_declared<S>(hasher: &mut blake3::Hasher, descriptor: &Descriptor<S>,
bytes: &[u8], handle_hash: impl Fn(&S, i64) -> [u8; 32])` — the `handle_hash`
callback is how vix's `ValueStore` lookup gets threaded through without weavy
knowing what a "store" is. Internally: for `Access::Record`, check
`byte_ownership` — if every field is `Scalar`/fixed-`Array`-of-`Scalar` with no
`Handle` anywhere in its subtree AND all inter-field gaps are proven padding,
hash the **whole byte range in one `update()` call**; otherwise walk fields
(today's behavior, unconditionally). This is not new schema infrastructure —
it's consuming a proof weavy already computes at descriptor-build time, once,
never per-hash.

Why NOT lowered IR for (1): it would mean emitting `Op`s that execute inside a
`Task` (interpreted or JIT'd) for something that fires on *every* store
allocation, including ones with no vix-visible call site (a `Ret` returning an
aggregate, a `Call` argument copy that happens to be a fresh record). Lowered
IR is the right tool for things a vix *program* does; the store's own
identity bookkeeping is not vix-program behavior, it's substrate behavior — it
belongs at the same layer as `mem::declared`'s layout computation, i.e. Rust,
in weavy, called directly (no `HostCall` dispatch indirection) from
`ValueStore`/`FleshStore`.

**For (2), if/when it's needed:** *that* is the case for "lowered weavy IR" —
a user-facing `contenthash(x)` would lower to ordinary IR that reads `x`'s
declared fields through the normal typed accessors and calls a **primitive**
host op per leaf (`Op::HostCall { host: BLAKE3_UPDATE_HOST }` or similar, sitting
in the V4 capability registry, not a bespoke reflective closure). That
primitive can and should be the *same* `weavy::hash` module's leaf-level
building blocks, just driven by generated code instead of by `ValueStore`
directly. Nothing forces building this now — flag as future work, not part of
this pass's scope, since no current corpus test exercises it.

---

## (d) How flesh must interact — and does the current design support it?

**Yes, mostly — the discipline is already right, the missing piece is the
hash-carrier, not a flesh redesign.** Restating what's grounded above:

- Flesh already defers container interning correctly: `array_push` interns
  only the pushed element, never the whole array, on both the reuse and COW
  paths (`4282–4345`). No change needed here.
- The demand boundary (`memo_key`, `4732`) already forces full interning of
  INVOKE arguments — this is correct and necessary (a memo key must be a
  content hash of something with stable identity; a flesh handle is
  process-and-moment-local and cannot be a memo key). No change needed here
  either.
- **What's missing is state, not discipline:** `FleshValue::ArrayWords` needs
  to carry the incremental hasher (part (b)) so that *when* an intern boundary
  is crossed — however many times the algorithm's own structure crosses it —
  each crossing costs O(1)/finalize instead of O(current length). This is an
  **additive** change to `FleshValue::ArrayWords`'s shape
  (`words: Vec<i64>` → `words: Vec<i64>, hasher: blake3::Hasher` or a small
  wrapper type), not a redesign of the flesh/store split, the refcount tier,
  or the negative-handle encoding (all already judged sound and un-rotten in
  the inventory's subsystem-2 correction).
- **One thing worth checking, not assumed:** does anything in the current
  `array_alloc`/`array_map_pending` paths (`2962–3030`) build a *fresh* array
  from a batch of already-known elements (bulk construction, not incremental
  push)? If so the carried hasher there is seeded once and fed the whole batch
  in one pass — same code path as today's `alloc_array_words` domain-string
  setup, just writing into the carried hasher instead of a throwaway one. No
  special-casing needed; "start empty, get pushed to N times" and "start with
  N elements already known" both terminate in the same finalize call.

**Whether the *granularity* of intern crossings (once per CDCL step vs. once
per solve) needs fixing is a lowering/algorithm question, not a machine
question**, and it's explicitly out of scope for "fix flesh" — the machine's
job is to make each crossing cheap; how often the Rodin-in-vix port crosses it
is a property of how that port structures its INVOKE calls, tracked
separately (`rodin-in-vix.md`'s own Spike A/B plan). Don't conflate the two
when reporting "fixed" — a stax re-profile after landing (b) should show the
SHA-256/Blake3 compression function no longer dominating CPU **even at the
current crossing frequency**; if it still dominates, that's a signal the
crossing count itself also needs work, and that's a separate, follow-up
finding, not evidence this fix was wrong.

---

## (e) The vx-store relationship

Already covered under (a) for the *digest* question (Blake3, settled by V2,
reversing the older `vx-store-as-vix-memo.md` §"Hard questions for Amos" #1).
Two things beyond that, both **out of scope for this pass**, flagged because
they're adjacent enough to be confused with it:

- `vx-store` addresses two object *classes* — raw chunks (`blake3(bytes)`,
  FastCDC-chunked for large blobs) and PHON-encoded `CasNode`s (`blake3
  (node_bytes)`, Merkle directories). Neither is "hash a `Descriptor`-shaped
  in-memory value" — that mapping (a vix `StoreEntry` → a `CasNode`/chunk
  shape) is the actual bridge work `vx-store-as-vix-memo.md`'s "one persistent
  memo substrate" section designs (`PersistentValueStore` trait, `vix.value.
  v1`/`vix.memo.v1`/... namespaces). That design still stands; this document
  only unblocks its digest-compatibility precondition. Building the bridge
  itself is separate, larger, proprietary-adjacent work.
- Content-defined chunking (FastCDC) is a **large-blob** technique (vx-store
  uses it for multi-chunk blobs, 4MB+ objects) — it has nothing to do with the
  in-memory aggregate-hashing problem this document addresses (arrays of
  handles measured in the tens-to-thousands of elements, not megabyte blobs).
  Don't reach for CDC here; the incremental Blake3 hasher in (b) is the right
  tool at this scale.

## (f) A better name for "flesh"

The inventory already narrowed this to *molten* / *draft* / *wip* / *scratch*
and asked Amos to pick (open decision #2). One elimination worth making before
he does: **rule out *scratch*** — `weavy::mem::runtime::RawScratch` already
exists (per the inventory's own "grounded" section) and means something
adjacent-but-different (raw uninitialized memory for construction, not
mutable-refcounted-pre-intern aggregate staging). Reusing the word in
`driver.rs` for a different tier would be a real collision, not just a style
nit — someone reading "scratch" in a stack trace spanning both crates would
have to disambiguate by crate, every time.

Between the remaining three: *molten* is the strongest fit, because the
existing code and the inventory doc **already use "freeze" as the verb**
("before they freeze into the content-addressed store," inventory
subsystem 2) — molten is the natural pre-freeze noun, and it's not a generic
English word doing double duty elsewhere in the codebase the way "draft" and
"wip" are (both are used informally all over software for unrelated things;
neither ties to the freeze metaphor already in use). If Amos wants a fourth
option: *unfrozen* is the most literal (adjective, not noun — `UnfrozenStore`,
`UnfrozenValue`) and has zero collision risk, at the cost of being a slightly
awkward type-name prefix (`UnfrozenValue::Record` reads fine; `unfrozen.alloc()`
reads a little odd compared to `molten.alloc()`). Recommend *molten*; *unfrozen*
as the fallback if "molten" feels too cute for a systems codebase.

---

## Blast radius

- **Corpus:** zero hardcoded hash literals found in `tests/`; every hash
  assertion is differential or a length check that survives a same-width
  algorithm swap (confirmed by grep, see (a)). The array incremental-hash
  invariant (bulk == incremental) is a **new** test to add, not a corpus risk.
- **Call sites touched:** ~10 inline `Sha256::new()` sites in `driver.rs`
  (centralize into one module first, then swap algorithm once) +
  `FleshValue::ArrayWords`'s shape (additive field) + `array_push`/
  `array_alloc`/`alloc_array_words` (route through carried hasher instead of
  fresh one) + `hash_handle_list` (same treatment, optional/could ship later).
- **Not touched:** `hash_map_pairs` (deferred per (b)), `hash_value_into`'s
  Record/Enum/Array walk logic itself (only gains a POD-flat short-circuit,
  doesn't change for the walked case), the flesh refcount/COW tier, the
  negative-handle encoding, `memo_key`/the demand-boundary placement.
- **New dependency surface:** `blake3` added to `vix/Cargo.toml` (already a
  workspace dep elsewhere) and, if hashing centralizes into weavy per (c),
  weavy's first non-optional dependency.

## Incremental sequence (corpus green at every step)

1. **Centralize hashing into one module** (in `driver.rs` first, or directly
   in weavy if (c) is decided up front) with zero behavior change — same
   SHA-256, same domain strings, same field order, just no longer ten
   hand-rolled call sites. Corpus must be byte-for-byte identical (every
   `content_hash` produced today must still be produced) — this step is
   pure refactor, verifiable by an exhaustive "hash every existing corpus
   fixture before/after, assert equal" run, not just `cargo nextest run`.
2. **Swap SHA-256 → Blake3** at that one now-central site. Corpus assertions
   are differential (confirmed above) so this should be green with no test
   edits; if anything red surfaces, it's evidence of an undiscovered
   hardcoded-hash dependency, not a design flaw — investigate, don't route
   around.
3. **Add the POD-flat fast path** to the Record/Array walk, consuming
   `RecordAccess::byte_ownership::is_padding_range` (weavy, already exists).
   Pure perf, zero observable change — add a test asserting a POD-flat
   struct's one-shot-range hash equals its per-field-walk hash (the two
   must agree; that's the correctness bar for adding the fast path at all).
4. **Add the bulk-vs-incremental array-hash invariant test** (build via N
   pushes vs. one bulk call, assert equal `content_hash`) — write this
   **before** step 5, so it's red-then-green evidence, not a test written to
   match whatever the implementation happens to produce.
5. **Give `FleshValue::ArrayWords` a carried `blake3::Hasher`**, route
   `array_push`/`array_alloc`/`alloc_array_words` through it, finalize instead
   of rebuild at intern time. This is the O(N²)→O(N)-ish fix. Re-run the stax
   profile from `rodin-in-vix.md` on the same workload — expect
   `sha2::compress256`/`blake3` compression to drop out of the top-CPU
   position; if it doesn't, the crossing-frequency question (part (d)'s
   closing point) needs to be reopened, not this step redone.
6. **(Optional, only if profiling later shows it matters)** design and add
   the Merkle-map structure for `hash_map_pairs`. Not scheduled by this
   document; flagged as future work in the open questions below.

## Risks / open questions for Amos

1. **Is `blake3::Hasher`'s per-append cost actually O(1) amortized at this
   element size (32-byte content hashes as leaves)?** The reasoning above
   (1024-byte internal chunking, O(log N) CV stack) is from Blake3's public
   design, not from a benchmark run against vix's actual access pattern —
   worth a microbenchmark on the real leaf size before committing, same
   "measure don't guess" doctrine the earlier flesh spike learned the hard way.
2. **Map hashing is explicitly deferred** (part (b)) — confirm that's
   acceptable, i.e. that the measured hot path really is array-shaped (CDCL
   trail) and not also map-shaped somewhere else in the Rodin port that hasn't
   been profiled yet.
3. **Naming:** *molten* recommended, *unfrozen* as fallback, *scratch* ruled
   out (collision with `weavy::mem::runtime::RawScratch`) — final call is
   Amos's per the inventory's own open-decision framing.
4. **Whether weavy should take `blake3` as its first hard dependency** (part
   (c)) — the alternative is keeping the centralized hash module in vix and
   having it merely *consume* weavy's `Descriptor`/`byte_ownership` types
   (which requires no new weavy dependency) rather than *living* in weavy.
   Functionally equivalent either way; it's a "where does this code
   physically live" call, not a behavior call — recommend weavy (V1/V3
   already put `Descriptor` ownership there, and a future user-facing
   `contenthash()` builtin per (c)'s option 2 would want the same leaf
   primitives available to lowered IR, which argues for weavy), but this is
   genuinely Amos's to place.
5. **The intern-crossing *frequency*** (once per CDCL step) is called out
   repeatedly above as out of scope for the machine fix — confirm Amos agrees
   that's a lowering/Rodin-port concern to track under the Spike A/B plan in
   `rodin-in-vix.md`, not something this document's sequence should also
   attempt, so "done" here isn't silently redefined to include it later.
