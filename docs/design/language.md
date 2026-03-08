# Vixen language

Vixen is a strongly typed language for build systems. Every value has a type.
Vixen has arrays, maps, functions, and closures.

## Core principles

**Value semantics.** There are no references, no borrowing, no ownership. Every
value can be used as many times as you want. Nothing is ever consumed or moved.
Passing a value to a function does not prevent you from using it again. There is
only `Fn` — no `FnOnce`, no `FnMut`. Iterating over an array does not consume it.

**Demand-driven execution.** There is demand at the top (what the build system
needs), and evaluation is driven downward from that demand. There is no separate
planning phase that builds a dependency graph and then an execution phase that
runs it — dependencies are discovered dynamically as evaluation proceeds. The
core side effect is `exec`, which runs commands on remote executors (see below).
Values produced by `exec` may not be immediately available; evaluation implicitly
suspends until the values it depends on resolve.

**Universal value properties.** Because Vixen has value semantics and no
references, every value can be compared for equality, hashed, and
serialized/deserialized. This is essential — closures and their captured values
must be serializable to ship to remote executors.

**Pure and impure code.** Most Vixen code is pure — it transforms values and
cannot compromise the build. Impure code has access to operations that affect
hermeticity and cacheability: reading process output, committing artifacts to
the store. This is Vixen's equivalent of Rust's safe/unsafe split. Impure
functions are explicitly marked with `impure fn`, and impure built-ins (like
`commit`) can only be called from impure contexts. Observer closures are
impure. Orchestrator code is pure. `exec` is the boundary between the two
worlds.

## Conventions

All types are written in UpperCamelCase. Every binding is written in snake_case.
This is enforced by the Vixen evaluator. 

Comments are `//` and `/*`. Statements are separated by semicolons. In blocks,
the last expression is what the block evaluates to (this includes functions).

## Scalar types

Vixen supports Scalar types out of the box:
  * U64, I64, F64
  * Bool
  * String 

Vixen supports various literals literals:

```vixen
a_lot = 4_U64; // explicit typing
not_pi = 3.14;
name = "John";
name = `John ${doe_as_a_var}`; // string interpolation is supported
```

## Newtypes

Vixen supports defining new types using newtype constructs. These behave the
same as the underlying scalar types, but require explicit conversion when
assigned. 

```vixen
newtype Blake3 of String;

hash = blake3_hash_of(something); // of type Blake3
print_string(hash); // error: expected String, got Blake3
print_string(hash as String); // error: expected String, got Blake3
```

More on the `as` operator later.

## Tuples

Vixen supports heterogeneous tuples like in Rust. Field access is exactly like in Rust with .0, .1, etc. 

```vixen
a = (5, 3);
eight = a.0 + a.1;
```

Tuples can be spread into other tuples:

```vixen
two_three = (2, 3);
one_to_four = (1, ..two_three, 4); // (1, 2, 3, 4)
```

Trailing commas are allowed, and not just for empty tuples:

```vixen
a = (,);
b = (1,);
c = (1, 2,);
```

Open quesiton what does `()` mean? Empty tuple (awkward) or unit value (which != empty tuple?).

## Structs

Vixen supports Rust-like structs. Types are always introduced with the `type`
keyword:

```vixen
type Name = struct {
  first: String,
  last: String,
};

name = Name { first: "John", last: "Doe" };
```

If you already have bindings with the right name, you can use shortcut syntax:

```vixen
first = "John";
last = "Doe";
name = Name { @first, @last };
```

If you already have another value with compatible fields you can spread it into the struct:

```vixen
type FirstName = struct {
  first: String,
};

fn = FirstName { first: "John" };
name = Name { ..fn, last: "Doe" };
```

You can assign to a field of a struct:

```vixen
name = Name { first: "John", last: "Doe" };
name.last = "Wilson"; // congrats
```

Although Vixen has value semantics, so this is actually sugar for:

```vixen
// declares a binding (a name for a value)
name = Name { first: "John", last: "Doe" };

// this shadows the first binding with a new value, built from the first one.
name = Name { ..name, last; "Wilson" };
```

### Nested type declarations

