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
