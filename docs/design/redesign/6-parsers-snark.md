+++
title = "Redesign · Format parsers out of the language (Snark-for-binary)"
+++

Status: analysis only, no code changed. Covers V5 (format parsers are not
machine primitives) and V8 (the machine must not know domain concepts), for
`ELF_DOC_HOST`/`AST_DOC_HOST`/`AST_FN_HOST`/`OCI_DOC_HOST`/`DOC_PARSE_HOST`/
`CRATE_ARCHIVE_HOST` and the `Version`/`VersionSet`/`Cc` domain vocabulary.

## Headline finding, before the detail

**The five parsers are not one problem wearing five hats — they split cleanly
into a case that is already solved and a case that is genuinely hard.**

- `AST_DOC_HOST`/`AST_FN_HOST` (`ast_probe.rs`) parses **vix's own source
  text**. This is not hypothetically Snark-able — it is *already fully
  Snark-backed at runtime, today*: `VixParser::parse` (`vix/src/lib.rs:112`)
  runs `playgrounds/snark/src/bundled/vix/grammar.js` through
  `parse_prepared_weavy_with_report` → a resolved CST → the **build-time
  generated** `ast::lower_source_file` (`vix/src/lib.rs:1-19`,
  `vix/build.rs`, `vix/vix_ast.snark.js`). `ast_probe.rs` doesn't parse
  anything; it calls `VixParser::new().parse(source)` (`ast_probe.rs:46-52`)
  and then hand-walks the *already-Snark-produced* typed AST into a
  `Value::Map`/`Value::Array` summary tree. This is a V10 problem (push
  materialization vs. pull accessors), not a V5 problem — the format-parsing
  work V5 worries about is done.
- `ELF_DOC_HOST` (`elf.rs`) parses a **binary struct format with
  data-dependent pointer-chasing** (section-header table at a computed file
  offset, string tables addressed by index, linked-list version-need records
  walked via `next`/`aux` offset fields). This is a different computational
  shape from anything Snark/tree-sitter does today, and honestly, from
  anything the *tree-sitter technique* was built to do at all — see (a).
- `OCI_DOC_HOST`/`CRATE_ARCHIVE_HOST`/tar sit in between: mostly JSON (a
  genuinely good Snark target) plus tar (a repeated-fixed-record format,
  plausible but scanner-shaped) plus, for crate archives, gzip/DEFLATE
  decompression, which isn't parsing at all.

So "Snark extended to binary formats" is not a single yes/no. It's: **yes and
already-done for AST; maybe for tar; not really for ELF** — and V5's own
fallback order (vix-native, then registered capability) already anticipates
this by not requiring Snark to be the answer for everything.

---

## (a) Can Snark do binary formats?

### What Snark is, concretely

Snark (`/Users/amos/oss/facet-cc/snark`, 36.7k lines) imports Tree-sitter
grammars (`grammar.rs`, `tree_sitter.rs`), validates them into an IR
(`validated.rs`: `ValidatedGrammar`, `GrammarExpr`, `RuleDecl`), derives
lexical facts (`lexical.rs`: `LexicalFacts`, `TerminalFact`, external-scanner
ABI), builds LR/GLR parse tables (`parser.rs`, 9.3k lines: `ParserGrammar`,
`ParseTable`), executes them over source text (`ExternalScanRequest`,
`ResolvedCstNode`), and lowers accepted parses to a **Weavy dialect**
(`lower/weavy.rs`, 16.8k lines: `SnarkWeavyLowered = WeavyLowered<SnarkBlockId,
SnarkIntrinsic>`, built on `weavy::ir::{WeavyOp, IntrinsicOp, ...}` — the same
IR vix itself lowers to). It also carries a query engine compatible with
Tree-sitter's `.scm` query language (`query.rs`, 2.1k lines) used for pull-style
projections (`VixParser::highlights`, `vix/src/lib.rs:86-109`, is a live
example: CST → query → captures, no materialized tree).