Types can be declared inline in field position using the same `type X = ...`
syntax. The declared type lives under the parent's namespace and can be
referenced by name elsewhere:

```vixen
type CargoToml = struct {
  package: type Package = struct {
    name: String,
    version: String,
    edition: String?,
  },
  dependencies: Map[String, type DependencySpec = String | struct {
    version: String?,
    path: String?,
    git: String?,
    features: Array[String]?,
  }]?,
  dev_dependencies: Map[String, DependencySpec]?,
  bin: Array[type BinTarget = struct {
    name: String,
    path: String?,
  }]?,
  lib: type LibTarget = struct {
    name: String?,
    path: String?,
  }?,
  workspace: type Workspace = struct {
    members: Array[String]?,
  }?,
};
```

Nested named types are accessible through the parent namespace:
`CargoToml.Package`, `CargoToml.DependencySpec`, `CargoToml.BinTarget`, etc.

The `DependencySpec` sum type shows how inline definitions compose with the
rest of the type system — it's defined where `dependencies` first uses it,
then reused by name for `dev_dependencies`. The anonymous struct inside the
sum type doesn't need a name since it's only accessed through `DependencySpec`.

## Deserialization

Vixen has built-in parsers for structured data formats (JSON, TOML, etc.).
Parsing is driven by type — you provide a type parameter and the parser
produces a value of that type:

```vixen
manifest = Toml.parse[CargoToml](src.read("Cargo.toml"));
manifest.package.name   // "my-crate"
manifest.bin            // Array[CargoToml.BinTarget]?
```

The same pattern works for other formats:

```vixen
config = Json.parse[Config](some_blob);
```

The type drives deserialization — field names map to keys, `Option` fields
are optional, sum types handle polymorphic values (like `DependencySpec`
being either a string or a table in TOML).

## Control flow

`if`/`else` works like in Rust — no parentheses around the condition, braces
are required:

```vixen
if x > 0 {
  "positive"
} else if x == 0 {
  "zero"
} else {
  "negative"
}
```

`if`/`else` is an expression — it evaluates to a value:

```vixen
label = if x > 0 { "positive" } else { "negative" };
```

`for` iterates over anything iterable (arrays, ranges, maps):

```vixen
for item in [1, 2, 3] {
  // ...
}

for i in 0..5 {
  // i takes values 0, 1, 2, 3, 4
}

for key, value in some_map {
  // ...
}
```

## Functions

Freestanding functions can be defined with `fn`:

```vixen
fn add(a: U64, b: U64) -> U64 { a + b }

ten = add(2, 8)
```

Functions can take keyword arguments:

```vixen
fn greet(name: String, { cheerful: Bool = false }) -> String {
  if cheerful {
    `A heartily welcome to ${name}!`
  } else {
    `Hi, ${name}.`
  }
}

g1 = greet("Winter");
// note: keyword arguments go _after_ positional arguments, always.
g2 = greet("Spring", cheerful = true);
```

The last expression in a function body is its return value. `return` can
be used for early exit:

```vixen
fn abs(x: I64) -> I64 {
  if x < 0 {
    return -x;
  }
  x
}
```

## Arrays

Vixen has array literals:

```vixen
a = [1, 2, 3]; // an array of U64
```

In order to describe the type of arrays, Vixen must have generics:

```vixen
fn sum(items: Array[U64]) -> U64 {
  acc = 0
  for i in items {
    acc += i
  }
  acc
}
```

## Maps

Vixen has map literals:

```vixen
// this makes a Map<String, F64>
loc = { "lat": 53.156, "lng": 145.23 };
```

Keys can be any Vixen type, because all Vixen types are comparable for equality and hashable:

```vixen
john = Name { first: "John", last: "Doe" };
jane = Name { first: "Jane", last: "Doe" };
rsvp = { john: false, jane: true }
```

## The type of functions

Function types use `Fn(Args) -> Ret` syntax:

```vixen
// type: Fn(Array[U64]) -> U64
fn sum(items: Array[U64]) -> U64 { /* ... */ }
```

Only functions with positional arguments can have their type named. Functions
with keyword arguments cannot be passed as values:

