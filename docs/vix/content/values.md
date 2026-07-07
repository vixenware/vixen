+++
title = "Values and types"
weight = 1
type = "reference"
updated = "2026-07-08"
+++

Every vix value is **bytes described by a schema**. There is no universal
`Value` object that everything boxes into — no god-enum every operation has
to switch over, no runtime "what is this?" question. What a value *is* is
static, checked at the call and the pattern match; where its bytes live
(a register, a frame slot, a content-addressed store entry) is the machine's
business, invisible from vix source.

Two properties come with every value, without opt-in: it is **hashable by
content** (which is what makes it memoizable, storable, and comparable across
processes — you never implement `hash`, `eq`, or serialization yourself), and
it is **schema-checked at construction** (there's no reinterpreting bytes; the
checker verifies field access and pattern matches against declarations before
the machine runs).

## Scalars

`Int`, `Float`, `Bool`, `String`. Floats use ordered-float semantics at every
boundary that needs a total order (hashing, comparison, `collect`) — vix
values are required to be **totally ordered as well as hashable**, so that
collecting a set of results never depends on the order the machine happened
to finish them in. Scheduling nondeterminism is unobservable by construction.

## Structs and enums

Declared in vix source, matched with patterns. From the resolver,
[`rodin/rodin.vix`](https://code.vixen.rs/vixen/facet/src/branch/main/rodin/rodin.vix):

```vix
enum Source {
    CratesIo,
    Registry(String),
    Git { url: String, rev: String },
    Path(String),
}

enum CompatClass {
    Major(Int),
    Minor(Int),
    Patch(Int),
}

struct PackageId {
    source: Source,
    name: String,
    compat: Option<CompatClass>,
}
```

Enum variants can carry no payload (`CratesIo`), a tuple payload
(`Registry(String)`), or a struct-shaped payload (`Git { url, rev }`) — all
three are ordinary vix, matched the same way. See
[Pattern matching](/vix/patterns/).

## Containers

`[T]` arrays, `Map<K, V>`, `Option<T>`, tuples. Functional update methods
(`push`, `set`, `insert`) return the new value — and run in place when that's
provably invisible; see [Molten values](/vix/molten/).

**`Array<T>`** — typed, with full schema support (a landed capability: arrays
used to be a bag of untyped words). `push`/`pop`/`set`/`len`/`map`/`filter`/
`collect`/`join`. `pop` is the one surprising signature: it doesn't return
`Option<T>`, it returns `(T, [T])` — the popped element and the remainder,
paired, so the common "pop and recurse on the rest" shape doesn't need an
`unwrap`:

```vix
fn workspace_members_from_manifest_paths(paths: [Path], out: [String]) -> [String] {
    match paths.len() == 0 {
        true => out,
        false => workspace_members_from_manifest_paths_tuple(paths.pop(), out),
    }
}

fn workspace_members_from_manifest_paths_tuple(popped: (Path, [Path]), out: [String]) -> [String] {
    let out = workspace_members_from_manifest_paths(popped.1, out);
    out.push(member_from_manifest_path(popped.0))
}
```
— `playgrounds/snark/src/bundled/vix/samples/cargo_manifest.vix:254-264`. `.0`
and `.1` project tuple fields; `popped.1` is "the array with one element
removed," fed straight back into the recursive call.

**`Map<K, V>`** — `insert`, `get` (returns `Option<V>`), empty construction.
Maps are canonical: collecting into a map always yields the sorted-by-key pair
sequence, never insertion or arrival order, so two builds of the same map
content hash identically regardless of what order the machine happened to
compute the entries in. Sets are currently spelled `Map<K, Bool>` — a real set
type is on the ergonomics list (see [Honest gaps](/vix/gaps/)).

**`Option<T>`** — `Some(v)` / `None`, matched with patterns; `.unwrap()` is an
ordinary (partial) Option-shell operation, not a magic "force" primitive — see
[Demand semantics](/vix/demand/) for why that distinction is load-bearing.

**Tuples** — `(A, B)`, projected with `.0`/`.1`. The `Array.pop` signature
above is the idiomatic use.

## `Version`

Full semantic versioning, as a plain vix struct in the standard library —
not a primitive. See [The standard library](/vix/std/).

## `Path` — a capability, not a string

Paths are **join-only**. You never construct one from a bare string; you
`join` a segment onto a root you were *handed* — a workspace root, an
extracted crate's root, an out-dir, the tree output of another build step.
`String` is accepted only as a segment name, never cast freely into a path.
This is deliberate: provenance of every path traces back to a grant, which is
what lets the sandbox reason about what a build step is allowed to touch. An
earlier draft allowed `String.to_path()`; it was caught before landing and
reworked to `Path.join(segment)` for exactly this reason.

```vix
fn manifest_path(root: Path) -> Path {
    root.join("Cargo.toml")
}
```

## `Tree` — the artifact currency

A `Tree` is a Merkle-tree-shaped artifact: an extracted archive, a build
step's output directory, a workspace checkout. Trees are lazily materialized
— demanding a whole tree's identity is expensive (it means hashing everything
under it), but *projecting* a path out of a tree (`tree / p"Cargo.toml"`) is
cheap and doesn't force the rest of the tree to exist. `/` is the projection
operator; `p"..."` is a path literal. `Tree.text(path)` reads a single file
leaf as a `String` — a loud error if the path names a directory:

```vix
run.text(p"build.stdout")
```
— from `vix/tests/argv_arg.rs`, reading a build script's captured stdout
after running it. See [Exec nodes](/vix/exec/) for where the `Tree` comes
from.

## `Doc` — dynamic data, at the boundary

`Doc` is the deliberate escape hatch: dynamic, schema-less structured data,
the shape you get back from a parser like `toml()` or `json()` before you've
decided what typed shape it represents. You navigate it by field access and
coerce it into a typed value explicitly — the checker treats that coercion as
a checkable step, not a silent cast:

```vix
fn manifest_doc(manifest: Tree) -> Doc {
    toml(manifest / p"Cargo.toml")
}

fn package_doc(manifest: Tree) -> Doc {
    manifest_doc(manifest).get("package").unwrap()
}

fn doc_string(doc: Doc, key: String) -> String {
    doc.get(key).unwrap()
}
```
— `playgrounds/snark/src/bundled/vix/samples/cargo_manifest.vix:92-114`. See
[Document parsing](/vix/parsing/) for how `Doc` values get produced in the
first place.

## `Pending<T>` and `Realized<T>` — the honest shape of "not yet"

Most values cross function boundaries as plain content — but a value that
came from a producer that hasn't finished (an in-flight exec, a not-yet-solved
resolution step) needs to represent "not realized yet" without becoming a
special case everywhere. The declared shape is a two-variant wrapper:
`Realized<T> = Ready(T) | Pending(invocation identity)`. You mostly don't
write this by hand — schema-keyed containers (a `Map` that might hold a
promoted producer next to a literal) carry it so the container can stay
representationally uniform even when some entries are still in flight. The
discriminant lives in the value's own layout (a declared field, not a runtime
tag consulted by dynamic dispatch) — reading a `Ready` entry costs nothing
extra; touching a `Pending` one demands its producer. See
[Demand semantics](/vix/demand/) for the rule this rests on (**bits are
strict, identities are lazy**) and [Honest gaps](/vix/gaps/) for where this is
still filling in around aggregates.
