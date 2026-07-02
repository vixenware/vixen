# vix types: structs, enums, tuples, arrays — the destination

Companion samples (in the playground bundle, parse live at `#/vix`):
- `samples/types.vix` — the tour, build-flavored (targets, toolchains, artifacts)
- `samples/eval.vix` — the self-hosting shape: a recursive enum + match evaluator,
  i.e. the kind of program `resolve`/`type_of` will be when intelligence queries
  move into vix

## Ground rules (semantics before syntax)

1. **Every user type is automatically hashable, equatable, totally ordered,
   facet-shaped, serializable, and printable.** There is NO derive layer — nothing
   to derive. This is the build-cache invariant surfacing as ergonomics: the
   language cannot express a type that can't be a memo key. (This is also why the
   picante mapping needs no annotations.)
2. **Total order is structural and declaration-ordered**: enum variants order by
   declaration position, then payload; struct fields compare lexicographically in
   declaration order. Consequence worth saying out loud: *reordering variants or
   fields is a semantic change* (it changes sort order — and the shape hash anyway).
   `collect` over user-typed keys is canonical because of this rule.
3. **Values, not places.** No references, no lifetimes, no `mut`, no interior
   mutability. Recursive types need no `Box` — representation is the engine's
   problem, the surface is pure values. Collections are persistent values:
   `env.insert(k, v)` returns a new map.
4. **Type declarations are content-addressed values.** A closure's hash includes
   the canonical AST of every type it touches; changing a type's shape invalidates
   exactly the memo keys that could observe it. Field NAMES are part of shape
   (facet semantics).
5. **Projections stay free.** Field access, tuple index, and payload extraction
   are lenses: demanding `artifact.name` from a node's output demands that
   projection, not the whole value (aggregation heuristics still set the memo
   floor).
6. **Capabilities stay values.** A capability instance can sit in a user struct
   like any value; its fingerprint rides into whatever closure captures the
   struct. Types still carry capability KINDS only; no permissions in types.

## Declarations

```vix
struct Toolchain {
    cc: Cc,
    ar: Ar,
    opt: Int = 2,          // field defaults mirror kwargs-with-defaults
}

struct Sha256(String);      // tuple struct / newtype
struct Probe;               // unit struct

enum Os { Linux, Macos, Windows }

enum Artifact {
    Object(Path),
    Archive { name: String, members: [Path] },   // record variant
    Phony,
}

struct Pair<A, B> { first: A, second: B }        // generics: no lifetimes,
enum Option<T> { Some(T), None }                 // and no bounds needed for
enum Result<T, E> { Ok(T), Err(E) }              // hash/eq/ord — universal
```

- Generics are Rust-shaped minus everything references made necessary. The most
  common Rust bounds (`Hash + Eq + Ord`) don't exist because every type already
  has them; bounded polymorphism beyond that (traits? structural interfaces?) is
  deliberately DEFERRED — see critique agenda.
- `Option`/`Result` are prelude enums, not syntax. No `T?` sugar (innovation
  points spent elsewhere).

## Types

- named + generic application: `Toolchain`, `caps::Cc`, `Pair<Int, String>`,
  `Map<String, String>` (maps are ordered by construction — K is Ord, always)
- arrays: `[T]` (already shipped)
- tuples: `(A, B)`; function types: `fn(T, U) -> V` (closures infer them)
- no map literal syntax v0: `Map::from([(k, v), …])` — boring on purpose

## Expressions

```vix
let base = Toolchain { cc: cc, ar: ar };          // defaults fill `opt`
let fast = Toolchain { opt: 3, ..base };          // record update
let pair = (unit, artifact);                      // tuple literal
let first = pair.0;                               // tuple index
let os = Os::Linux;                               // unit variant
let obj = Artifact::Object(p"lua.o");             // tuple variant = call shape
let lib = Artifact::Archive { name: "liblua.a", members: objs };
```

## Patterns

```vix
match artifact {
    Artifact::Object(p)                  => install(p),
    Artifact::Archive { name, members }  => pack(name, members),   // shorthand
    Artifact::Phony                      => empty(),
}

match pair {
    (u, Artifact::Object(p)) if u == p"lua.c" => main_object(p),   // guards
    (_, a)                                     => a,
}

match tc {
    Toolchain { cc, .. } => cc,          // struct pattern + rest
}
```