```vixen
// type: cannot be named, cannot take its value / pass it to
// another function, since it has keyword arguments (and they
// even have defaults)
fn greet(name: String, { cheerful: Bool = false }) -> String { /* ... */ }
```

## Ranges

Vixen has range literals similar to Rust

```vixen
a = 0..10;       // Range[U64]
b = 0..=10;      // RangeInclusive[U64]
c = ..10;         // RangeTo[U64]
d = ..=10;        // RangeToInclusive[U64]
e = 0..;          // RangeFrom[U64]
f = ..;           // RangeFull
```

Ranges can be used in `for` loops:

```vixen
for i in 0..5 {
  // i takes values 0, 1, 2, 3, 4
}
```

Ranges can also be used for slicing arrays:

```vixen
arr = [10, 20, 30, 40, 50];
slice = arr[1..4]; // [20, 30, 40]
```

## Closures

Functions can be defined as top-level and passed to other functions:

```vixen
fn find(haystack: Array[String], predicate: Fn(String) -> Bool) -> String? {
  for item in haystack {
    if predicate(item) {
      return item;
    }
  }
  None
}

fn starts_with_ch(name: String) -> Bool {
  name.starts_with("ch")
}

names = ["alice", "bob", "charlie"];
found = find(names, starts_with_ch);
// found is "charlie"
```

Anonymous functions (closures) work too:

```vixen
found = find(names, fn(name) { name.starts_with("ch") });
```

There's a shorter arrow syntax (no pipes like in Rust, parens like in JS):

```vixen
found = find(names, (name) => name.starts_with("ch"));
```

And for single-argument closures, `$` refers to the argument:

```vixen
found = find(names, $.starts_with("ch"));
```

## Methods

In addition to freestanding functions, one can define methods on a
given type:

```vixen
fn Name.full(self) -> String {
  `${self.first} ${self.last}`
}

name = Name { first: "John", last: "Doe" };
name.full() // "John Doe"
```

### Static methods

Methods that don't take `self` are static — they are called on the type,
not on a value:

```vixen
fn Blob.from_string(s: String) -> Blob {
  // ...
}

b = Blob.from_string("Hello, world!");
```

Static methods are how constructors work. The type itself acts as a namespace.

## Enums

Vixen has Rust-like enums. The main difference is that each variant is
an actual type.

### Variant kinds

Variants can have no payload, a tuple payload, or a struct payload:

```vixen
type Color = enum {
  Red,
  Green,
  Blue,
};

type Expr = enum {
  Literal(F64),
  Add(Expr, Expr),
};

type Shape = enum {
  Circle { radius: F64 },
  Rectangle { width: F64, height: F64 },
};
```

Each variant is a type under the enum's namespace. `Shape.Circle` is a type,
and `Shape` is a type. They are distinct. This applies to all variant kinds:

- `Color.Red` — a zero-sized type
- `Expr.Add` — a tuple type, fields accessed with `.0`, `.1`
- `Shape.Circle` — a struct type, fields accessed by name (`.radius`)

```vixen
node = Expr.Add(Expr(Literal(1.0)), Expr(Literal(2.0)));
node.0 // the left operand, type: Expr
node.1 // the right operand, type: Expr

c = Shape.Circle { radius: 5.0 };
c.radius // 5.0
```

### Alias variants

In addition to the three payload kinds above, a variant can be an alias for
an existing type, using colon syntax:

```vixen
type Entry = enum {
  File: Blob,
  Dir: Tree,
};
```

`Entry.File` is an alias for `Blob` — an `Entry.File` value *is* a `Blob`.
`Entry.Dir` is an alias for `Tree`. This is useful when building enums out
of types that already exist, giving them new names within the enum.

If you don't need to rename, the `@` shorthand reuses the type name as the
variant name:

```vixen
type Entry = enum {
  @Blob,
  @Tree,
};
```

This is equivalent to `Blob: Blob, Tree: Tree` — the variant `Entry.Blob`
is an alias for `Blob`, and `Entry.Tree` is an alias for `Tree`.

### Sum type shorthand

For the common case of "one of these types, no renaming needed," there's
a one-line form:

```vixen
type Entry = Blob | Tree;
```

This is equivalent to the `@` form above. The variant names are the type
names.

### Building enum values

