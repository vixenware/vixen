+++
title = "Language Guide"
insert_anchor_links = "heading"
weight = 10
+++

# Vixen Language Guide

This guide is the shortest route to reading and writing current Vixen. It is
non-normative and example-first. If it disagrees with the rest of the spec,
these pages win:

- [Core Language](core.md)
- [Evaluation](build.md)
- [Build and Runtime Standard Library](../stdlib/build.md)

## Start from a real build file

The fastest way to understand Vixen is to read a small real build:

```vixen
fn main(workspace: Tree) with { ..VixenMain } -> Tree (
  let rust = RustToolchain where (version >= 1.91);
  let hello = exec [rust.rustc, workspace "src/main.rs", "-o", "out/hello"] with {
    writable: ["out"],
    observer: (proc, vfs) => (
      if proc.exit_code != 0 (
        fail `rustc failed`
      ) else (
        vfs.file "out/hello"
      )
    ),
  };
  Tree %{ "target/debug/hello" => hello }
)
```

That one example already shows most of the authored surface:

- `main` is the package entry point. It takes the workspace as a `Tree` and
  returns a `Tree` that overlays onto it.
- `RustToolchain where (...)` requests a toolchain ability; the runtime
  resolves it and routes the `exec` to a matching executor.
- `Tree %{ ... }` builds a tree from a map of path-strings to entries.
- `Blob("...")` builds a file-like value from text.
- `exec [ ... ] with { ... }` runs a process with positional inputs and a
  keyword record.
- `observer: (proc, vfs) => ...` is a closure that runs on the executor.
- `if cond ( then ) else ( else )` is an expression.

## Records are not maps

Vixen uses two distinct literal forms that look similar at first:

```vixen
let settings = { optimize: true, debug: false }
let outputs = %{ "bin/app" => binary, "lib/libapp.a" => archive }
```

- `{ field: value }` is a record.
- `%{ key => value }` is a map.

That distinction matters everywhere:

- record literals use field names
- map literals use general key expressions
- `with { ... }` passes keyword arguments as a record
- `Tree %{ ... }` takes a map payload, not a record payload

If you remember one syntax rule, remember this one.

## Paths are strings, distinguished by provenance

There is no path-literal syntax. Path-shaped values are ordinary strings:

```vixen
"src/main.rs"
"out/hello"
`out/${name}.o`
workspace "zoo/lua-src"
```

What makes a string a "path" is **provenance**, not quoting. A string produced
by projecting a `Tree` (`workspace "src/main.rs"`) carries tree-provenance, so
the runtime mounts it into the sandbox and mixes it into cache keys; a plain
`"-o"` does not. Build paths with `/` or backtick interpolation; `Path.join`
and `Path.split` live on the value type. Provenance is preserved through `Path`
joins and lost through arbitrary string munging.

## Calls and `with`

Most Vixen code reads like ordinary function calls:

```vixen
f(x)
g [a, b, c]
Tree %{ "bin/app" => binary }
Point { x: 1, y: 2 }
```

The practical rule is:

- `f { ... }` passes a record payload
- `f %{ ... }` passes a map payload
- `with { ... }` is keyword application

So these are different:

```vixen
exec [cc, "-c", file] with { mounts: %{ "src" => source } }
Point { x: 1, y: 2 }
Tree %{ "bin/app" => binary }
```

`with` is the only keyword-argument form.

## Blocks, bindings, and conditionals

Vixen uses expressions almost everywhere. Blocks are parenthesized:

```vixen
let result = (
  let message = "hello";
  if ok (
    message
  ) else (
    fail `not ok`
  )
)
```

The main things to notice:

- `let` introduces a binding
- blocks use `(` ... `)`, not `{ ... }`
- the last expression is the block's value
- `if` always has an `else`

This is why real observers often look like this:

```vixen
observer: (proc, vfs) => (
  if proc.exit_code != 0 (
    fail `process failed`
  ) else (
    vfs.subtree("out")
  )
)
```

## Closures, pipelines, and partial application

Closures use arrow syntax:

```vixen
(x) => x + 1
(proc, vfs) => vfs
```

They are common in combinators:

```vixen
config.platforms(env)
  .map((name) => Tree %{ name => compile_hello(config.executor_for_platform(name)) })
  .merge()
```

And in reusable partially applied helpers:

```vixen
let cexec = partial exec with {
  observer: (proc, vfs) => (
    if proc.exit_code != 0 (
      fail `process failed`
    ) else (
      vfs.subtree("out")
    )
  ),
};
```

That lets later calls supply only the changing parts:

```vixen
cexec [tc.cc, "-c", `src/${file}`, "-o", `out/obj/${file}.o`] with {
  mounts: %{ "src" => src },
  writable: ["out"],
}
```

## What belongs where

Keep this split in mind while reading the rest of the spec:

- the core language covers syntax, functions, closures, records, maps,
  control flow, types, and modules
- the evaluation chapter covers laziness, demand, forcing, memoization, and
  boundaries
- the standard library covers `main`, `exec`, `acquire`, `Blob`, `Tree`,
  `Path`, capability probes, and related runtime-backed APIs

## Read next

Once this guide feels natural, read the normative pages in this order:

1. [Core Language](core.md)
2. [Evaluation](build.md)
3. [Build and Runtime Standard Library](../stdlib/build.md)