This is real, working machinery, not a stub — three languages in this
workspace already ship on it end-to-end: **vix itself** (`vix/build.rs`,
`vix/vix_ast.snark.js`, `vix/src/lib.rs`), **fable** (`fable/build.rs`:
`generate_typed_ast` from `snark_dsl::typed_ast`, `fable/src/lib.rs:3`: "Fable
parses through its snark grammar, lowers the generated typed AST"), and
**gingembre** (`gingembre-snark-spike/`, `gingembre_ast.snark.js`). The
recipe is uniform: a `grammar.js` (Tree-sitter grammar) plus an `X_ast.snark.js`
annotation file (structure/enum naming) drives `build.rs` codegen
(`generate_typed_ast`) into a typed Rust AST, and a runtime parser
(`ParserGrammar`/`ParseTable`/`WeavyParsePlan`) executes the grammar over
real input at runtime, lowering to Weavy along the way.

### The load-bearing evidence: Snark is char/UTF-8-native, not byte-native

This is not a documentation gap or a convention that could flip with a config
flag — it's structural, in the core scan loop and in the type vocabulary:

- `runtime_input.rs:53`: `ByteOffset` is explicitly documented "UTF-8 byte
  offset"; `Utf8ColumnBytes` (`runtime_input.rs:114`) is a column "measured in
  UTF-8 bytes." The coordinate system is UTF-8-shaped by design, not
  incidentally.
- `lex_match.rs` — the pattern/regex matcher that actually recognizes
  terminals — is built entirely on `char`: `.chars()`, `char_indices()`,
  `ch.len_utf8()` appear ~25 times in a 540-line file (`lex_match.rs:162,
  189, 272, 284, 420-537`). Character classes, ranges (`'-'.len_utf8()` at
  `lex_match.rs:499`), and quantifiers are all defined over Unicode
  codepoints extracted from a `&str`.
- `parser.rs:4361`: the scan-request type carries `input: &'a str` — not
  `&[u8]`. `parser.rs:4794-4797`: incremental reparse validates edit
  boundaries with `str::is_char_boundary`. `source.rs:203`: package sources
  are read with `std::fs::read_to_string`, which itself rejects non-UTF-8.
- `scanner.rs:261-279`: the external-scanner host ABI's first operation is
  `ReadLookahead` — documented as "read the current lookahead **codepoint**."
  Every scanner, official or authored, is written against a codepoint cursor.

None of this is fatal to text-shaped binary-adjacent formats (see tar,
below) — codepoints 0–255 map 1:1 onto Latin-1/raw bytes, so a grammar could
in principle be fed a byte string reinterpreted as Latin-1 "characters" and
literal-byte/byte-range terminals would still match correctly. But that's a
workaround, not "the same technique working for binary": every layer above
the terminal matcher (`ByteOffset`/`is_char_boundary`, the query engine's
byte ranges, the CST's positions) still thinks in UTF-8 terms, and — more
importantly — the *scanning model itself* is wrong for what these binary
formats actually need, independent of encoding.

### Why the scanning model, not just the encoding, is the real mismatch

Tree-sitter/Snark's engine is a **forward, single-pass (or bounded-lookahead
GLR) recognizer over a linear token stream**, with a lookahead cursor that
advances, skips "extras," and never jumps backward or to an
arbitrarily-computed absolute position except through the external-scanner
ABI's byte-at-a-time `Advance` (`lexical.rs:229-239`). That is exactly the
right shape for source code: nested, left-to-right, recursive, no
random-access seeks.

`elf.rs` is the opposite shape. Concretely, `parse_sections`
(`elf.rs:312-350`) reads `e_shoff`/`e_shnum`/`e_shentsize` from the fixed
64-byte header, then **computes an absolute file offset** `shoff + index *
entsize` for each of `e_shnum` section headers — a jump, not a scan. Each
section header itself carries `offset`/`size` fields that are absolute
positions into unrelated regions of the file (`elf.rs:339-347`). String
lookups (`cstr`, `elf.rs:368-378`) resolve an offset *found in one section*
into bytes located via a *different, linked* section (`section.link`,
`elf.rs:239-251`). And `needs_glibc` (`elf.rs:139-175`) walks an honest
linked list embedded in the file: each version-need record's `aux`/`next`
fields (u32 byte offsets read via `u32_at`) point to the next record,
followed until a zero terminator. This is pointer-chasing through a flat
buffer using values computed from the buffer's own content — the domain
Kaitai Struct and 010 Editor templates exist for, not the domain tree-sitter
grammars exist for. A CFG production can't express "read a u32 here, then go
parse whatever is at that byte offset, whose length is a u64 you'll read once
you get there" without effectively hosting a general-purpose imperative
interpreter inside the grammar — at which point you've built a second
language, not written a Snark grammar.

**Honest conclusion for (a): Snark cannot reasonably do ELF (or e.g. archive
central-directory formats with the same pointer-chasing shape). V5's framing
— "the same technique should work for binary" — doesn't survive contact with
what `elf.rs` actually does.** Where Snark *does* fit is discussed per-format
below; it's real, just narrower than "binary formats" as a category.

### Per-format feasibility, concretely

- **tar** (`tar.rs`, 74 lines) — a flat repeat of fixed-size 512-byte
  records, each carrying an octal-ASCII size field that determines how far
  to skip before the next record (`walk`, `tar.rs:7-38`). This is the
  closest of the four to something Snark's external-scanner ABI could model:
  a lexical root that reads a 512-byte header token, computes a length from
  a sub-field, and then must consume exactly that many further bytes
  (rounded up) as an opaque "contents" token before the next header token is
  expected. Tree-sitter's external-scanner ABI supports scanner state and a
  byte-at-a-time cursor (`ScannerHostOperation::{ReadLookahead, Advance,
  MarkEnd}`, `lexical.rs:261-279`) — an external scanner *could* be written
  to advance byte-by-byte through the computed-length content, which works
  but starts to reintroduce hand-written host code for the one part that
  actually matters (variable-length-field-driven skipping), which is exactly
  the V5-#3 fallback ("registered Rust capability"), just wearing a Snark
  grammar as a thin shell around it. Net: plausible, but the "declarative,
  debuggable" win V5 wants from Snark is mostly captured by the *fixed*
  512-byte header fields (name/size/typeflag), not by the payload-skipping,
  which stays imperative either way.
- **gzip/DEFLATE** (`crate_archive.rs:35-97`) is not a parsing problem at
  all — it's entropy decoding (Huffman + LZ77 backreferences via
  `miniz_oxide::inflate::decompress_to_vec`, `crate_archive.rs:82-83`).
  There's no grammar to write; a "byte stream in, byte stream out"
  transducer isn't a category tree-sitter/Snark addresses, full stop. This
  stays a plain registered capability (V5-#3) regardless of what happens to
  the rest of the redesign — it's the one case where "never built into the
  language" and "not Snark" are simultaneously and permanently true.
- **OCI** (`oci.rs`) — once you strip out tar (delegated to `super::tar::walk`,
  `oci.rs:115`) and JSON (`crate::data::parse_json`, `oci.rs:222`, already a
  parser the machine calls out to), what's left in `oci.rs` is manifest/config
  digest-chain following across a `Tree` of blob paths
  (`parse_layout`, `oci.rs:90-111`; `layer_bytes`, `oci.rs:289-298`) and
  whiteout-path matching (`whiteout_covers`, `oci.rs:327-339`). That's
  demand-graph / cross-file lookup logic, not within-blob grammar
  recognition — it doesn't need Snark at all, Snark or not. JSON itself
  *is* a clean Snark target (context-free, textual, no pointer-chasing) —
  `playgrounds/snark/src/bundled/json/grammar.js` already exists as a bundled
  grammar — so if `crate::data::parse_json` isn't already Snark-backed, that
  would be the one real "Snark win" inside OCI's dependency chain, orthogonal
  to the OCI/tar-specific code.
- **AST** (`ast_probe.rs`) — already fully Snark, see the headline finding.

### What "the same technique" would honestly require for ELF

If Amos wants ELF (or similarly-shaped formats) declarative and visualizable
the way Snark gives text formats, that's a *different tool* from
tree-sitter's engine — closer to Kaitai Struct's model: declarative
`seq`/`instances`/`types` with `pos`/`size` expressions that can reference
prior fields and perform arbitrary seeks, typically compiled to a
struct-reader rather than a CFG parser. Building that inside Snark's crate
would mean adding a second, unrelated execution model next to the
grammar/parser/lexer one — not extending the existing lexer to consume `u8`.
That's a real, standalone project, not a "detour... we have months" extension
of the current work. Given V5's own preference order already allows for
vix-native (option 2) and registered-capability (option 3) fallbacks, ELF
should honestly be routed to one of those, not held hostage to a Snark
extension that would need to be invented from scratch.

---

## (b) What the current parsers actually do (recap, for what any replacement must cover)

- **`elf.rs`** (444 lines): ELF64/little-endian only header validation
  (`parse_header`, `elf.rs:292-310`); section-header-table walk; 7 named
  projections (`Projection::{Arch,Kind,DynamicDeps,NeedsGlibc,Sections,
  Symbols,LinkerMetadata}`, `elf.rs:20-77`) — arch/kind from `e_machine`/
  `e_type` (`elf.rs:270-289`), `DT_NEEDED` dynamic dependency extraction via
  string-table-linked entries (`dynamic_deps`, `elf.rs:123-137`), highest
  `GLIBC_x.y` symver requirement via version-need linked-list walk
  (`needs_glibc`, `elf.rs:139-175`), section name/size listing
  (`elf.rs:177-195`), symbol-name listing from `.symtab`/`.dynsym`
  (`elf.rs:197-223`), and `.comment`/`.ident` linker-metadata C-strings
  (`elf.rs:225-237`).
- **`oci.rs`** (347 lines): OCI image-layout → `Tree` (walks a tar archive,
  keeping only regular files as UTF-8 text entries, `archive_to_tree`,
  `oci.rs:113-128`); layout parsing that chases `index.json` →
  manifest-by-digest → config-by-digest → layer list (`parse_layout`,
  `oci.rs:90-111`), all via JSON field access helpers
  (`object_field`/`array_field`/`string_field`/`int_field`, `oci.rs:246-274`);
  6 named projections (layers/config/env/entrypoint/cmd/files); a
  layer-order (last-wins) file lookup honoring whiteouts (`project_file`,
  `find_in_layer`, `whiteout_covers`, `oci.rs:159-179, 307-339`); and its own
  input-hash for content addressing (`input_hash`, `oci.rs:181-215`, still
  SHA-256 — same V2 debt as the rest of the machine).
- **`tar.rs`** (74 lines): the shared USTAR reader both `oci.rs` and
  `crate_archive.rs` call into — fixed 512-byte header records, name/prefix
  concatenation (`tar_name`, `tar.rs:40-50`), octal size parsing
  (`tar_size`, `tar.rs:62-69`), 512-byte-aligned content skipping
  (`padded_len`, `tar.rs:71-73`).
- **`crate_archive.rs`** (139 lines): gzip container unwrap (header flags,
  optional extra/filename/comment fields, CRC skip, `miniz_oxide` DEFLATE
  inflate, footer size check, `gzip_decode`, `crate_archive.rs:35-97`) then
  `tar::walk` over the inflated bytes, single-root detection and stripping
  (crates.io's `name-version/` convention, `single_root`/`strip_root`,
  `crate_archive.rs:112-134`).
- **`ast_probe.rs`** (548 lines): *not* a parser — a hand-written summarizer
  over the already-Snark-produced `ast::SourceFile`/`Item`/`FnItem`/`Expr`
  tree (`crate::VixParser`, `crate::ast`), building `Value::Map`/`Array`
  trees for 4 projections (items/fns/fn/fn.body.children,
  `ast_probe.rs:8-44`) plus a large ad-hoc field-dump (`fn_fields`,
  `ast_probe.rs:88-176`) used elsewhere for reflection. Every leaf is a push
  materialization of data the typed AST (and Snark's query engine) already
  expose structurally.

---

## (c) Migration, per format

### General pattern: remove the keyword, remove the host op, remove the packed-int union

Two independent things currently bake these formats into the language, and
both have to go, not just one:

1. **The host op + `PendingPrimitiveKind`.** `ELF_DOC_HOST=24`,
   `AST_DOC_HOST=25`, `AST_FN_HOST=26`, `OCI_DOC_HOST=28`, `DOC_PARSE_HOST=21`,
   `CRATE_ARCHIVE_HOST=49` (`driver.rs:112-168`) are constants in the same
   fixed 57-entry table V4 wants replaced by a startup registry. Three of
   them are glued into demand/memoization via `PendingPrimitiveKind::{Elf,
   Ast, Oci}` (`driver.rs:776-801`), whose `to_word`/`from_word` pack three
   *disjoint, format-specific* enums into one `i64` by range convention
   (`elf < 1000`, `ast` `to_word` returns `1000..1003`
   (`ast_probe.rs:26-33`), `oci` is `2_000 + projection.to_word()`
   (`driver.rs:787, 792-793`)) — a hand-rolled tagged union that breaks the
   moment a fourth format or a fifth AST projection pushes past 1000/2000.
2. **The keyword-level special form in the lowerer.** This is the part V5's
   framing under-states: `elf(...)`, `ast(...)`, `oci(...)`,
   `crate_archive(...)`, `toml(...)`, `json(...)`, `version(...)` are literal
   `match name { "elf" => ..., "ast" => ..., "oci" => ..., "crate_archive" =>
   ... }` arms inside `call()` (`lower.rs:3155-3169`), checked **before**
   ordinary function-name resolution (`lower.rs:3170-3178`). Each arm
   (`elf_call`/`ast_call`/`oci_call`, `lower.rs:5933-6018`) does nothing but
   validate the argument schema and emit `Op::HostCall { host:
   ELF_DOC_HOST }`/etc. directly. Removing the host op without removing
   these match arms leaves the language still reserving these five
   identifiers as syntax, which is exactly what V8 forbids. The fix has to
   make `elf`/`ast`/`oci`/`crate_archive` resolve through the **same path
   ordinary user functions and (once V4 lands) registered capabilities use**
   — `resolve_function_name`/`fn_refs` (`lower.rs:3170-3172`) — with no
   special-cased string anywhere in `call()`.

### `AST_DOC_HOST` / `AST_FN_HOST` → delete `ast_probe.rs`, expose pull accessors over the already-Snark AST

Because the parsing here is already Snark (see headline finding), this
migration is entirely a V10 problem, not a V5 problem: nothing to write in
Snark, nothing to port to vix. `ast_probe.rs`'s 4 projections + the
`fn_fields` dump should become pull accessors over the frozen, weavy-schema'd
`ast::SourceFile` (tag/field/element access given a handle, on demand) —
using Snark's own query engine (`snark::query`, already proven for
`VixParser::highlights`, `vix/src/lib.rs:86-109`) as the mechanism a vix-level
query (`fn.body.children`, `fns`, etc.) compiles to, instead of a Rust
`match` walking every `ast::Expr` variant by hand. `PendingPrimitiveKind::Ast`
and its `1000..1999` reserved range disappear entirely (it's one of only
three variants in a three-variant enum — deleting it also removes the
`1_000`/`2_000` magic-range hack for the other two, see below).

### `CRATE_ARCHIVE_HOST` / tar → split into a real registered capability (gzip) + a shared "record walk" (Snark-plausible, but decide the boundary deliberately)

`crate_archive.rs` is two unrelated jobs stapled together: decompress
(register as a Rust capability — there's no other honest option, see (a)),
then walk USTAR records. If tar's fixed-header/computed-skip shape is worth
a Snark grammar (a genuine but modest experiment — see the sequencing below),
the boundary should be: Snark grammar produces a CST of header-record
tokens + opaque content-blob tokens (each content token's *length* supplied
by an external scanner reading the preceding octal field, consistent with
`ScannerHostOperation` already existing for exactly this kind of
data-dependent token); vix-level code (or a lowered Snark→weavy pipeline)
turns that CST into the `Tree { entries, blobs }` shape both `oci.rs` and
`crate_archive.rs` currently build by hand. If that experiment doesn't pay
for itself, tar stays a registered capability like gzip — no worse than
today, and it doesn't block anything else.

### `OCI_DOC_HOST` → decompose; no format-specific host op survives

- tar walk → shared with crate_archive (above).
- JSON parsing (`crate::data::parse_json`, `oci.rs:222`) → if not already a
  registered/Snark-backed JSON parser, this is the one clean, low-risk
  Snark win available in this whole set (JSON is genuinely context-free with
  no pointer-chasing; `playgrounds/snark/src/bundled/json/grammar.js`
  already exists).
- Manifest/config/layer digest-chain following, whiteout matching,
  layer-order file lookup → this is ordinary vix code (string splitting,
  map/array traversal over the already-parsed JSON `Value` and the `Tree`)
  once JSON parsing and tar-walking are capabilities/library functions vix
  can call — no host op needed at all. `PendingPrimitiveKind::Oci` and its
  `2_000 +` offset disappear along with `Ast`.
- `input_hash` (`oci.rs:181-215`) is content-hashing and should fold into
  the V2 Blake3 rework, not survive as a bespoke SHA-256 routine.

### `ELF_DOC_HOST` → registered Rust capability (V5-#3), by elimination

Per (a), Snark can't reasonably do this, and hand-writing a pointer-chasing
binary reader in vix directly (V5-#2) would mean reimplementing raw
byte-slicing, bounds-checked arithmetic, and endianness handling in a
language whose whole value model is schema'd records/arrays/handles — likely
possible but a worse fit than a small, well-typed Rust capability with a
**stable registered identity** (V4) that vix calls like any other capability.
This is the one format where V5's own fallback order, not its favored
option, is the honest answer. The 7 `Projection` variants
(`elf.rs:20-77`) become the capability's declared return shape (ideally a
typed record via weavy's declared-struct ABI, V1, rather than the current
untyped `Value` variants) instead of a machine-known "Doc" with string-keyed
`.field()` dispatch.

### How the demand/projection model survives all of the above

None of this changes the fact that vix code wants `elf(bytes).arch` to be
lazy, memoized by content, and to only recompute when the *input bytes*
change — that's what has to be preserved, and nothing above touches it.
Concretely, today: a call like `elf(...)` allocates a `Pending<Doc>` handle
carrying `PendingPrimitiveKind::Elf(projection)` + args
(`PendingPrimitive`, `driver.rs:770-773`); forcing it
(`pending_coercion`, `driver.rs:5102-5194`) computes the value once, records
a `ProjectionPath::Elf { projection }` keyed by the **input's content hash**
against the caller's read-set (`record_projection_for_matching_args`,
`driver.rs:5179-5181`), and a bespoke `elf_projection_memo:
HashMap<(ContentHash, Projection), i64>` (`driver.rs:1590`) avoids
recomputing it. That mechanism — pending handle → forced-on-demand →
content-hash-keyed memo → read-set entry for incremental invalidation — is
demand-driven-ness itself, and it's independent of *how* the value gets
computed. Whether the eventual computation is a registered Rust capability
(ELF), a Snark-lowered-to-weavy pipeline (AST, JSON), or vix stdlib code, the
shape stays: allocate a pending value tagged with (stable capability
identity, args), force it lazily, hash the *result*, key the memo on
(content hash of input, capability identity, args) so a demand-graph
recompute short-circuits identically regardless of what's on the other side
of the call. The one thing worth fixing *while* touching this (not
required, but cheap once you're here): `elf_projection_memo`/
`ast_projection_memo`/`oci_projection_memo`/`oci_file_memo`/
`crate_archive_memo` (`driver.rs:1590-1596`) are five hand-rolled,
differently-shaped caches sitting right next to the driver's *generic*
`memo: HashMap<CanonMemoKey, MemoEntry>` (`driver.rs:1584`) — once none of
them need a bespoke `PendingPrimitiveKind` variant, there's no reason left
for them not to be the same generic memo everything else already uses.

---

## (d) Removing `Version`/`VersionSet`/`Cc` from the machine

**`Version` is already done — read it as the template, not as remaining
work.** `vix/std/version.vix` (commit `e9305bf71`, "Version is a plain vix
struct; kill the VERSION_FIELD intrinsic") is a plain `pub struct Version {
major, minor, patch }` with `parse_version`/`version_lte` written entirely in
vix over string/int primitives (`before`/`after`/`parse_int`) — no host op,
no machine knowledge of semver. `VERSION_FIELD_HOST = 50` (`driver.rs:162`)
is now dead: it doesn't appear anywhere in `lower.rs`'s host-call sites (the
only remaining hits are its own `const` definition), so its removal is pure
cleanup, already earned by prior work, not a design decision. This is
concrete proof the "parser written in vix directly" (V5-#2) path works in
practice for at least the simple case.

**`VersionSet` is genuinely more work, but it's the same kind of work, not a
grammar problem.** `version_set.rs` (562 lines) computes a full **interval-set
algebra** over semver ranges — not just parsing `VersionReq` strings
(`from_req`, `version_set.rs:31-53`, still depends on the `semver` crate for
comparator parsing) but union/intersect operations over release and
prerelease interval sets (`IntervalSet`, `version_set.rs:12-15`), used
wherever a dependency resolver needs to represent and combine "the set of
versions acceptable under these constraints." That's arithmetic and data
structure manipulation (sorted interval lists, comparisons, merges) — well
within what vix already does for `Version` — plus faithfully reproducing
semver's comparator semantics (`^`/`~`/wildcard operators, prerelease-opt-in
rules) without the `semver` crate. That last part is the real cost: it's not
hard *conceptually*, it's a matter of carefully porting a well-specified but
detail-heavy comparison spec, still `VERSION_SET_PARSE_HOST`/
`VERSION_SET_OP_HOST`-shaped work (`lower.rs:6062,6106`) that has to move
into `vix/std/version.vix` (or a sibling module) before those two host ops
and `version_set.rs` can go.

**`Cc`/`Ar`/`Rustc` are a naming problem wearing a hashing-mechanism
disguise.** `cc_capability_hash` (`driver.rs:9096-9102`) builds a reflective
`Value::Struct { name: "Cc".into(), fields: [("fingerprint", ...)] }` purely
to get a canonical hash of it — V8 correctly separates "the hashing method is
fine" (a lesser sin) from "the language has `"Cc"`/`"Ar"`/`"Rustc"`/`"cc"`/
`"ar"`/`"build_script"` as recognized strings at all"
(`cap_schema`/`capability_hash`, `driver.rs:9112-9154`, plus the parallel
`ar_capability_hash`, `driver.rs:9104-9110`) — the real crime. Once V4's
capability registry exists (stable identifiers assigned at registration,
not a fixed position-indexed array), a capability's identity hash should be
derived from **its registration record** (name + signature the registry
already knows, or a content hash thereof) — computed once, generically, by
the registry machinery for every registered capability, not by a per-capability
`fn xxx_capability_hash` the machine hand-writes for each domain concept it
happens to know about. `cc`/`ar`/`rustc`/`build_script` stop being strings
the *machine* matches on; they become names *Vixen-the-build-system* passes
when it registers those capabilities with the (domain-agnostic) registry.
This also retires the three matching `RenderedValue::{Version,VersionSet}`
variants (`driver.rs:365-370`) as part of V10's broader god-enum deletion —
`Sealed` (`driver.rs:410-415`) is the fourth domain-flavored variant there,
tracked separately under V10/subsystem-11, not this problem.

---

## (e) Blast radius, sequencing, risks, open questions

### Blast radius

- **Constants removed from `driver.rs:112-168`:** `DOC_PARSE_HOST`,
  `ELF_DOC_HOST`, `AST_DOC_HOST`, `AST_FN_HOST`, `OCI_DOC_HOST`,
  `CRATE_ARCHIVE_HOST`, `VERSION_PARSE_HOST`, `VERSION_SET_PARSE_HOST`,
  `VERSION_SET_OP_HOST`, `VERSION_FIELD_HOST` (already dead) — 10 of the 57
  host-op constants, once the registry (V4) exists to hold their
  replacements' stable identifiers instead.
- **Modules deleted:** `elf.rs` (444 lines, logic moves to a registered Rust
  capability — code migrates, doesn't vanish), `ast_probe.rs` (548 lines,
  mostly deleted outright — replaced by pull accessors + Snark queries, much
  smaller), `version.rs`/`version_set.rs` (622 lines, logic moves to
  `vix/std/version.vix` and grows there). `oci.rs`/`tar.rs`/
  `crate_archive.rs` shrink to whatever remains after JSON/tar move out from
  under them (likely: tar's record-walk stays either way; the
  JSON/digest-chain/whiteout logic becomes ordinary vix or capability code).
- **`lower.rs` call-dispatch:** `lower.rs:3155-3169`'s 7-arm special-form
  match (`elf`/`ast`/`oci`/`crate_archive`/`toml`/`json`/`version`) and the
  6 corresponding emitter functions (`elf_call`/`ast_call`/`oci_call`/
  `crate_archive_call`/`doc_parse_call`/`version_call`, `lower.rs:5933-6018`
  and neighbors) go away, replaced by ordinary `resolve_function_name`/
  `fn_refs` resolution (`lower.rs:3170-3178`) — this is the part that
  actually satisfies V8, independent of what backs each name.
- **`PendingPrimitiveKind`** (`driver.rs:776-801`) collapses from 3 variants
  to 0 (or is deleted outright) along with its `1_000`/`2_000` packed-range
  hack — `elf`'s replacement doesn't need a `Pending<Doc>` variant if it's a
  capability call through the same registered-capability path everything
  else uses.
- **Five bespoke memo caches** (`driver.rs:1590-1596`) collapse toward the
  driver's existing generic `memo`/`memo_candidates`
  (`driver.rs:1584-1585`) — not required by V5/V8, but freed up by them, and
  cheap to do once you're touching these call sites anyway.
- **`RenderedValue`:** `Version`/`VersionSet` variants (`driver.rs:365-370`)
  removed as part of this problem; the rest of the god-enum (V10) is a
  separate, larger cleanup this doesn't need to wait for or block.
- **Tests touching this surface directly:** `vix/tests/checker_corpus.rs`
  and `vix/tests/real_process.rs` are the only files invoking
  `elf(...)`/`oci(...)`/`ast(...)`/`crate_archive(...)` as vix-level calls
  (one call-site each); broader coverage of the projections themselves lives
  in `driver.rs`'s own `#[cfg(test)]` module (e.g. `driver.rs:6448,
  6479, 8224, 8278` — the `Projection::ALL` exhaustive-probe tests) which
  will need to move or be rewritten against whatever new call surface
  replaces the host ops.

### Incremental, corpus-green sequence

1. **AST first — it's a rename, not a redesign.** The parsing is already
   Snark. Replace `ast_probe.rs`'s hand-written summarizers with pull
   accessors (or Snark queries) one projection at a time
   (items → fns → fn → fn.body.children), keeping `AST_DOC_HOST`/
   `AST_FN_HOST` and `PendingPrimitiveKind::Ast` until the last projection
   moves, then delete both together. Lowest risk in the set — proves the
   "pull accessor over Snark-produced tree" pattern V10 wants, before
   leaning on it anywhere harder.
2. **`Version` cleanup — pure deletion.** Delete `VERSION_FIELD_HOST`
   (already unreferenced) and confirm `version.rs`'s remaining callers
   (`VERSION_PARSE_HOST`) are only the simple `Version`, not `VersionSet` —
   if so, this can go the same pass as step 1, no port required, it's
   already vix code.
3. **`Cc`/`Ar` capability-hash naming.** Independent of everything else here
   — swap `cc_capability_hash`/`ar_capability_hash`/`cap_schema` for
   registry-derived identity once V4's registration record exists. Can land
   whenever V4 lands, doesn't block or depend on the parser work.
4. **JSON, if not already Snark/registered.** Check whether
   `crate::data::parse_json` already goes through Snark; if not, this is the
   cleanest standalone Snark win here — do it in isolation, verified against
   whatever JSON fixtures already exist, before touching `oci.rs`.
5. **OCI decomposition**, now that tar and JSON are settled: pull the
   digest-chain/whiteout/layer-lookup logic out into vix or a thin
   capability, re-point `OCI_DOC_HOST` call sites at it, delete
   `PendingPrimitiveKind::Oci` and the `2_000 +` hack.
6. **tar experiment** (optional, time-boxed): try the Snark
   external-scanner-backed record grammar described in (c). If it doesn't
   pay for itself in debuggability/visualization versus the current 74-line
   `tar.rs`, stop and leave tar as shared plain Rust — this is explicitly
   the kind of "detour, don't care, months" experiment V5 licenses, but it
   shouldn't gate anything else in this sequence.
7. **`VersionSet`.** Port the interval-set algebra to
   `vix/std/version.vix`, faithfully reproducing the comparator semantics
   currently delegated to the `semver` crate. Largest single chunk of
   *porting* work in this whole problem, but zero design risk — same shape
   as step 2, just bigger.
8. **`crate_archive`/gzip** → register as a plain Rust capability once V4's
   registry exists; combine with whatever tar grammar/module step 6 landed
   on (or the plain `tar.rs` if it didn't).
9. **ELF → registered capability.** Last, because it has no Snark path and
   benefits most from V4's registry existing first (stable identity) and V1's
   declared-struct ABI existing first (typed return shape instead of another
   ad-hoc `Value` enum). Corpus-green means: same `Projection::ALL` behavior,
   verified via the existing exhaustive probe tests (`driver.rs:6448` etc.),
   moved to exercise the capability-call surface instead of the host op.
10. Only after all of the above: delete the now-empty
    `PendingPrimitiveKind` enum and whatever's left of the `1_000`/`2_000`
    packed-range convention, and fold the five per-format memo caches into
    the generic `memo`.

### Risks

- **Scope creep into "rewrite the format parsers in Rust anyway, just
  differently."** The anti-pattern V5 names explicitly ("a build step emits
  an executable, another step reads a version of it" becoming 5,000 lines of
  Rust) is easy to recreate by accident if the ELF capability, once
  registered, grows unchecked scope (more ELF classes, more architectures,
  a full symbol-versioning implementation) instead of staying scoped to what
  `Elf::Projection::ALL` actually needs today.
- **The tar/Snark experiment (step 6) is genuinely open-ended.** It's the
  one piece of this whole plan where "try it and see" is the honest plan,
  not false modesty — resist the temptation to force it through if the
  external-scanner plumbing turns out uglier than 74 lines of `tar.rs`.
- **`VersionSet` correctness regressions.** Porting the `semver` crate's
  comparator semantics by hand risks subtle divergence (prerelease
  admission rules in particular, `version_set.rs:38-52`) that won't show up
  until a real dependency resolution hits an edge case. This is exactly the
  kind of thing that wants an oracle (the existing `semver`-crate-backed
  behavior *is* the oracle) — differential-test the vix port against the
  Rust implementation on a corpus of real-world version ranges before
  deleting `version_set.rs`.
- **Registry (V4) is a hard dependency for steps 3, 8, 9**, not just a nice-
  to-have — without it, "registered Rust capability" degrades back into
  "another fixed-position host-op constant," reproducing the exact problem
  being fixed. If V4 lands later than this work, ELF/gzip/Cc-Ar should land
  provisionally on the current host-op mechanism and be revisited, not
  block on V4 finishing first.
- **Bespoke memo caches (driver.rs:1590-1596) collapsing into the generic
  `memo`** touches code paths every one of these formats' tests exercises —
  worth doing, but do it as its own commit/step, not folded silently into a
  format migration, so a regression is attributable.

### Open questions for Amos

1. For the tar/Snark experiment (step 6): is the bar "declarative header
   fields even if content-skipping stays scanner/imperative" an acceptable
   partial win, or does it need to be all-or-nothing to be worth the detour
   at all?
2. For ELF's registered capability: should its typed return shape ride V1's
   declared-struct ABI from the start (more work now, no re-migration
   later), or land first as today's `Value`-shaped return and get typed
   later once V1 exists generally?
3. Is there an existing corpus of real-world Cargo version-range strings
   (crates.io lockfiles, etc.) already available to differential-test the
   `VersionSet` port against `semver`, or does one need to be assembled?
4. Does `crate::data::parse_json` already run through Snark, or is that
   itself an open item this problem happens to depend on? (Not confirmed by
   this read — `oci.rs:222` just calls it as an opaque function.)