To build a variant value (narrow type):

```vixen
c = Shape.Circle { radius: 5.0 }; // c is of type Shape.Circle
```

To build an enum value (wide type), use the enum as a constructor. Inside
`Shape(...)`, variant names are in scope — no need to qualify them:

```vixen
s = Shape(Circle { radius: 5.0 }); // s is of type Shape
```

### Auto-widening

A `Shape.Circle` can be passed anywhere a `Shape` is expected:

```vixen
fn area(s: Shape) -> F64 {
  // ...
}

// both work:
area(Shape(Circle { radius: 5.0 }));
area(Shape.Circle { radius: 5.0 });
```

More generally, if a variant wraps exactly one type (tuple variant with one
field, or an alias variant), values of that inner type auto-widen into the
variant, and from there into the enum. This chains:

```vixen
type Entry = Blob | Tree;

// Blob auto-widens to Entry.Blob, which auto-widens to Entry:
fn process(e: Entry) { /* ... */ }
process(some_blob); // ok — Blob → Entry.Blob → Entry

// Works in collections too:
entries = { "src": some_tree, "readme.txt": some_blob };
// Map[String, Entry] — each value auto-widened
```

But the reverse is not true — a function that wants a specific variant only
accepts that variant:

```vixen
fn circumference(c: Shape.Circle) -> F64 {
  2.0 * 3.14159 * c.radius
}

circumference(Shape.Circle { radius: 5.0 }); // ok
circumference(Shape(Circle { radius: 5.0 })); // error: expected Shape.Circle, got Shape
```

### Type aliases for variants

If a variant name is used often, a type alias can bring it to the top level:

```vixen
type Circle = Shape.Circle;

c = Circle { radius: 5.0 };        // a Circle (aka Shape.Circle)
s = Shape(Circle { radius: 5.0 }); // a Shape
```

## The `as` operator

`as` converts between types. The conversion must succeed — if it doesn't,
it's an error:

```vixen
// narrowing an enum to a variant
s = Shape(Circle { radius: 5.0 });
c = s as Shape.Circle; // ok, c is of type Shape.Circle
r = s as Shape.Rectangle; // error: s is a Circle, not a Rectangle
```

This also works for newtypes:

```vixen
newtype Blake3 of String;
hash = blake3_hash_of(something);
raw = hash as String; // ok
```

## Option

`Option` is a built-in enum:

```vixen
type Option[T] = enum {
  Some(T),
  None,
};
```

`T?` is sugar for `Option[T]`:

```vixen
fn first_even(items: Array[U64]) -> U64? {
  for item in items {
    if item % 2 == 0 {
      return item;
    }
  }
  None
}
```

## The `as?` operator

`as?` is the fallible version of `as`. Instead of erroring, it returns
an `Option`:

```vixen
s = Shape(Circle { radius: 5.0 });
c = s as? Shape.Circle;    // Option(Some(Shape.Circle { radius: 5.0 }))
r = s as? Shape.Rectangle; // Option(None)
```

## Streams

`Stream[T]` is a built-in generic type that produces values of type `T` over
time. A stream is constructed using `yield`:

```vixen
fn count_up(n: U64) -> Stream[U64] {
  for i in 0..n {
    yield i;
  }
}
```

`yield` produces a value into the stream. The function's return type is
`Stream[T]` where `T` matches the type of the yielded values.

Streams support operations like `find`, `filter`, and `map`:

```vixen
s = count_up(10);
evens = s.filter($ % 2 == 0);    // Stream[U64]
first_big = s.find($ > 5);       // U64 — but may be pending
```

A stream operation that produces a single value (like `find`) returns a
potentially pending value. A stream operation that produces multiple values
(like `filter`, `map`) returns another `Stream`.

## Store types

`Blob` is a single file in the content-addressable store (keyed by Blake3
hashes). `Tree` is a directory with its full structure and filenames preserved.

There is no separate "local tree" vs "store tree" type. A `Tree` might be
backed by a local directory on disk, by the CAS, or by a mix. The runtime
handles materialization transparently. When a `Tree` backed by a local
directory is needed by an `exec` on a remote executor, the runtime lazily
hashes and commits only the parts that need to move — content-defined chunking,
merkle trees, and CAS dedup make the transfer efficient. Only missing chunks
are uploaded.

