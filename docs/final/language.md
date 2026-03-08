# Vixen language

This document defines the Vixen language.

Vixen is a strongly typed language whose syntax is inspired by Rust.

There is no let, one can just assign bindings:

```vixen
// comments are C-like
a = 42; // semicolons separate statements
s = "A string";
t = `A string with ${a} interpolation`;
```

Built-in scalar types:

  * u64, i64
  * bool
  * String
  
Built-in composite types:

  * List[T]
  * Set[T]
  * Map[K, V] (sorted by K)
  * Stream[T]

Functions are declared with fn - arguments must be typed.

```vixen
fn add(l: u64, r: u64) -> u64 {
  a + b // like in Rust, last expression returns
}
```

All arguments are named:

```vixen
x = add(l: 3, r: 5);
```

Shorthand syntax exists:

```vixen
l = 3;
r = 5;
x = add(@l, @r);
```

Vixen has structs:

```vixen
struct Name {
  first: String,
  last: String,
}

name = Name { first: "John", last: "Doe" };
```

Struct literal syntax has shorthand:

```vixen
first = "John";
last = "Doe";
name = Name { @first, @last };

// or: (trailing commas allowed)

name = Name {
  @first,
  @last,
};
```

Structs can be generic:

```vixen
struct Name[S] {
  first: S,
  last: S,
}

let n = Name[String] { first: "", last "" };
```

Vixen has lists:

```vixen
let arr = [1, 2, 3];

two = arr[1]; // lists are 0-indexed
```

Lists have a known lengths, streams don't.

Methods can be declared with an `impl` block:

```vixen
impl u64 {
  fn double(self) -> u64 {
    self * 2
  }
}

let x = 16.double();
```

```vixen
impl[T] List[T] {
  fn sum(self) -> T {
    // we haven't talked about loops yet so that
    // one's gonna be hard
  }
}
```

Vixen has closures:

```vixen
fn odds(l: List[u64]) -> List[u64] {
  l.filter((x) => x % 2 == 1)
}

fn uppercase(m: Map[String, String]) -> Map[String, String] {
  m.filter((k, v) => k.to_uppercase() == k)
}
```

There's a shorthand for closures that take only one argument:

```vixen
fn odds(l: List[u64]) -> List[u64] {
  l.filter($ % 2 == 1)
}
```

Vixen has enums:

```vixen
enum CargoDep {
  Path { path: String },
  Git { git: String, branch: String }, // etc.
}

let dep1 = CargoDep.Path { path: "../my-crate" };
let dep2 = CargoDep.Git { git: "https://github.com/foo/bar", branch: "main" };
```

Enum variants are always structs, and they're always real, nameable types, you
can imagine them as:

```vixen
enum CargoDep {
  Path(CargoDep.Path)
  Git(CargoDep.Git)
}

struct CargoDep.Path {
  path: String 
}

struct CargoDep.Git {
  git: String, branch: String 
}
```

Vixen has builtin `Option[T]` and `Result[T, E]` like Rust.

Vixen has an `as` operator which allows narrowing an enum:

```vixen
path_dep = cargo_dep as CargoDep.Path; // evaluates to an Option<CargoDep.Path>
```

Vixen has value semantics:

```vixen
list = [1, 2, 3];

fn f(list: List[u64]) {
  list[2] = 53;
}

f(list)
list; // still is [1, 2, 3]
```

In fact:

```vixen
list = [1, 2, 3];

list[2] = 53;
// is a shortcut for
list = [..list, 2: 53];
```

`..` is the spread operator, it works on structs, too:

```vixen
struct Name {
  first: String,
  last: String,
}

name1 = Name { first: "John", last: "Doe" };
name2 = Name { ..name1, first: "Jane" }; // order isn't fixed like in Rust
```

Vixen is a build language, therefore not every value is available immediately.

Specifically, `exec`, a built-in, schedules work in a sandboxed environment using
a VFS, potentially on a remote node, 

```vixen
cmd = exec(
  // bad example: "sleep" might not be available, we're not specifying which build env, etc.
  argv: ["sleep", "5"],
);
cmd.status // accessing this suspends until it's available
```

Vixen's idea is that `cmd` _itself_ does not suspend, so one can use it to make
an entire build graph just by calling function recursively, and returning a top-level
result which depends on a bunch of exec succeeding.
