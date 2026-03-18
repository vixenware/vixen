# Build Recipes And Configured Packages

This note captures the design pressure that came out of trying to model a
real dependency stack in `zoo`:

- `zlib`
- `libpng`
- an application that links against `libpng`
- static and shared variants on Linux, macOS, and Windows

The main conclusion is that the missing piece is not "a better monolithic
sketch". The missing piece is a build model that distinguishes:

1. a recipe module
2. a configured package instance
3. lazy named outputs from that configured package

## What Prompted This

Returning one monolithic `Tree` from `build_zlib()` or `build_libpng()` is too
coarse.

That loses important structure:

- static vs shared outputs
- public headers vs runtime files
- compile defines needed by consumers
- link inputs needed by consumers
- generated config headers
- Windows import library vs DLL split

The evaluation model is already demand-driven, so the right answer is not to
force everything into one eager install tree. The right answer is to return a
structured value with separate lazy outputs.

## Core Model

### 1. Recipe Module

A `.vx` file imported from somewhere is best understood as a recipe module.

It should export a function like `build(...)`, but `build(...)` is closer to a
configure step than to a raw compile step. It selects:

- version
- options
- dependencies
- target / toolchain

and returns a configured package instance.

### 2. Configured Package

A configured package is the result of applying a recipe to concrete parameters.

It should not be a plain `Tree`. It should be a struct that exposes the
configured package's outputs and consumer-facing metadata.

For C-family builds, that likely means something shaped like:

```vx
struct LinkVariant {
  link_inputs: Array[ExecInput],
  runtime_files: Tree,
  dev_files: Tree,
}

struct CLibrary {
  headers: Tree,
  defines: Array[String],
  variants: Map[String, LinkVariant],
}

struct CExecutableVariant {
  binary: Blob,
  runtime_files: Tree,
}

struct CExecutable {
  variants: Map[String, CExecutableVariant],
}
```

The exact names are provisional. The important part is the shape:

- common semantic fields for the common case
- named variants for static/shared/etc
- no requirement to collapse everything into one tree

### 3. Lazy Named Outputs

Outputs should be independently demand-driven.

If a consumer only asks for:

- `libpng.variants["static"]`

then only the work needed for that variant should be forced.

If a consumer also asks for:

- `libpng.variants["shared"]`

then that additional work should happen too.

This is not a special new laziness feature. It is just the existing
demand-driven model applied to a structured result instead of a monolithic
artifact tree.

## What Is Language-Level Versus Stdlib-Level

Most of this is standard-library design, not language design.

The language already has the key pieces:

- structs
- maps
- modules
- demand-driven evaluation
- trees and blobs

So the following should be treated as stdlib/build-model concerns:

- "what is a C library build result?"
- "how do I represent static vs shared outputs?"
- "how do consumers link against a dependency?"
- dependency lookup / resolution policy

What does feel language-level is the module/import surface.

## Pressure On Imports

The current import model brings exported names into scope directly. That is too
weak for serious build composition because every recipe naturally wants to
export a function called `build`.

We want to be able to write something conceptually like:

```vx
let zlib = import "./zlib"
let png = import "./libpng"
let app = import "./pnggen"

let z = zlib.build(version: "1.3.1", tc: tc);
let p = png.build(version: "1.6.43", tc: tc, zlib: z);
let a = app.build(tc: tc, libpng: p);
```

That implies Vixen should support namespaced import forms such as:

- `let zlib = import "./zlib"`
- or `import "./zlib" as zlib`

This is more important than default exports.

Default exports might still be useful, but the primary missing feature is a way
to refer to a module namespace explicitly.

## Build As Configure

`build(...)` should be thought of as "configure + expose outputs", not merely
"compile immediately".

That matters because recipe parameters naturally include:

- version
- feature options
- static/shared selection
- dependency instances
- target / toolchain

So the configured package identity is something like:

- recipe
- version
- option set
- dependency instances
- target/toolchain

That identity is the natural place where dependency unification eventually
lives.

This is especially relevant for Cargo integration later: dependency unification
is not a separate problem from "how do we parameterize and identify configured
builds?" It is the same family of problem.

## Versioning Models

Vixen should support both of these models.

### Model A: Version-Specific Build Files

Good for simple vendored C packages.

Examples:

- `zlib/1.3.1/build.vx`
- `libpng/1.6.43/build.vx`

This is often attractive when each version has materially different build
quirks.

### Model B: Parameterized Recipe

Good when a family of versions mostly shares the same build logic.

Example:

```vx
let libpng = import "./libpng"
let p = libpng.build(version: "1.6.43", opts: ..., tc: tc);
```

This is especially attractive for ecosystems where dependency solving and
version unification matter more.

Vixen should not force one model. Different codebases will want different
tradeoffs.

## Static And Shared Variants

The motivating example should explicitly exercise both:

- static library outputs
- shared library outputs
- static app linkage
- dynamic app linkage

because this flushes out real platform differences:

- Linux shared library is a `.so`
- macOS shared library is a `.dylib`
- Windows shared library is a `.dll` plus import `.lib`

That means the stdlib model should not pretend a "shared library output" is
just one file.

Instead, a shared variant should be able to express:

- what a consumer links against
- what must ship/run at runtime
- what development files belong to that variant

That is exactly why `LinkVariant`-style structs are preferable to a single
output tree.

## Dependency Interfaces

For now, the right composition model is explicit value wiring between recipe
modules.

For example:

```vx
fn build_libpng(tc: Toolchain, zlib: CLibrary) -> CLibrary
fn build_pnggen(tc: Toolchain, libpng: CLibrary) -> CExecutable
```

That is enough to model real dependency structure inside `zoo` without
pretending we already have package registry / package lookup semantics.

Longer term, dependency lookup and resolution can sit on top of the same model.

## Non-Goals

This note is not proposing:

- a package registry design
- a global dependency solver
- system-library discovery and linking
- a finalized stdlib API

It is only trying to identify the conceptual pieces that a real build API
needs.

## Concrete Wishlist

### Language

1. Namespaced imports / module expressions.
2. Possibly default exports later, but this is secondary.

### Stdlib Build Model

1. Standard result structs for configured packages.
2. Standard result structs for C libraries and executables.
3. Named variants for static/shared/etc.
4. Consumer-facing metadata:
   - headers
   - defines
   - link inputs
   - runtime files
5. A clear distinction between:
   - recipe module
   - configured package instance
   - lazy named outputs

### Later

1. Dependency lookup / resolution layered on top of configured package
   identities.
2. Dependency unification using the same configured-package model.

## Immediate Implication For `zoo`

When we return to the `zlib` / `libpng` / app example, the goal should not be:

- "make one sketch that happens to work"

The goal should be:

- define a provisional stdlib-style result shape
- split the example into recipe modules
- wire configured package results explicitly between them
- stress static/shared variants across all three platforms

That will teach us more than another monolithic build file.