**The binding-vs-variant rule (kills Rust's typo footgun deterministically):**
a bare identifier pattern that names a variant in scope is a variant pattern;
a bare identifier that does NOT is a binding — and it is an ERROR for a binding
pattern to shadow a known variant name. No case heuristics, no silent
catch-all-by-typo. (Today's binder already records `Linux`/`Macos` as
constructor-like unresolved; this rule is where that lands.)

## Round 2 (Amos's critique, landed)

- **Map literals** (was: "Map::from is too austere" — confirmed): bare braces are
  free in vix (no block/if exprs), so maps get `{ "CC": "clang", "OPT": flag }` —
  keys are expressions, `{}` is the empty map, and it works as a field DEFAULT:
  `env: Map<String, String> = {}`.
- **Partial application** (field defaults kept, and generalized): a trailing bare
  `..` means "the rest comes later" — the same `..` as record update and pattern
  rest. `scaled(k: 2, ..)` is a function of the not-yet-given params;
  `Toolchain { cc: c, .. }` is a partial constructor. Explicit marker on purpose:
  a FORGOTTEN argument is an error, a partial application is a decision. Typing:
  the partial's remaining params are the missing non-defaulted ones; defaulted
  params stay overridable at the completion site.
- **`t.0.1` SOLVED, no `._0` ugliness**: `tuple_index` is a dedicated integer
  token, legal only where field names are expected — states where `number` (with
  its fraction rule) is NOT legal. Per-state lexing means no float token ever
  forms where none can occur, so nothing needs re-splitting (the rustc token
  hack rust-analyzer hates). Same mechanism as flags-vs-minus.
- **Guards**: kept. **Tuple structs**: kept.

## Bounded polymorphism (reasoned out)

What actually generates pressure for bounds, case by case:

1. **Hash/Eq/Ord bounds** — the overwhelming majority of bounds in real Rust
   collections code. Gone by construction: universal.
2. **Collection/HOF utilities** (`map`, `filter`, `fold`, `sort_by_key`) — need
   only first-class fn types + universal ord. No bounds.
3. **Numeric abstraction** (`fn sum<T: Add>`) — a build language does arithmetic
   on Ints and Floats, not on abstract rings. Concrete overloads suffice; skip.
4. **Display/Debug/Serialize** — engine-provided via facet for every value.
   There is no "printable" bound because there is no unprintable value.
5. **Abstracting over capabilities** ("any C compiler") — looks like a trait,
   is actually the COMMAND GRAMMAR / capability-kind layer's job. Type bounds
   are the wrong tool; the daemon's capability taxonomy is the right one.
6. **Self-hosted intelligence queries** — match over concrete AST enums;
   generic helpers covered by (2). No bounds.

What remains is method-shaped abstraction over user types ("anything with a
`name()`"), which is rare in build code and NONE of our sketches need. Position:
**ship no bounds; the escape hatch, if self-hosting ever hurts, is structural
interfaces** (conformance by shape, checked at instantiation — no impl blocks
for conformance, no coherence, no orphan rules), and never trait objects: the
demand graph doesn't need dynamic dispatch, and weavy's speculation/IC machinery
already handles polymorphic call sites better than vtables would.

Monomorphization note: the interpreter doesn't specialize at all; the JIT
specializes hot instantiations through the existing guard/deopt lanes. Bounds
absence keeps both trivial.

## Total order = declaration order (clarified)

The question I was asking, concretely: `enum Os { Linux, Macos }` makes
`Linux < Macos` BECAUSE Linux is declared first. Sort a `[Os]` and Linux sorts
first; swap the declaration order and the same program's sorted output flips.
So variant order in SOURCE is semantics, not style — an auto-formatter or a
well-meaning alphabetizer changes behavior. The saving grace: the shape hash
changes on reorder anyway, so caching already treats it as a new type; there is
no INCREMENTAL surprise, only a human one. Alternatives (alphabetical order,
explicit discriminants) buy little and cost syntax. Staying with declaration
order unless it bites.

## impl blocks (destination, not v0.2 of the grammar)

```vix
impl Artifact {
    fn name(self) -> String { … }        // self by value — everything is
    fn phony() -> Artifact { … }         // associated fn
}
```

Methods are receiver sugar over functions; no `Self` type v0, no traits v0.

## What this buys (why this is the destination)

- `eval.vix` type-checks the self-hosting story: recursive enums + match +
  persistent maps is exactly the shape of `resolve`/`type_of`. When the user type
  surface lands, the Rust binder becomes the reference oracle for its own vix
  reimplementation.
- Every type here is a facet shape ⇒ fable computes over it, phon/postcard
  persists it, the journal pins it, and the playground can pretty-print it —
  one data plane, no bridges.

## Critique agenda

1. Field defaults in `struct` decls: keep (symmetry with kwargs) or cut (creep)?
2. Map story: is `Map::from([(k, v)])` too austere for a build language
   (env vars everywhere), or do we hold the no-literal line?
3. Bounded polymorphism: traits, structural interfaces, or nothing-until-it-hurts?
   (Self-hosting will answer this empirically; nothing-until-it-hurts is my vote.)
4. Guards in match: kept them (cheap, Rust-familiar) — veto?
5. The binding-shadows-variant ERROR: right call, or should it be a
   deny-by-default lint so prelude growth can't break old programs?
6. Declaration-order total order: comfortable making reorder = semantic change?
   (The shape hash changes anyway, so caching already treats it as one.)
7. Tuple structs: earn their place (newtypes like Sha256) or cut tuples-in-decls
   and keep only tuple values?
8. `match` scrutinee vs struct literal ambiguity: adopting Rust's rule (no bare
   struct literal in scrutinee position; parenthesize) — grammar enforces it.
