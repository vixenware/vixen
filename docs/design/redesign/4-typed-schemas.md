+++
title = "Redesign · Typed schemas + newtypes (kill String/i64 everywhere)"
+++

Scope: V3 (`Descriptor<String>` → `Descriptor<SchemaRef>`) and V6 (the master
disease — `String`/`u64`/`i64` standing in for every kind of identity the
machine has: schema, handle, function, closure). Read-only research; no source
touched. Every claim below is cited `file:line` against
`vix/src/machine/driver.rs`, `vix/src/machine/lower.rs`, `vix/src/module.rs`,
`phon/rust/phon-schema/src/schema.rs`, `phon/rust/phon-ir/src/descriptor.rs`,
`weavy/src/mem.rs`.

---

## 0. The one-sentence shape of the fix

weavy's `Descriptor<SchemaRef>`, `Access<SchemaRef>` (`weavy/src/mem.rs:230,
248`) are **already generic** — vix instantiated the generic parameter with
`String` instead of a real schema-ref type
(`weavy::mem::{Access, Descriptor}` imported at `driver.rs:39`, then always
written `Descriptor<String>`, e.g. `driver.rs:990, 1582, 6992`). phon already
ships the correct instantiation and never touches vix's code:
`phon-ir/src/descriptor.rs:32` — `pub type Descriptor =
weavy::mem::Descriptor<SchemaRef>`. So the fix at the type level is close to a
**substitution**, not a redesign: swap the generic argument, then work through
everywhere that substitution doesn't type-check because the code was actually
doing string comparison instead of structural matching. That second part is
where the real work and the real risk live.

---

## (a) `schema: String` → `SchemaRef`

### What `SchemaRef` actually is

From `phon-schema/src/schema.rs`:

- `SchemaId(pub u64)` (`schema.rs:18`) — "the first 8 bytes of the BLAKE3 hash
  of the schema's canonical structural encoding." Content-derived, not
  allocation-order. Exactly one `i64`/`u64` frame word wide — this matters
  below (§ frame-word budget).
- `Schema { id: SchemaId, type_params: Vec<String>, kind: SchemaKind }`
  (`schema.rs:38`) — the full schema record kept in a registry somewhere,
  looked up by id.
- `SchemaKind` (`schema.rs:49`) — a real ADT: `Primitive(Primitive)`,
  `Struct{name,fields}`, `Enum{name,variants}`, `Tuple{elements}`,
  `List{element}`, `Set{element}`, `Map{key,value}`, `Array{element,
  dimensions}`, `Tensor`, `Option{element}`, `Channel`, `Dynamic`,
  `External{kind, metadata}`.
- `SchemaRef` (`schema.rs:119`) — `Concrete{id: SchemaId, args: Vec<SchemaRef>}`
  or `Var{name}` (generic type-parameter reference). This is the thing
  `Descriptor<SchemaRef>` is generic over — a *reference* to a schema, cheap to
  clone/compare (small enum, no string formatting), carrying its own
  type-argument list for generics instead of baking them into a formatted name.
- `Primitive` (`schema.rs:150`) is the ADT `Bool`/`I64`/`F64`/`String`/... with
  a `tag()`/`from_tag()` round-trip to a canonical string **used only for the
  identity hash's canonical encoding**, never for runtime dispatch.

`phon-ir/src/descriptor.rs` re-exports weavy's whole descriptor vocabulary
specialized at `SchemaRef` (`Descriptor`, `Access`, `RecordAccess`,
`EnumAccess`, ... all `= weavy::mem::X<SchemaRef>`, `descriptor.rs:32–70`).
`weavy::mem::Access<SchemaRef>` (`mem.rs:248`) already has a
`Handle { target: SchemaRef }` variant (`mem.rs:263–265`) — an immutable
value-store handle whose target schema is known **statically**. vix already
uses this variant (`driver.rs:7008, 7619, 7914, 9198`, `descriptor.access`
matched against `Access::Handle { target }` throughout) — it's just that
`target` is a `String` today instead of a `SchemaRef`.

### The current vix instantiation, concretely

- `Driver.descriptors: HashMap<String, Descriptor<String>>` (`driver.rs:1582`,
  same shape in `ModuleTables.descriptors`, `module.rs:36`) — the descriptor
  registry, keyed by schema **name string**. ~60 function signatures in
  `driver.rs` take `descriptors: &HashMap<String, Descriptor<String>>` as a
  parameter (grep count).
- `Driver.schema_refs: Vec<String>` (`driver.rs:1583`) plus
  `intern_schema_ref` (`driver.rs:1719–1727`) — a **hand-rolled interning
  table**: linear `position()` scan over a `Vec<String>` to turn a name into a
  dense `i64` index (the frame word actually carried as `type_ref` in
  `STORE_ALLOC`, `driver.rs:2468`), and `schema_name_for` (`driver.rs:8682`)
  to turn the index back into a name for every store op (14+ call sites:
  `driver.rs:1356, 2470, 2669, 2689, 2693, 2701, 2802, 2806, 2904, 2966, 3294,
  3420, 3555, 3966, 4255, 4487, 8624, 8626`). Every one of these round-trips
  `i64 → String` and then uses the string to index `descriptors` again.
- **The interning order must currently be reproduced by hand.** On reload,
  `Machine::load_modules_with_lane` re-derives the schema name list and
  replays `intern_schema_ref` calls, then asserts the resulting index matches
  the compiler's own `schema_refs[name]` (`lower.rs:118–125`):
  ```rust
  // The driver's own interning must reproduce the fresh 0-based assignment.
  for name in &c.schema_names {
      let actual = driver.intern_schema_ref(name.clone());
      assert_eq!(actual, c.schema_refs[name], "schema ref assignment is deterministic");
  }
  ```
  This is a real correctness hazard load-bearing on iteration-order discipline
  between two independent name lists — exactly what a **content-derived** id
  removes by construction (two processes hashing the same canonical encoding
  agree without coordinating assignment order at all).
- **String dispatch, not structural matching**, at every leaf that needs to
  know *what kind* of schema it's looking at:
  - `Access::Scalar if descriptor.schema == "Float"` (`driver.rs:6999, 9193`) —
    Float needs special canonicalization (NaN handling) at the memo boundary;
    the schema-kind check is a string equality instead of matching
    `Primitive::F64`.
  - `descriptor.schema.starts_with("Int"/"Float"/"Bool")` (`driver.rs:7620–
    7622`) inside the rendering/reflection path — deriving primitive kind by
    string prefix where `SchemaKind::Primitive(Primitive::I64/F64/Bool)` is a
    three-way `match`.
  - `is_projectable_schema` (`driver.rs:4893–4901`): `schema == "Tree" ||
    schema == "Array" || schema == "Doc" || schema == "Blob" ||
    schema.starts_with("Map<") || pending_value_schema(schema).is_some() ||
    self.descriptors.contains_key(schema)` — seven-way dispatch by string
    literal/prefix/parse, deciding whether a value is "the kind of thing whose
    read-set can be tracked field-granular."
  - `canonicalize_return_word`: `if self.fns[fn_ref].return_schema ==
    "Float"` (`driver.rs:4964`).
  - `hash_u64(&schema)` fed into `DriveEvent::StoreAlloc { schema_ref: hash_u64(&schema),
    .. }` (`driver.rs:2547`) — hashing the schema **name string** as a
    stand-in schema identity for trace events. This is literally reinventing
    `SchemaId` (content-derived hash) by hand, except it hashes the *name*
    (an arbitrary, renamable label) rather than the *canonical structural
    encoding* phon-schema's `SchemaId` is defined over — two schemas with the
    same name but different shape (impossible today because names are unique
    per module, but the coupling is accidental, not structural) would collide;
    a genuine structural rename would falsely change the id.
- **Generic type names are built and re-parsed as formatted strings.**
  `module.rs:230–252` (`type_schema_name`) builds names like `"Map<String,Doc>"`
  and `"Tuple<A,B>"` by `format!` at the compiler front door (`module.rs:240,
  249`); `lower.rs` has free functions `option_schema`/`pending_schema`/
  `map_schema` that do the same for `Option<T>`, `Pending<T>`, `Map<K,V>`
  (`driver.rs:8690–8700`, `lower.rs` call sites), and the **runtime** then
  re-derives structure by `starts_with("Map<")` (`driver.rs:4898`) and
  `strip_prefix("Pending<").strip_suffix('>')` (`pending_value_schema`,
  `driver.rs:8698–8700`). This is exactly what `SchemaKind::{Tuple, List, Map,
  Option}` (real ADT nodes with a `SchemaRef`/`Vec<SchemaRef>` payload,
  `schema.rs:59–71, 82–84`) and `SchemaRef::Concrete { id, args }`
  (`schema.rs:120`, generic arguments as a real `Vec<SchemaRef>`, not
  comma-joined into a name) already model. No string formatting or parsing
  should exist anywhere in this path once the swap lands.
- **`Tree`/`Array`/`Blob` have no `Descriptor` entry at all** —
  `add_builtin_descriptors` (`lower.rs:1895–1948`) registers `Bool`, `Sealed`,
  `Target`, `Os`, `Arch`, `Run`, `Doc`, but never `Tree`/`Array`/`Blob`; they're
  handled as opaque host-managed store citizens (`ArrayEntry::Words {
  elem_schema, words }`, `driver.rs:804–810`; `TreeEntry`, `driver.rs:881–886`)
  reached purely through the string-literal dispatch above. `Array` is
  actually a homogeneous sequence with a real element schema
  (`elem_schema: String` right there in `ArrayEntry`) — it should be
  `SchemaKind::List { element }`, which phon-schema already has
  (`schema.rs:62–64`), not a magic string. `Tree` (an exec-result DAG) and
  possibly `Blob` are genuinely opaque, non-Rust-shaped host values — that's
  exactly what `SchemaKind::External { kind, metadata }` (`schema.rs:92–96`)
  already exists for. Confirm `Blob`'s actual shape during Stage 3 (§d) before
  deciding `External` vs `Primitive::Bytes`.

### Where name lookup is still needed — and how, without string dispatch

Type **names** are a real, permanent part of the surface: vix source code
refers to types by identifier (`struct Foo`, `Foo::Bar`), and diagnostics need
to show the human name back to the user. Both are legitimate, but both belong
at the **compiler front door**, not the runtime:

1. **Source → schema resolution** (`module.rs`, at parse/lower time): keep a
   `HashMap<String, SchemaId>` (or richer, a name→`Schema` table) **local to
   the compiler**, used exactly once per identifier occurrence to resolve
   `ast::Type` → `SchemaId`/`SchemaRef`. This replaces `type_schema_name`'s
   `String` return with a resolved `SchemaRef`; the `format!("{base}<{}>",
   ...)` composition (`module.rs:240`) becomes constructing
   `SchemaRef::Concrete { id, args }` instead of a formatted label.
2. **Diagnostics** (error messages, the `RenderedValue`/reflection path slated
   for deletion under V10): keep the name as a **display-only** side table
   (`SchemaId → String` for pretty-printing), consulted only when formatting
   output for a human, never for control flow. `Schema.kind`'s `Struct{name,
   ..}`/`Enum{name, ..}` variants (`schema.rs:52,56`) already carry the
   declared name for exactly this purpose — no separate table may even be
   needed.
3. Everywhere else — the store, the frame-word encoding, the projectable-arg
   check, the Float-canonicalization check, the render/compare/hash paths —
   **never touches a name again.** They match on `SchemaKind`/`Primitive` or
   compare `SchemaId`s.

---

## (b) Newtypes to add

### Handle: kill the `i64` sign convention

Handles are `i64` words where `< 0` means "flesh" (unfrozen, mutable staging)
and `>= 0` means "store" (frozen, content-addressed): `flesh_word(index) = -1
- index`, `flesh_index(word) = (word < 0).then(|| -1 - word)`
(`driver.rs:873–879`). This convention is **used correctly and consistently**
today (the redesign doc's V-verdict on flesh explicitly says the negative
encoding is not the rot — it's a measured-necessary perf tier, doc §2). The
newtype problem is that the convention lives entirely in the *reader's head*:
every one of dozens of call sites does `if flesh_index(handle).is_some() {
... } else { store.entry(handle) }` (e.g. `driver.rs:2508, 2586, 2771,
4287, 4352, 4405, 4500`) by hand, and nothing in a function signature says
"this parameter is a tagged handle, not a plain integer."

Add:
```rust
struct FleshHandle(usize);   // index into FleshStore.entries
struct StoreHandle(usize);   // index into ValueStore.entries — note: unsigned
enum Handle { Flesh(FleshHandle), Store(StoreHandle) }
impl Handle {
    fn from_word(word: i64) -> Self { ... }  // the existing flesh_word/flesh_index logic, in one place
    fn to_word(self) -> i64 { ... }
}
```
`Handle::from_word`/`to_word` become the **only** place the sign convention is
encoded; every `if flesh_index(...).is_some()` branch becomes a `match
Handle::from_word(word) { Handle::Flesh(h) => ..., Handle::Store(h) => ... }`,
checked exhaustively by the compiler instead of by convention. The frame-word
wire representation (`i64`, negative-tagged) is **unchanged** — this is a
Rust-API-level typing fix, not a wire/ABI change, so it doesn't touch V7's
territory (frame ABI) at all.

**A `StoreHandle` newtype already exists** (`driver.rs:78`:
`pub struct StoreHandle(pub i64)`) — but it does not do this job. It's used
only in `lower.rs`'s public `Machine` facade (`call`, `intern_arg`,
`export_value`, `import_value`, `pending_alloc`, `intern_tree`,
`lower.rs:207–344`), and every one of those sites **immediately unwraps it**
into a raw `i64` to call into `Driver`, whose internals (`ValueStore`,
`FleshStore`, all of `driver.rs`) never see the type at all — they take and
return bare `i64`/`usize`. Worse, because it wraps the *word* (`i64`), not a
resolved handle, a `StoreHandle` can silently hold a flesh-space (negative)
value — e.g. `lower.rs:320: let handle = StoreHandle(handle);` right after a
call that may return either kind. So today's `StoreHandle` is a thin
public-API label around the exact same overloaded sign convention; it doesn't
prevent the flesh/store confusion it looks like it should. The fix folds it
into the `Handle`/`FleshHandle`/`StoreHandle` scheme above and threads it
**through the driver internals**, not just the facade.

### FnRef: a fourth, currently-conflated index space

`fn_ref: usize` (`driver.rs:1912` and pervasive, e.g. `demand(&mut self,
fn_ref: usize, args: Vec<i64>)`, `driver.rs:2015`) indexes `Driver.fns:
Vec<LoweredFn>`. This is a **different index space** from:
- weavy's own `FnId` (`LoweredFn.task_fn: FnId`, `driver.rs:203`) — index into
  the *task program*'s functions, already typed by weavy.
- the schema-ref `i64` index into `schema_refs: Vec<String>` (§a).
- `Handle` (flesh/store index space, above).

All four are bare `i64`/`usize` today and appear side-by-side in the same
frame-word arithmetic and the same function signatures, so a transposition
bug (passing a schema ref where a `fn_ref` is expected, say) type-checks.
Add `struct FnRef(usize)` for the `Driver.fns` index and thread it through
`demand`, `spawn`, `memo_key`, `pending_for_fn`, `fn_ops`, `fn_hash`,
`semantic_comparator_len`, `canonicalize_return_word`, and the ~30 other
`fn_ref: usize` parameters in `driver.rs`.

### SchemaId: reuse phon-schema's, don't reinvent

Once `Descriptor<SchemaRef>` lands (§a), `schema_refs: Vec<String>` +
`intern_schema_ref` + `schema_name_for` (§a) collapse into: the frame word
already carrying a `SchemaId` (or a process-local dense index that maps to
one — see § frame-word budget). No new type needed here beyond what phon
already ships; the newtype work is entirely *removing* the vix-local
reinvention (`schema_refs`/`intern_schema_ref`/`schema_name_for`), not adding
one.

### ClosureHash / content-hash fields: the other `u64`/`Vec<u8>` offenders

V6 calls out `hash: u64` by name. Concretely:
- `LoweredFn.hash: u64` (`driver.rs:201`, doc comment: "Closure hash: the
  memo key's function component") and `CodeRef.closure_hash: u64`
  (`driver.rs:61`) — same concept, two independent fields, no shared type.
- `DriveEvent` trace variants: `Demanded { fn_hash: u64 }`,
  `SpawnedInvocation { fn_hash: u64, key_hash: u64 }` etc. (`driver.rs:240–
  257`) — again bare `u64`, no way to tell a closure hash from a memo-key hash
  from a content hash by the type alone; a copy-paste bug swapping two `u64`
  arguments type-checks silently.
- `ContentHash = [u8; 32]` (`driver.rs:613`) is a **type alias**, not a
  newtype — it's already a fixed-size array (better than the `u64`/`Vec<u8>`
  cases) but has no `Debug`/`Display`/hex-string convention of its own (unlike
  phon-schema's `SchemaId`, which has both, `schema.rs:20–30`), and it's
  inconsistently paired with `StoreValue.content_hash: Vec<u8>`,
  `CodeRef.module_hash: Vec<u8>` (`driver.rs:54, 60`) — the *wire/export*
  form uses `Vec<u8>` where the *internal* form uses `[u8; 32]`, so the
  boundary conversion is implicit and unchecked (a truncated or
  wrong-length `Vec<u8>` from an imported bundle is a runtime panic waiting to
  happen, not a type error).

Propose: `struct ClosureHash(u64)` for the closure-hash family (replaces
`LoweredFn.hash`, `CodeRef.closure_hash`, the `fn_hash` trace fields), and
promote `ContentHash` from a type alias to a real newtype (`struct
ContentHash([u8; 32])`) with `Display`/hex conversions mirroring
`SchemaId`'s, used consistently at both the internal and wire (`StoreValue`,
`CodeRef`) boundary instead of switching to `Vec<u8>` at the edge. This is
lower-risk, more mechanical work than §a/§(Handle/FnRef) — it doesn't block
anything else and can land whenever, but it's the same disease.

### Out of scope here (V7's territory, noted only for the boundary)

`store_alloc_region`/`store_read_region`/`store_tag_region`/`primitive_region`/
`invoke_region: u32` (`driver.rs:216–226`) are frame **byte offsets** — also
bare integers, also namable (`struct FrameOffset(u32)`), but they're part of
the frame ABI V7 already owns (`path_with_ext`-style raw frame pokes are V7's
specific target). Not proposing to touch them in this problem's pass; flagged
so the newtype sweep doesn't accidentally wander into V7's scope.

---

## (c) What to remove

- `Driver.descriptors: HashMap<String, Descriptor<String>>` and
  `ModuleTables.descriptors: HashMap<String, Descriptor<String>>`
  (`driver.rs:1582`, `module.rs:36`) — replaced by `HashMap<SchemaId,
  Descriptor<SchemaRef>>` (or a `Vec` if ids are kept dense locally, see §d).
- `Driver.schema_refs: Vec<String>`, `intern_schema_ref`, `schema_ref_map_for`
  (`driver.rs:1583, 1719–1734`), `schema_name_for` (`driver.rs:8682–8688`), and
  every one of its 14+ call sites — gone once the frame word carries a real
  id and nothing needs the name back at runtime.
- The reload `assert_eq!` order-reproduction dance (`lower.rs:118–125`) —
  content-derived ids don't need two independent lists to agree on assignment
  order.
- String-formatting/parsing helpers: `option_schema`, `pending_schema`,
  `pending_value_schema` (`driver.rs:8690–8700`), `type_schema_name`'s
  `format!` composition for generics (`module.rs:240, 249`), and any sibling
  `map_schema`-style helper in `lower.rs`. Replaced by `SchemaRef::Concrete {
  id, args }` and matching on `SchemaKind`.
- String-equality/prefix dispatch: `descriptor.schema == "Float"`
  (`driver.rs:6999, 9193, 4964`), `.starts_with("Int"/"Float"/"Bool")`
  (`driver.rs:7620–7622`), `is_projectable_schema`'s seven-way string check
  (`driver.rs:4893–4901`), `receiver.schema != "Array"` (`lower.rs:3532` etc.,
  and the whole family of `schema == "Array"/"Tree"/"Doc"` checks in
  `lower.rs`, e.g. `lower.rs:2828, 3496–3521`). Replaced by matching
  `descriptor.access` (`Access::Scalar` with `Primitive` kind, `Access::Handle
  { target }` compared as `SchemaId`, etc.) or `schema_kind ==
  SchemaKind::List { .. }` and friends.
- `hash_u64(&schema)` as a pseudo schema-id (`driver.rs:2547`) — once the
  `SchemaId` is already sitting in the frame word, the trace event carries it
  directly; no re-hash of a name needed.
- **Not removed, orthogonal**: `PendingPrimitiveKind::to_word/from_word`
  packing three parser kinds into disjoint `i64` ranges via `+ 2_000`
  (`driver.rs:783–800`) is the same "magic int convention" pattern but belongs
  to V5's projections refactor (elf/ast/oci parsers move to Snark or vix), not
  this problem. Noted so it isn't half-fixed here and half there.

---

## (d) Blast radius and a tractable incremental order

**Why foundational:** the redesign doc is explicit — "while every field is
`String`/`i64`, the store's target form can't even be *stated*" (doc §1,
"Should [OPEN]"). Nothing about the store's indexability (V9) or the memo/
read-set's typed surface can be designed on top of a string-keyed descriptor
map; this has to go first among the store-adjacent work.

**Why sprawling:** `Descriptor<String>` appears in ~60 function signatures in
`driver.rs` alone (grep count above), plus `module.rs` and `lower.rs`; schema
string-equality dispatch is scattered across ~25 call sites in `driver.rs` and
a comparable number in `lower.rs`'s method/operator dispatch. The type swap
itself (`Descriptor<String>` → `Descriptor<SchemaRef>`) is a **whole-crate**
change — Rust won't let it compile in a partially-migrated state, since every
signature that takes `&HashMap<String, Descriptor<String>>` has to change in
the same commit as its callers. That's the one stage that can't be split
further; everything else can and should be staged around it.

Proposed order, each stage landing as its own commit(s), full corpus
(`vix/tests/`) run at the end of each stage:

1. **Add `phon-schema` as a vix dependency; no behavior change.** Confirm the
   crate compiles standalone against vix's `Cargo.toml` (it's `#![no_std]`-ish,
   reflection-free per its own doc comment, `schema.rs:7`, so this should be
   friction-free). No driver/lower code touched yet.

