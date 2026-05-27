+++
title = "Rust/Vixen FFI shape"

[extra]
type = "ArchitectureNote"
topic = "language"
created = "2026-05-26"
tags = ["language", "runtime", "ffi", "intrinsics"]
+++

How Vixen code calls into the Rust runtime.

## Body-absent declarations are runtime intrinsics

A function declared in Vixen with **no body** is an intrinsic: the runtime supplies the implementation. Looking at `crates/vx-stdlib/std/build.vx`:

```
fn exec[T](
    args: Array[ExecInput]
) where { ... } -> T;
```

The trailing semicolon, no body — that's an intrinsic. The runtime sees a call to `exec` and dispatches to the corresponding native implementation.

No special annotation, no `#native` effect, no `extern` keyword. Just a function declared without a body. Inputs, outputs, capability annotations on the signature all behave like any other function. The mechanism is invisible at the call site: the caller doesn't know (or care) whether the callee is body-Vixen or body-Rust.

## Parsing: Rust parses into Vixen types

A specific instance of the runtime/language split that's worth calling out. When `vixen-rust` needs to read a `Cargo.toml`, it declares a Vixen struct shaped like the cargo manifest (with rename / alias / optional annotations on fields, like a serde struct definition), then calls the parsing intrinsic. The Rust runtime parses the TOML bytes *into* that Vixen type using the schema declared on the Vixen side.

The schema lives in Vixen; the parser lives in Rust. Vixen gets typed data without the runtime knowing what the data means.

The Vixen type system is flexible enough — anonymous types, named types, optional fields, sum types via `enum` — to express the shapes that serde-style deserialisation would handle on the Rust side.

## What's actually in the FFI

From `core.vx` and `build.vx`, the current intrinsics include:

- Process execution under sandbox: `exec`, plus everything in the observer signature.
- Fetching with content-addressing: `acquire(url) where {digest}`, `acquire(url) where {digest, extract}`.
- Local-host filesystem access (under `#fs`): `read_file`, `read_dir`, `glob`, `exists`, `metadata`.
- Build evaluation entry: `build(entry) -> T`.
- Host discovery: `discover[T](class, probe) #discover -> T`.
- Template rendering: `render[T](template) where {bindings} -> Blob`.
- Stabilisation across builds: `stabilize` (signature truncated in the visible header).
- Basic operations on `String`, `Symbol`, etc.

The list grows when the runtime needs to expose a new primitive (a new sandbox capability, a new constraint-solver algorithm, etc.). It does *not* grow when domain logic changes; that's a Vixen-package concern. See [[Domain logic lives in Vixen, not in the runtime]].

## Effects on intrinsics

Capability annotations (`#fs`, `#discover`, `#host`, `#sandbox`) decorate intrinsic signatures the same way they decorate any function. `read_file(path: String) #fs -> Blob` declares that calling `read_file` requires the `#fs` capability — host-side filesystem access. The capability semantics are uniform; the runtime is just the implementation of those particular bodies.

## Status

The shape is established. The exact list of intrinsics is in flux as the language matures. The "body-absent = intrinsic" convention is stable.

## Related

- [[Domain logic lives in Vixen, not in the runtime]]
- [[Values are hashable, comparable, and serialisable]]
- [[Bytecode and JIT exploration]]
- [[Package management for Vixen]]
