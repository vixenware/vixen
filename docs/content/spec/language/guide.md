+++
title = "Syntax Guide"
insert_anchor_links = "heading"
+++

# Vixen Syntax Guide

This page is a guide-level summary of the current language direction.
It is intentionally shorter and more example-driven than the normative
spec pages:

- [Core Language](core.md)
- [Build and Evaluation](build.md)

If this page and the spec disagree, the spec wins.

## Mental model

The language is organized around a few core ideas:

- Application is unary: `f x`, not `f(x)`.
- Chained application is left-associative: `f x y` means `(f x) y`.
- Dot syntax is only for fields and methods, never free-function UFCS.
- `where expr` is the only keyword-application form.
- Records and maps both use braces, but they are different value kinds.
- `!` means zero-arg invocation, not eagerness or forcing.

## Calls

The common forms are:

```vixen
f x
f (x, y)
f [x, y]
f { left: x, right: y }
```

That means:

```vixen
add 1 2
distance (x1, y1) (x2, y2)
Tree [compile @lua.c, liblua]
```

To navigate the result of a call, group it first:

```vixen
(discover_tool class).path
```

## Keyword arguments

Keyword application uses `where`:

```vixen
exec [cc, "-c", file] where { mounts: src }
exec [cc, "-c", file] where common
```

Inline records and record values are interchangeable here:

```vixen
let common = { verifier, observer }
let cexec = partial exec where common
```

Required and optional keywords are both part of the function's declared
call surface:

```vixen
fn exec[T](args: Array[ExecInput]) where {
  observer: Fn(Process, Tree) #sandbox -> Stream[T],
  verifier: Fn(Host) #host -> () = (_host) => (),
} -> Stream[T];
```

## Records and maps

Records use field labels:

```vixen
let common = { verifier, observer }
let point = { x: 1, y: 2 }
```

Maps use general key expressions:

```vixen
let ports = { 80: "http", 443: "https" }
let outputs = { @bin/lua: lua_exe, @lib/liblua.a: liblua }
```

Use this rule of thumb:

- `name: value` with field labels gives you a record
- general keys give you a map

Record spread works left-to-right:

```vixen
let tuned = { ..defaults, optimize: true }
cexec [cc, "-c", file] where { ..common, mounts: src }
```

## Constructors

Type names are constructor values in term position.

Examples:

```vixen
Blob "hello"
Point { x: 1.0, y: 2.0 }
Tree { @bin/lua: lua_exe, @lib/liblua.a: liblua }
```

That gives a few useful patterns:

- struct construction via record payloads
- enum variant construction via record payloads
- builtin construction via other payload kinds such as arrays or maps

So `Tree [ ... ]` is not special syntax. It is just `Tree` called with
an array literal.

## Fields, methods, and `!`

Dot navigation never falls back to free functions.

```vixen
config.package.name
tool.path
```

When the selected member is a zero-arg method:

```vixen
blob.utf8    // bound method value
blob.utf8!   // invoke it
```

`!` is only zero-arg invocation. It does not mean "force" in the
demand-driven evaluator.

Postfix chains are left-associative:

```vixen
blob.utf8!.trim!
```

To call a zero-arg free function, use the same operator:

```vixen
get_config!
get_config!.database.port
```

## Methods vs free functions

Methods live in namespaces:

```vixen
namespace Blob {
  fn Blob(s: String) -> Self;
  fn utf8(self) -> String;
  fn utf8_lossy(self) -> String;
}
```

Free functions chain with `|>`:

```vixen
cexec [ar, "rc", @out/liblua.a, ..objs]
  |> cexec [ranlib, _]
```

`_` is only the pipe placeholder.

## Closures and shorthand

Arrow closures are the explicit form:

```vixen
(tool) => verify_tool(host, tool)
```

`$` is closure shorthand:

```vixen
files.map ($.utf8!)
items.reduce ($0 + $1)
tools.values!.each (verify_tool host $)
```

Use this split:

- `_` for pipes
- `$` for closures
- `partial` when you are freezing part of a callable surface

## Partial application

`partial` freezes only what you write:

```vixen
let add_two = partial add 2
let cexec = partial exec where common
```

That means:

- written positional arguments are bound
- omitted positional arguments stay open
- written keyword arguments are bound
- unspecified keyword arguments stay open

## Paths

Two path forms are in play:

```vixen
@out/bin/lua
@(out/obj/{file}.o)
```

Use:

- bare `@path` for the common simple case
- `@( ... )` when interpolation is needed

Bare paths do not interpolate.

## A build-shaped example

```vixen
fn build(ws: Path, args: Array[String], env: Map[String, String]) #fs -> Tree (
  let src = Tree.from_path ws;
  let common = { verifier, observer };
  let cexec = partial exec where common;

  fn compile(file: Path) -> Tree (
    cexec [cc, "-c", "-o", @(out/obj/{file}.o), file] where { mounts: src }
  );

  let liblua = all_lib_sources.map compile |> _.merge!;
  let lua_exe = link Tree [compile @lua.c, liblua];

  Tree {
    @bin/lua: lua_exe,
    @lib/liblua.a: liblua,
  }
)
```

## Current open edges

These are still active design questions:

- exact constructor visibility rules for structs and enum variants
- the final wording of callable return transformation (`.then(...)` or similar)
- the exact lexical envelope of bare `@path`
- how much leading-dot method-adapter shorthand should remain, versus
  using `$` closures explicitly