2. **`Handle`/`FleshHandle`/`StoreHandle` newtype, wire format unchanged.**
   Mechanical: introduce the enum + `from_word`/`to_word`, replace
   `flesh_word`/`flesh_index` call sites one at a time with the typed match.
   Zero semantic change — the corpus should be green throughout since the
   `i64` encoding on the wire is untouched; this is pure Rust-side typing.
   Lowest risk, good first PR to build confidence the newtype style doesn't
   fight the frame-word plumbing.

3. **`FnRef` newtype for `Driver.fns` index.** Same shape as (2): mechanical,
   no behavior change, corpus green throughout.

4. **Build the compile-time `Schema`/`SchemaId` model in `module.rs`.**
   Replace `type_schema_name`'s `String` return with a resolved `SchemaRef`
   (§a "where name lookup is still needed", step 1); introduce the
   name→`SchemaId` table local to the compiler front door. Decide the
   `Pending<T>` representation here (see § open questions — this is the one
   real design fork in the whole plan). This stage can be built and tested
   in isolation against `module.rs`'s own tests before touching `driver.rs`
   at all, since `ModuleTables.descriptors` isn't consumed by the driver until
   stage 5.

5. **The whole-crate type substitution: `Descriptor<String>` →
   `Descriptor<SchemaRef>` (phon-ir's type).** This is the stage that must
   land in one shot per Rust's compilation model. Scope it as tightly as
   possible: this stage changes **types only**, not logic — every
   `descriptor.schema == "..."` string check that doesn't yet compile against
   `SchemaRef` gets a `todo!()`/temporary `.to_string()`-shim comparison
   *marked for stage 6*, not fixed inline, so this stage stays a pure
   signature-and-plumbing change reviewable as one unit. Run the full corpus
   once at the end, not per-function (it can't compile in between anyway).

6. **Kill the string-dispatch call sites**, one call site (or tight cluster)
   at a time, replacing each `== "Float"`/`starts_with`/`is_projectable_schema`
   check with a match on `SchemaKind`/`Primitive`/`SchemaId` equality. This
   stage *does* change behavior-adjacent code paths (even though the
   observable behavior should be identical), so it's the one to go slowest
   on and check the corpus after each cluster, not just at the end.

7. **Remove the `schema_refs`/`intern_schema_ref`/`schema_name_for`
   machinery and the reload `assert_eq!` dance.** Only possible once stage 6
   has removed every consumer of the interned-name round trip.

8. **(Independent, any time)** `ClosureHash`/`ContentHash`-as-newtype pass —
   doesn't block or get blocked by 1–7, can be its own small side-track.

---

## (e) Risks and open questions for Amos

- **`Pending<T>` has no home in `SchemaKind`.** Every other generic wrapper
  vix invented at the string layer (`Option<T>`, `Map<K,V>`, tuples) maps
  cleanly onto an existing `SchemaKind` variant. `Pending<T>` does not — it's
  a vix/demand-machine-specific "this value may not be realized yet" wrapper,
  and phon is off-limits (we take the type, we don't extend it). Two ways to
  represent it, both compatible with "phon-schema is off-limits":
  1. A small vix-local wrapper type that sits *alongside* `SchemaRef` (e.g.
     `enum VixSchemaRef { Value(SchemaRef), Pending(SchemaId) }`) used
     everywhere vix's own descriptor/schema plumbing needs it, with
     `SchemaRef` still used unwrapped for the store/hash/render paths that
     don't care about pending-ness.
  2. Realization/pending-ness moves **out of the schema entirely** into a
     side channel on the `Handle` (recall `Handle::Store`/`Flesh` above) —
     i.e. "pending" becomes a property of *how a value is currently reached*,
     not *what shape it has*. This reads more consistent with V8 ("no domain
     concepts in the schema") since "pending" isn't a value shape so much as
     a scheduling state, but it's a bigger structural change to the
     store/flesh handle space than this problem's scope, and touches how
     `OptionPayload`/`Realization` (`driver.rs:901–938`) already encode
     readiness — worth understanding that existing mechanism fully before
     picking. **This is the one real fork in the plan; needs Amos's call
     before stage 4 (§d) starts**, since the compile-time `Schema` model
     built there has to commit to one shape.
- **`Blob`'s real shape is unconfirmed.** It has no `Descriptor` entry
  (`lower.rs:1895–1948` doesn't register it) and appears only in string
  dispatch (`driver.rs:4897`, `4184`, `lower.rs:775`). Before deciding
  `SchemaKind::External` vs `Primitive::Bytes` for it (§a), trace its actual
  producers/consumers (likely OCI blob content) — not done in this pass.
- **Corpus dependency on internal schema-name strings.** This analysis
  assumes `vix/tests/` exercises the machine only through the public
  `Machine`/vix-source surface, never asserting on internal Rust
  `Descriptor<String>` schema-name strings directly. That assumption should
  be checked (a quick grep of `vix/tests/` for driver-internal identifiers)
  before stage 5 (§d) lands, since if any test *does* reach into internal
  schema strings, those are test-harness code to update mechanically, not
  corpus behavior to preserve — but better to know going in than discover it
  mid-stage.
- **Frame-word budget is fine, not a blocker.** `SchemaId` is `u64`
  (`schema.rs:18`) — exactly one `i64` frame word, same width as today's
  interned index (`type_ref` in `STORE_ALLOC`, `driver.rs:2468`). No frame-ABI
  change forced by this migration; V7 (frame ABI) and this problem are
  orthogonal, confirmed by inspection rather than assumed.
- **Local dense re-indexing may still be worth keeping as a cache, now purely
  an optimization.** Nothing requires frame words to carry the full 64-bit
  `SchemaId` if a process-local `Vec<SchemaId>` (or `IndexSet`) dense-indexing
  cache turns out to matter for some other reason (it doesn't appear to today
  — `intern_schema_ref`'s linear scan is only called at compile/reload
  boundaries, never per-value, so there's no measured pressure to keep it).
  Worth remembering this option exists, but nothing in the current code
  justifies paying for it up front.
- **`StoreValue`/`CodeRef`/`ValueBundle`'s wire (export/import) shapes are
  Facet-derived** (`#[derive(... Facet)]`, `driver.rs:49, 58, 64, 70`) and
  cross a process boundary (the `ValueBundle` export/import path,
  `lower.rs:275–281`). Any newtype introduced for `SchemaId`/`ContentHash`
  used in these wire structs needs its own `Facet` derive or hand-written
  facet impl — check this compiles before committing to the newtype shape in
  stage 8, it's a small but real detail (not a design fork, just an
  implementation tax to budget for).
