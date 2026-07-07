+++
title = "Redesign · Reflection interface (kill the RenderedValue god-enum)"
+++

# Reflection interface — kill `RenderedValue`

Verdict **V10** (corrected): `pub enum RenderedValue` (`driver.rs:345`, `#[repr(u8)]
#[facet(tag = "type")]`, 20 variants) is exactly the god-Value-enum vix exists to
forbid — one materialized enum eagerly representing *any* value, built by walking
the whole tree before anyone can look at a single field. The fix is not a nicer
enum; it's **pull accessors** over the frozen weavy-schema'd store bytes vix
already has. This doc inventories the current push path, designs the pull surface
concretely against `weavy::mem`, and sequences the migration so the corpus stays
green throughout.

## How it works today (grounded)

`RenderedValue` (`driver.rs:345-416`) has 20 variants: `Int, Float, Bool, String,
Path, Flag, Version, VersionSet, Raw, Tuple, Record, Enum, Array, Map, Tree,
TreePending, Pending, Doc, Sealed`, each carrying a payload struct
(`RenderedField` at `304`, `RenderedMapEntry` at `310`, `RenderedTreeEntry` at
`319`, `RenderedPending` at `326`, `RenderedTreePending` at `334`).

It is built by one recursive dispatcher, `render_word` (`driver.rs:7106`), which
switches on the *string* schema name and delegates to:
`render_option` (`7204`), `render_tree` (`7256`), `render_sealed` (`7290`),
`render_array` (`7309`), `render_map` (`7343`), `render_doc` (`7386`),
`render_declared` (`7485`), `render_pending` (`7603`). Every one of these, when it
hits a nested value, calls `render_word` again — so rendering any value walks the
**entire** reachable subtree and allocates a `RenderedValue` node per field, every
`String`/`Vec<u8>`/`format!` cloned along the way. `Machine::render_value` /
`Machine::render_result` (`lower.rs:395-404`) are the only public entry points;
`Machine::render_word` (`driver.rs:1858`) is the driver-level method they call.

