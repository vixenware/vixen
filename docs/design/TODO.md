# Vixen design TODO

## Query / target selection
How does the user ask for specific outputs? Is there a query language? Pattern
matching on target names? Labels? How do CLI args (`Args`) map to what gets
built inside `std.rust.workspace`?

## Toolchain acquisition
How are toolchains (rustc, clang, etc.) downloaded and managed? Are they
store-backed? How does version selection work?

## Network / downloads
How does Vixen fetch dependencies from registries (crates.io, npm, etc.)?
Is there a built-in for HTTP fetches? Is it cacheable? How does it interact
with the store?

## Deserialization
How does Vixen parse structured data (JSON, TOML, Cargo.toml, etc.)? Are
parsers built-in? Are they generic over Vixen types?

## Arg/env type for exec
The exact type for `exec` args and env values that can carry both plain strings
and store-backed paths. Needs a name and clear semantics.

---

# Resolved

## Tree is one type (not local vs remote)

There is no separate "local tree" vs "store tree" type. `Tree` is `Tree`. A tree
might be backed by a local directory, by the CAS, or by a mix. The runtime
handles materialization transparently:

- The entry point receives the working directory as a `Tree`
- Tree operations (glob, filter, subtree) produce new Trees — all lazy
- Nothing is hashed or committed until an `exec` boundary actually needs
  the content shipped to an executor
- At that point, content-defined chunking, merkle trees, and CAS dedup
  make the transfer efficient — only missing chunks are uploaded

The Vixen language never exposes Blake3 hashes. The hash is an implementation
detail of the store, not a language-level concept.

## Source file discovery for Rust crates

`std.rust.workspace` handles discovering which files belong to a crate. Simple
`.rs` globbing is insufficient — crates can pull in arbitrary files via
`include_bytes!`, `include_str!`, `build.rs`, proc macros, etc.

The default behavior does its best (parse `Cargo.toml`, follow conventional
paths), but it will miss cases that require full macro evaluation. For those
cases, users write a `build.vixen` that explicitly declares additional inputs.
This is the escape hatch: the default works for most crates, and custom build
files handle the rest.

## Demand model

Entry point is `fn build(src: Tree, args: Args) -> Tree`. The runtime calls it
with the working directory (lazy tree) and parsed CLI args. The returned tree is
the build output, materialized to disk by the runtime.

## Outputs

The build function returns a `Tree` — the output directory to materialize.
No separate target catalog or enumeration phase.

## Cargo-to-Vixen compatibility

If no `build.vixen` exists, Vixen looks for `Cargo.toml` and evaluates as if
a thin build file called `std.rust.workspace(src, args)` were present. CLI
flags match cargo's (`--bin`, `--lib`, `--package`, `--release`, etc.).

## Enum alias variants and auto-widening

Enums support alias variants (`File: Blob`), `@` shorthand (`@Blob`), and
sum type shorthand (`enum Entry = Blob | Tree`). Auto-widening: if a variant
wraps exactly one type, values of that type auto-widen into the variant and
up to the enum.

## Static methods and constructors

`fn Type.method() -> T` without `self` is a static method. Used for
constructors like `Blob.from_string`, `Tree.of`. Tree operations (subtree,
glob, merge) are lazy instance methods.
