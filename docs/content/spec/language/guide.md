+++
title = "Syntax Guide"
insert_anchor_links = "heading"
+++

# Vixen Syntax Guide

This page is a guide-level overview of the current syntax direction.
It teaches the raw expression forms first, then the real sugar on top of
them.

If this page and the normative spec disagree, the spec wins:

- [Core Language](core.md)
- [Build and Evaluation](build.md)

## 1. What counts as one value

Vixen application is unary, so the first thing to learn is what can act
as a single payload.

### Parentheses

```vixen
(expr);          // grouping
(left, right);   // tuple
(
  let x = 1;
  x + 1
);               // block expression
```

### Brackets

```vixen
[a, b, c];       // array
```

### Braces

```vixen
{ x: 1, y: 2 };                       // record
{ 80: "http", 443: "https" };         // map
{ @bin/lua: lua_exe, @lib/lib.a: a }; // map with Path keys
```

Use braces for both records and maps:

- field-name keys give you a record
- general expression keys give you a map

### Paths

```vixen
@out/bin/lua;
@(out/obj/{file}.o);
```

Use:

- bare `@path` for the simple case
- `@( ... )` when interpolation is needed

Bare paths do not interpolate.
Their lexical envelope is the simple ASCII set
`[A-Za-z0-9_./-]+`.
Inside `@( ... )`, `{expr}` interpolates and `{{` / `}}` produce
literal braces.

## 2. Unary application

The base call form is:

```vixen
f x
```

That one payload can itself be a tuple, array, record, map, path, or
block:

```vixen
f (x, y);
f [x, y];
f { left: x, right: y };
f {
  @bin/lua: lua_exe,
  @lib/liblua.a: liblua,
};
f (
  let tmp = build_input;
  tmp
);
```

Chained application is left-associative:

```vixen
f x y    // means: (f x) y
```

So `f x y` is only valid when `f x` itself returns a callable.

To navigate the result of an application, group it first:

```vixen
(discover_tool class).path
```

## 3. Fields, methods, and `!`

Dot syntax is only for fields and methods.

```vixen
x.y;
x.y.z;
```

Zero-arg invocation uses postfix `!`:

```vixen
blob.utf8!;
get_config!;
get_config!.database.port;
```

The raw form is:

```vixen
(blob.utf8)!
```

So `!` means only:

- invoke a zero-arg free function
- invoke a zero-arg bound method

It does **not** mean “force now” in the demand-driven evaluator.

Postfix chains are left-associative:

```vixen
blob.utf8!.trim!
```

## 4. Methods and namespaces

Methods live in type namespaces:

```vixen
namespace Blob {
  fn Blob(s: String) -> Self;
  fn utf8(self) -> String;
  fn utf8_lossy(self) -> String;
}
```

Dot never falls back to free-function UFCS. If you want to chain free
functions, use `|>`.

## 5. Sugar: multi-parameter declarations

This declaration:

```vixen
fn add(a: U64, b: U64) -> U64
```

is sugar for a function whose one payload is a tuple.

So the call site is:

```vixen
add (1, 2)
```

not:

```vixen
add 1 2
```

Likewise:

```vixen
fn distance(p1: (F64, F64), p2: (F64, F64)) -> F64
```

is called as:

```vixen
distance ((x1, y1), (x2, y2))
```

## 6. Records and keyword application

First, a function can declare keyword parameters with `where`:

```vixen
fn exec[T](args: Array[ExecInput]) where {
  observer: Fn(Process, Tree) #sandbox -> Stream[T],
} -> Stream[T];
```

Then the call site uses:

```vixen
where expr
```

where `expr` evaluates to a record.

So these are both valid:

```vixen
exec [cc, "-c", file] where { mounts: src };
exec [cc, "-c", file] where common;
```

The second form is possible because records are real values:

```vixen
let common = { observer };
```

Record shorthand:

```vixen
{ observer }
```

means:

```vixen
{ observer: observer }
```

Record spread also works inside keyword application:

```vixen
exec [cc, "-c", file] where { ..common, mounts: src }
```

## 7. Constructors

Constructors are ordinary unary callables.

These are ordinary calls:

```vixen
Blob "hello"
Point { x: 1.0, y: 2.0 }
Tree [compile @lua.c, liblua]
Tree {
  @bin/lua: lua_exe,
  @lib/liblua.a: liblua,
}
```

So:

```vixen
Tree [ ... ]
```

is not a special form. It is just `Tree` applied to an array payload.

And:

```vixen
Tree { ... }
```

is just `Tree` applied to a map payload.

Named-field constructors take record payloads:

```vixen
Point { x: 1.0, y: 2.0 }
Shape.Circle { radius: 5.0 }
```

`Point { ... }` produces a `Point`. `Shape.Circle { ... }` produces a
`Shape.Circle`, which widens to `Shape` only when a checking context
asks for the parent enum.

Opaque declarations do not synthesize constructors:

```vixen
type Tree;
```

If an opaque type supports construction, it declares that surface
explicitly. That is why `Tree [ ... ]` and `Tree { ... }` can exist even
though `Tree` is not a visible struct.

## 8. Sugar: pipe

Pipe is sugar for threading a value into the next stage.

```vixen
x |> f
```

means:

```vixen
f x
```

Use `_` when the piped value does not go in the default position, or
when the stage needs to navigate from the current value:

```vixen
get_blob my_blob |> _.utf8!
```

means:

```vixen
(get_blob my_blob).utf8!
```

Likewise:

```vixen
x |> pair(y, _)
```

means:

```vixen
pair(y, x)
```

For multiline pipelines, just write an ordinary multiline binary chain:

```vixen
let liblua =
  objs
  |> cexec [ar, "rc", @out/liblua.a, .._]
  |> cexec [ranlib, _];
```

Use `_` only in pipes.

## 9. Sugar: closure shorthand

This:

```vixen
(tool) => verify_tool(host, tool)
```

can be written as:

```vixen
verify_tool (host, $)
```

More examples:

```vixen
files.map ($.utf8!);
items.reduce ($0 + $1);
```

Use `$` only for closures.

## 10. Sugar: partial application

This:

```vixen
(args) => exec args where common
```

can be written as:

```vixen
partial exec where common
```

That means:

- written positional applications are bound
- omitted positional applications stay open
- written keyword arguments are bound
- unspecified keyword arguments stay open

## 11. Sugar: callable return transformation

This:

```vixen
(args) => parse (load args)
```

can be written as:

```vixen
load.then parse
```

`.then` keeps the callable's positional and keyword surface and changes
only what happens to its return value.

## 12. A build-shaped example

```vixen
fn build(ws: Path, args: Array[String], env: Map[String, String]) #fs #discover -> Tree (
  let src = Tree.from_path ws;
  let common = { observer };
  let cexec = partial exec where common;

  fn compile(file: Path) -> Tree (
    cexec [cc, "-c", "-o", @(out/obj/{file}.o), file] where { mounts: src }
  );

  let liblua =
    all_lib_sources.map compile
    |> _.merge!;
  let lua_exe = link Tree [compile @lua.c, liblua];

  Tree {
    @bin/lua: lua_exe,
    @lib/liblua.a: liblua,
  }
)
```

## Intentionally unspecified for now

These are intentionally postponed details:

- constructor visibility rules are not specified yet