Producers of the input this walks: `store.entry(handle)` → `StoreEntry { schema:
String, bytes: Vec<u8>, content_hash, taint }` (`driver.rs:716`), read through
`descriptors: &HashMap<String, Descriptor<String>>` (weavy's `Descriptor`, but
keyed by bare `String` — V3's complaint) plus a **second, ad hoc name table**,
`RenderNames { structs: BTreeMap<String, Vec<String>>, enums: BTreeMap<String,
Vec<RenderVariant>> }` (`driver.rs:419-428`), because `Descriptor<String>` alone
carries offsets and tags but no field/variant *names*. That second table is
telling: it is vix's own reinvention of exactly what `phon_schema::Schema::Struct
{ name, fields: Vec<Field> }` / `Schema::Enum { name, variants: Vec<Variant> }`
already carry (`phon-schema/src/schema.rs:38-97, 272-286`) — a schema stores
names, a `Descriptor` doesn't. This is V10 quietly depending on V3: once vix
carries a real `SchemaId → Schema` registry, `RenderNames` disappears as a
category, not just an implementation.

**Consumers, the actual blast radius:**
- `vix/src/machine/lower.rs` — 29 occurrences, all inside `#[cfg(test)]` unit
  tests (lines `6934, 7010, 11119-11290`), pattern-matching
  `RenderedValue::{Sealed,Tuple,Enum,Record,Map,Doc,Tree,Pending,Array,String,Int,Bool}`.
- `vix/tests/checker_corpus.rs` — `checker_string`/`checker_int` (`367-381`)
  destructure `RenderedValue::String`/`Int` after `checker_rendered` calls
  `machine.render_result` (`393`).
- `vix/tests/real_process.rs:95`, `vix/tests/crate_lock.rs:162-163` — same
  `RenderedValue::String { value }` pattern.
- `vix-wire/tests/wire.rs` — `RenderedValue::Array`/`Path`/`Bool` assertions
  (`170, 420-429`), plus a local `render_bundle(module, schema, bundle) ->
  RenderedValue` helper (`433`) that **constructs** an expected `RenderedValue`
  tree to diff whole values for equality — the one place a full materialized
  snapshot is genuinely wanted (see migration below).
- `vix-daemon/src/lib.rs:332` — `eval_on_actor` (the daemon's real production
  consumer, wired to the IDE/playground over vox RPC): `machine.render_result(...)
  .map(|value| format!("{value:?}"))`, fed into `DemandEvent::Done { generation,
  result: String }`. The doc comment already reads *"the value's Debug form
  (v1)"* — the code itself is flagged provisional. This is the literal site of the
  "~20k lines" tax: a failed workspace resolve materializes the whole tree, then
  `Debug`-prints it into one `String` sent over the wire to `gen_ts.rs`-generated
  TS bindings.

**What's already pull-shaped, underneath the push wrapper.** The render_* fns
don't reimplement value access — they call primitives that are already
single-step and non-recursive: `store.array_entry` (`855`, one array's words or
pending handles), `store.map_pairs` (`1056`), `store.option_payload` (`1204`),
`store.tree_entry` (`1399`), `store.string_value` (`1425`), plus
`descriptor.access` matching, `read_word_at` (`7627`), `read_variant_tag`. These
are exactly the right granularity for pull accessors. **The hard part is already
done**; `RenderedValue` is a redundant recursive-push wrapper glued on top of
already-pull-shaped primitives.

## (a) The pull-accessor API surface

Ride weavy's own `Access<SchemaRef>` discriminant (`weavy/src/mem.rs:248-284`):
`Scalar, Record, Enum, Option, Array, Handle, Tensor, Sequence, Set, Map, Result,
Pointer, Dynamic, Opaque, Recurse`. A `View` is a cursor: (store, a handle or raw
word, a `Descriptor<SchemaRef>` for the value at that position, a name source for
Record/Enum). It never allocates a tree; every method reads only what it's asked
for.

```rust
pub struct View<'a> {
    store: &'a ValueStore,
    schemas: &'a SchemaTable,      // SchemaId -> phon_schema::Schema (names)
    descriptors: &'a DescriptorTable, // SchemaId -> Descriptor<SchemaRef> (layout)
    word: i64,                     // scalar value, or a store/flesh handle
    schema: SchemaRef,
}
```

Generic accessors, one weavy `Access` variant each:

- **Kind probe** — `fn kind(&self) -> ValueKind` (mirrors the `Access`
  discriminant plus the machine-native extensions below), `fn schema(&self) ->
  &SchemaRef`. Zero reads beyond the descriptor already in hand.
- **Scalar** — `fn as_i64/as_u64/as_bool(&self) -> Result<T, String>` read `word`
  directly; `fn as_f64` applies the existing bit-cast (`canonicalize_word_for_schema`).
- **String-like handles** (`Access::Handle{target: "String"}`, covers
  `String/Path/Flag/Template`) — `fn as_str(&self) -> Result<&str, String>`
  borrows `store.entry(word).bytes` as utf8 **without cloning** (today's
  `store.string_value` returns an owned `String` on every call; a pull accessor
  can return `&str` since the store outlives the view).
- **Record/Tuple** (`Access::Record`) — `fn field_count(&self) -> usize`;
  `fn field_name(&self, i: usize) -> Option<&str>` (from `Schema::Struct.fields`,
  `None`/positional for tuples); `fn field(&self, i: usize) -> Result<View, String>`
  computes the offset and wraps — one field materializes, not the record.
- **Enum** (`Access::Enum`) — `fn variant_index(&self) -> u64` (reads the tag per
  `EnumAccess::tag`: `Direct/Niche/Thunk`); `fn variant_name(&self) -> &str` (from
  `Schema::Enum.variants`); `fn variant_field_count`/`variant_field(&self, i)`
  same shape as Record.
- **Option** (`Access::Option`) — `fn is_some(&self) -> bool` (reads `Presence`);
  `fn some(&self) -> Option<View>`.
- **Array** (`Access::Array{element,count,stride}`, fixed-shape) — `fn len(&self)
  -> usize` is `count`, **no read at all**; `fn get(&self, i) -> Option<View>` at
  `base + i*stride`.
- **Sequence** (`Access::Sequence`, dynamic, covers vix's `Array` schema today) —
  `fn len(&self) -> usize` per `SequenceStorage`; `fn get(&self, i) -> Option<View>`.
- **Map/Set** (`Access::Map`/`Access::Set`) — `fn len(&self) -> usize`; `fn get(&self,
  key: &View) -> Option<View>`; `fn entries(&self) -> impl Iterator<Item = (View,
  View)> + '_` — **lazy**, yields one pair at a time; a caller that wants "all
  entries" collects explicitly, at the call site, not inside the accessor.
- **Result/Pointer** (`Access::Result`/`Access::Pointer`) — `is_ok/ok/err`,
  `deref`, same shape.
- **Handle** (`Access::Handle{target}`, generic) — `fn target(&self) -> Result<View,
  String>` follows the handle into the store with the target's descriptor. This
  generalizes what `render_word` special-cases per schema name today
  (`"Path" => store.string_value(...)`, `"Version" => ...`) into one accessor.

**Machine-native extensions (not weavy `Access` variants — vix's own demand
vocabulary, kept narrow and separate, not folded into a bigger enum):**

- **Pending** (today's `Pending`/`TreePending`/negative flesh handles) — `fn
  is_pending(&self) -> bool`; `fn pending_info(&self) -> Option<PendingInfo>`
  where `PendingInfo { closure_hash, identity_hash, remaining_arity, arg_count }`
  is a plain struct, not a `RenderedValue` variant. This is legitimate
  machine-vocabulary (V8 forbids *domain* concepts, not the demand machine's own
  concepts of "not yet resolved").
- **Tainted values** (`Sealed`) — `fn taint(&self) -> Option<&StructuralTaint>`
  always readable (it's metadata, not plaintext); every other accessor
  (`as_str`, `field`, …) checks taint **once, centrally, in the `View`
  constructor** and returns `Err` unless the value is declassified first. This
  is a real (small) behavior fix, not just a refactor — see Risks.
- **Tree/Run** — `fn tree_state(&self) -> TreeState` mirroring today's
  `TreeEntry::{Concrete, Merge, Exec}`, `fn tree_entries(&self) -> impl
  Iterator<Item = (&str, &[u8])>`. Whether `Tree`/`Run` *should* be machine-level
  concepts at all is subsystem 8/9 territory (Vixen-the-build-system leaking into
  the store), out of scope for this doc — flagged, not resolved, below.

## (b) Migrating the tests

The vast majority of call sites are `let RenderedValue::X { fields, .. } =
machine.render_result(name, handle).unwrap() else { panic!() }` — each becomes a
`view` plus direct pulls:

```rust
// before
let RenderedValue::Map { entries, .. } = machine.render_result("mapped", map).unwrap() else { panic!() };
assert!(matches!(&entries[0].key, RenderedValue::String { value } if value == "a"));
assert!(matches!(entries[0].value, RenderedValue::Int { value: 1 }));

// after
let view = machine.view_result("mapped", map).unwrap();
let (key, value) = view.entries().next().unwrap();
assert_eq!(key.as_str().unwrap(), "a");
assert_eq!(value.as_i64().unwrap(), 1);
```

Concretely, by call site:
- **`lower.rs` unit tests** (29 occurrences) migrate in groups matching the
  `render_*` fn they exercise — Sealed (`6934`), Doc/Map nesting (`7010,
  11174-11283`), Tuple/Enum/Record (`11119-11147`), Map (`11156-11164`), Tree
  (`11185`), Pending (`11203`) — each group is a mechanical pattern swap, run
  `cargo nextest run` after each.
- **`checker_corpus.rs`** — `checker_string`/`checker_int` (`367-381`) become
  two-line changes: `checker_view(...)?.as_str()` / `.as_i64()`.
- **`real_process.rs:95`, `crate_lock.rs:162-163`** — same one-line swap.
- **`vix-wire/tests/wire.rs`** — the odd one out: `render_bundle` builds an
  *expected* `RenderedValue` tree to `assert_eq!` a whole value, not just probe
  one field. Pull accessors deliberately make that impossible as a *production*
  operation (that's the point — no whole-tree materialization in the API). The
  fix is a **test-local, recursive deep-equality helper** that walks two `View`s
  in lockstep comparing leaves (`as_i64`, `as_str`, field-by-field, entry-by-entry)
  and short-circuits on the first mismatch — still pull, still no full-tree
  allocation, just callable recursively from test code. This lives in
  `wire.rs`, not in `vix`'s public API; the ban on push-materialization is about
  the machine's surface, not about what a test is allowed to loop over.

## (c) Daemon / UI reconstruction

The real production consumer is `vix-daemon`'s `eval_on_actor`
(`vix-daemon/src/lib.rs:297-338`), feeding the vox-RPC'd `Daemon::eval` service
(`lib.rs:227-234`) that the IDE/playground gets a generated TS client for
(`bin/gen_ts.rs`). Today it renders the *whole* result and `Debug`-formats it into
one `String` — `DemandEvent::Done { result: String }`, explicitly commented
*"the value's Debug form (v1)"* in the source. The "(v1)" is the tell: this was
already known to be provisional.

With pull accessors, the daemon becomes what the rest of the machine already is:
**demand-driven at the UI layer too**. Replace the one-shot render with a lazy
inspect protocol:

```rust
// one RPC per expand-click, not one RPC that pre-expands everything
async fn inspect(&self, handle: ValueHandle) -> InspectNode;

struct InspectNode {
    kind: ValueKind,           // Record | Enum | Array | Map | String | Int | ...
    summary: String,           // e.g. "3 fields", "\"rel\"", "Some", cheap to compute
    children: Vec<(String, ValueHandle)>, // name/index + handle, NOT the values
}
```

The UI (a tree view, matching every other vix debugger surface) requests one
`InspectNode` for the root, renders it collapsed, and issues one more `inspect`
call per node the user expands. A failed workspace resolve then costs exactly
what the user actually looks at, not 20k lines up front — the same
spawn-suppression principle (§ demand scheduler, `driver.rs:1-27`) applied one
layer up, to reflection instead of computation.

For the *simple* cases — `checker_string`/`checker_int`, `vixd`'s one-shot CLI
eval — a full recursive walk is still fine; it's just built as a **formatter on
top of pull accessors** (recursively call `inspect`/`View` methods and write to a
`String`), analogous to how `{:?}` is a `Debug`-impl built on Rust's own
reflection, not a wire type. The distinction that matters: that formatter lives
in test/CLI code, not as a `#[facet(tag = "type")]` enum baked into the daemon's
RPC surface.

Retiring `DemandEvent::Done { result: String }` for a `v2` lazy-inspect RPC is a
real `vox_schema` wire-protocol change with TS-client blast radius
(`gen_ts.rs`) — flagged as in-scope-but-separate in the sequence below, since it's
a decision with UI-facing consequences beyond `vix`/`vix-wire`.

## (d) What to REMOVE, and blast radius

**REMOVE** (once nothing references it):
- `pub enum RenderedValue` (`driver.rs:345-416`).
- `RenderedField` (`304`), `RenderedMapEntry` (`310`), `RenderedTreeEntry`
  (`319`), `RenderedPending` (`326`), `RenderedTreePending` (`334`).
- Free fns `render_word` (`7106`), `render_option` (`7204`), `render_tree`
  (`7256`), `render_sealed` (`7290`), `render_array` (`7309`), `render_map`
  (`7343`), `render_doc` (`7386`), `render_declared` (`7485`), `render_pending`
  (`7603`).
- `Machine::render_word` (`driver.rs:1858`), `Machine::render_value` /
  `Machine::render_result` (`lower.rs:395-404`) — replaced by `view_value` /
  `view_result` returning `View`.
- `RenderNames`/`RenderVariant` (`driver.rs:419-428`) and `render_names_for`
  (`lower.rs:819-876`) — **only** once `SchemaId → Schema` (V3) supplies names;
  see sequencing, this is the one deletion that may need to wait.

**KEEP, reused as the View's implementation** (already the right granularity):
`store.array_entry`, `store.map_pairs`, `store.option_payload`, `store.tree_entry`,
`store.string_value` (generalizes into `as_str`), `read_word_at`,
`read_variant_tag`, `descriptor_word_schema` (`7617`), `tuple_schema_fields` /
`split_top_level_schemas` (`7633+`, until V3's real `SchemaKind::Tuple` replaces
the string parsing).

**Blast radius (call sites touched):**
- `vix/src/machine/lower.rs` — 29 test sites + the two public methods.
- `vix/tests/{checker_corpus.rs, real_process.rs, crate_lock.rs}` — 4 sites.
- `vix-wire/tests/wire.rs` — 6 sites + the `render_bundle` helper rewritten.
- `vix-daemon/src/lib.rs` — `eval_on_actor` (`297-338`), and if the `v2` RPC is
  taken in the same pass, `Daemon::eval`'s schema + `gen_ts.rs` output.

**Adjacent, explicitly NOT in scope:** `crate::value::Value`/`Payload`
(`src/value.rs:8-59`) — a second, ~15-variant tagged enum, but this one belongs to
the *legacy tree-walking interpreter* (`Closure { body: Box<Expr>, env, .. }` —
nothing like it exists in the store/handle model), imported into `driver.rs` only
as a **write-direction** bridge (`alloc_doc_variant` and friends, constructing
Doc values from parsed input — driver.rs:5703-6350ish). It dies naturally with
`oracle.rs`/`engine.rs` at corpus parity (doc's subsystem 9 framing), not as part
of V10. Don't conflate the two enums during migration — they look similar
(tagged, ~20 variants) but run in opposite directions and belong to different
subsystems.

## (e) Incremental sequence (corpus green throughout)

1. **Add `View` and the generic accessors** in `driver.rs`, built directly on the
   existing `store.array_entry`/`map_pairs`/`option_payload`/`tree_entry`/`entry`
   helpers — purely additive, `RenderedValue` untouched, corpus green trivially
   (nothing deleted, nothing changed).
2. **Add the machine-native extensions** (`is_pending`/`pending_info`, `taint`,
   `tree_state`) on top of `View`, same additive step.
3. **Migrate the daemon**: `eval_on_actor` moves off `render_result`/`{:?}` onto
   `View` + a recursive test/CLI-style formatter first (behavior-preserving,
   still one `String` over the wire) — this alone proves the accessor surface is
   sufficient before touching any test. Corpus green (daemon isn't in the vix
   corpus, but its own tests must pass).
4. **Migrate `lower.rs` unit tests**, group by group (Sealed → Tuple/Enum/Record
   → Map → Doc → Tree → Pending → Array), `cargo nextest run` after each group —
   never one big-bang diff across 29 sites.
5. **Migrate `vix/tests/{checker_corpus,real_process,crate_lock}.rs`.**
6. **Migrate `vix-wire/tests/wire.rs`**, replacing `render_bundle`'s whole-value
   equality with the test-local recursive `View`-deep-equality helper.
7. **Delete** `RenderedValue` and all `render_*` fns/methods once `grep -rn
   RenderedValue` across the workspace returns zero hits — that grep is the exit
   criterion, not a judgment call.
8. **Only then, revisit `RenderNames`**: delete it if V3's `SchemaId → Schema`
   registry has landed and can supply names directly to `View`; otherwise leave it
   as the interim name source `View` takes as an explicit parameter (matching
   today's `render_word(schema, word, names)` shape) so V10 isn't blocked on V3's
   own timeline. Don't entangle the two verdicts into one diff.
9. *(Separate, later decision)* **The daemon's `v2` lazy-inspect RPC** — replacing
   `DemandEvent::Done { result: String }` with the `inspect`-per-node protocol
   from (c). Real wire/TS blast radius; do it as its own change once the `View`
   surface has proven itself in step 3-8's simpler "format the whole thing"
   shape.

## Risks / open questions for Amos

1. **V3 coupling on names.** `View`'s Record/Enum accessors need field/variant
   *names* to be useful to a human; today that's only `RenderNames`, an ad hoc
   table that exists because vix chose `Descriptor<String>` over
   `Descriptor<SchemaRef>` + a real `phon_schema::Schema` registry. I've designed
   the API above to accept names as an explicit interim parameter (step 8) so
   V10 doesn't have to wait on V3 landing first — confirm that's the right
   ordering, versus doing V3 first and designing `View` once, against `Schema`,
   rather than possibly twice.
2. **Where machine-native extensions live.** Pending/Tree/Run/Sealed don't map to
   any `weavy::mem::Access` variant. I've proposed a small, explicit, vix-owned
   set of extension methods on `View` rather than routing them through weavy's
   `Access::Opaque`/thunk machinery — because flesh's negative-handle encoding
   and pending-invocation bookkeeping aren't about wire bytes at all, and forcing
   them through weavy's thunk API would be the same category error V2 already
   flagged for hashing (a machine-level concept reflectively walking a
   wire-layout abstraction that wasn't built for it). Confirm before
   implementation — this is the one design choice in this doc that isn't just
   "expose what's already there."
3. **Centralizing the taint check is a small behavior change, not just a
   refactor.** Today `render_word`'s "refuse to render tainted value as
   plaintext" guard runs once, at the top-level dispatch (`driver.rs:7122-7130`)
   — a caller that reaches `render_declared`/`render_array` etc. *directly* for
   an already-tainted handle (rather than through `render_word`'s outer switch)
   isn't necessarily re-checked at that level. Moving the check into `View`'s
   constructor, once, per handle, closes that gap rather than merely preserving
   it. I'd keep this as part of the V10 patch series (the constructor is the
   natural chokepoint) but flag it explicitly so it isn't read as unplanned scope
   creep if a corpus test starts failing for a case that was silently under-taint-checked before.
4. **`Doc`'s relationship to `Access::Dynamic`.** vix's `Doc` schema
   (`Null/Bool/Int/Float/String/Blob/Array/Map/Virtual` — `render_doc`,
   `7386-7483`) looks like a hand-rolled reinvention of weavy's `Access::Dynamic`
   / phon's `SchemaKind::Dynamic`. I've scoped this doc to keep `Doc` as its own
   narrow accessor (mirroring today's `DocPayload`) rather than folding it into a
   generic `Access::Dynamic` walk — that fold is a separate, later cleanup, not
   part of "delete the push enum, expose pull." Confirm that scoping.
5. **The daemon's `v2` wire protocol** is a real decision with IDE/playground
   blast radius (`gen_ts.rs` regenerates TS bindings), not a pure-Rust refactor.
   I've sequenced it last (step 9) and separate from the `View` migration proper
   — confirm that's acceptable, versus wanting the lazy-inspect RPC bundled into
   this same effort.
6. **`vix-wire/tests/wire.rs`'s whole-value golden comparisons** are the one
   place pull accessors push back hardest against "just materialize it, it's a
   test." I've proposed a test-local recursive deep-equality helper (still built
   from pull primitives, just called in a loop) rather than adding any
   `PartialEq`-over-`View` operation to the production API — the latter would
   risk becoming `RenderedValue` again through a side door. Confirm that's the
   right line to draw.