The Vixen language never exposes Blake3 hashes. The hash is an implementation
detail of the store, not a language-level concept.

### Blob constructors

`Blob.from_string` creates a blob from a text string:

```vixen
readme = Blob.from_string("# My Project\n\nHello.");
```

Blobs with binary content are not constructed in the language — they come from
`exec` + `commit` (a tool produces a file, the observer commits it to the
store).

### Tree constructors

`Tree.of` creates a tree from a map of names to entries. Entries can be
blobs or other trees — auto-widening handles the conversion:

```vixen
type Entry = Blob | Tree;

output = Tree.of({
  "README.md": Blob.from_string("# Hello"),
  "src": src_tree,
});
```

Trees can also be derived from other trees:

```vixen
// select a subtree
lib_src = src.subtree("src/lib");

// filter by glob
rs_files = src.glob("**/*.rs");

// merge two trees (later entries win on conflict)
combined = tree_a.merge(tree_b);
```

These operations are lazy — they describe a new tree without copying or
hashing anything. The runtime materializes only what's needed at `exec`
boundaries.

## Paths

`Path` is its own type — not a newtype of `String`. Root paths come from
`.at()` on store types, which gives them a location in the VFS (relative
to the working directory). Sub-paths are built with the `/` operator:

```vixen
src = src_tree.at("src");          // Path — mount this tree at ./src
entry = src / "lib.rs";            // Path — ./src/lib.rs

deps = deps_tree.at("deps");      // Path — mount at ./deps
rlib = deps / "libfoo.rlib";      // Path — ./deps/libfoo.rlib

rmeta = rmeta_blob.at("libdep.rmeta");  // Path — a single file
```

A `Blob` has no name — `.at()` gives it one. A `Tree` already has names
inside it — `.at()` sets its mount point.

Paths carry their store references. When a `Path` appears in an `exec`
argument (via string interpolation or directly), the runtime knows which
blobs and trees need to be materialized in the VFS.

```vixen
// the path carries the blob reference + desired name
arg = `--extern=dep=${rmeta}`;

// the path carries the tree reference + mount point
entry_arg = `${entry}`;
```

String interpolation with a `Path` produces a structured value (not a plain
`String`) that preserves the store dependencies. The exact type of `exec`
args is an open question, but it must support both plain strings and these
structured values. The same applies to `env` values — an environment
variable might define an include path pointing to mounted content.

## Exec

`exec` is the core built-in for running external processes. It runs a command
on an executor — a sandboxed runner with a virtual filesystem, potentially a
VM in the cloud.

The executor provides a working directory backed by the VFS. All mount paths
are relative to this working directory — there are no absolute paths in the
VFS. The sandbox also provides read-only access to system paths (toolchains
like Xcode, MSVC, GCC) which are fingerprinted but not controlled by Vixen.
This design works across Linux, macOS, and Windows without requiring root
or filesystem namespace control.

```vixen
// Open question: the exact types for args and env values — they must
// support both plain strings and structured values carrying store refs.
exec[T](
  args: Array[...],
  env: Map[String, ...],
  mounts: Map[Path, Mount],
  observer: impure Fn(Process) -> Stream[T],
) -> Stream[T]
```

`exec` is generic over `T`. The observer closure determines `T`, and `exec`
returns the same `Stream[T]` to the caller.

### The observer closure

The observer runs **on the executor**, not the orchestrator. It is serialized
along with its captured values and shipped to the executor. A closure cannot
be sent until all its captures are resolved — attempting to serialize a
pending value is just another form of depending on it, so it suspends.

The observer receives a `Process` handle and returns a stream of events:

- `process.stdout` — standard output of the running process
- `process.stderr` — standard error of the running process

`commit` is a separate top-level built-in that commits a path from the
virtual filesystem to a shared content-addressable store, returning a `Blob`
(a Blake3 hash reference). The store is ambient — both orchestrator and
executor have access to it. Only lightweight references (Blake3 hashes) flow
between them.

```vixen
impure fn commit(path: Path) -> Blob
```

### Observer patterns

**Streaming observer** — for tools like `rustc` that emit structured output
during execution. The observer parses stdout incrementally, commits artifacts
as they appear, and yields events one at a time. This allows dependents to
start work early. For example, downstream crates can begin compiling as soon
as `rustc` emits its metadata file, without waiting for code generation to
finish.

**Simple observer** — for tools where you only care about the final result.
The observer waits for the process to exit, commits the output directory,
and yields a single event.

### Pending values

`exec` returns a `Stream[T]`. Operations on that stream — like `find`,
`filter`, `map` — produce values that may not be immediately available. Code
that depends on a pending value implicitly suspends until the value resolves.
There is no explicit `await` or special syntax.

```vixen
artifacts = exec[RustcArtifact](...);

metadata = artifacts.find($.kind == "metadata");
lib = artifacts.find($.kind == "lib");

// this call suspends until both metadata and lib are available
result = compile_dependent(metadata, lib);
```

When a function call depends on multiple pending values, all of them must
resolve before the call proceeds. There is no `race` or `select` — the
runtime implicitly joins on all dependencies.

### Memoization and caching

Because all values are hashable and comparable, the runtime can memoize any
pure function call transparently. A pure function called with the same
arguments always produces the same result — the runtime caches it
automatically, keyed on (function, arguments).

`exec` is a special case because it is impure. Its cache key includes the
runner fingerprint in addition to the declared inputs: **(args, env, mounts,
observer + captures, runner fingerprint)**. If all of these match a previous
execution, the runtime replays the cached stream of events without running the
process again.

Most `exec` inputs are declared explicitly. But some come from the runner
itself — toolchain versions like Xcode, MSVC, GCC, etc. These are not mounted
or installed by Vixen. They are whatever is on the runner. But they are
fingerprinted and tracked: if the toolchain version changes, the cache key
changes and the result is invalidated. Runner-contributed inputs are not
visible in the Vixen language — the runner reports its fingerprint and the
runtime incorporates it into the cache key automatically.

## Demand model

A Vixen build starts with a `build.vixen` file at the repository root. The
entry point is a function that receives the working directory as a `Tree`,
the command line arguments, and the process environment, and returns a `Tree`:

```vixen
fn build(src: Tree, args: Array[String], env: Map[String, String]) -> Tree {
  std.rust.workspace(src, args, env)
}
```

- `src` — the working directory as a lazy `Tree` (backed by the local
  filesystem, nothing hashed until an `exec` needs it)
- `args` — raw CLI arguments as an `Array[String]`, not pre-parsed. The
  Vixen code decides how to interpret them.
- `env` — the process environment as a `Map[String, String]`

The returned `Tree` is the build output. The runtime materializes it to disk —
for a Rust build, this would be a tree containing `target/debug/foo`,
`target/release/libbar.rlib`, etc. The runtime diffs what's already on disk
with the output tree and writes only what changed.

There is no separate planning phase that enumerates all possible targets.
The `args` tell the build what's demanded (`--bin foo`, `--release`, etc.),
and evaluation proceeds lazily from that demand. Only the subgraph needed to
produce the requested outputs is ever evaluated.

### Cargo compatibility

If no `build.vixen` exists, Vixen looks for a `Cargo.toml`. If found, it
evaluates as if the thin build file above were present — calling
`std.rust.workspace` with the working directory, CLI args, and environment.
Target selection
uses the same flags as cargo: `--bin`, `--lib`, `--package`, `--example`,
`--test`, `--release`, `--target`, `--features`, etc.

This means `vx build` is a drop-in replacement for `cargo build` on any
existing Rust project. No configuration needed.

### Source file discovery

`std.rust.workspace` handles discovering which files belong to a crate. It
reads `Cargo.toml`, follows conventional paths (`src/`, `tests/`, `benches/`),
and determines what the crate's inputs are.

Simple globbing is insufficient — crates can pull in arbitrary files via
`include_bytes!`, `include_str!`, `build.rs`, proc macros, etc. The default
behavior does its best, but it cannot cover cases that require full macro
evaluation.

For those cases, users write a `build.vixen` that explicitly declares
additional inputs. This is the escape hatch: the default works for most crates,
and custom build files handle the rest.
