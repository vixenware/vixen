# Vixen language

Vixen is a strongly typed language. Every value has a type. Vixen has arrays,
maps, functions, and closures.

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

Vixen supports Rust-like structs

```vixen
struct Name {
  first: String,
  last: String,
}

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
struct FirstName {
  first: String,
}

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

You can only name functions that have only positional arguments:

```vixen
// type: Fn[(Array[U64],), U64]
fn sum(items: Array[U64]) -> U64 { /* ... *? }

// type: cannot be named, cannot take its value / pass it to
// another function, since it has keyword arguments (and they
// even have defaults)
fn greet(name: String, { cheerful: Bool = false }) -> String { /* ... */ }
```

Open question: special syntax for function types, a-la Rust?

```vixen
// long, annoying to write/read
Fn[(Array[U64],), U64]

// peaceful, centered, also annoying to parse
Fn(Array[U64]) -> U64
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

## Enums

Vixen has Rust-like enums. The main difference is that each variant is
an actual type:

```vixen
enum Color {
  Red,
  Green,
  Blue,
}

enum Expr {
  Literal(F64),
  Add(Expr, Expr),
}

enum Shape {
  Circle { radius: F64 },
  Rectangle { width: F64, height: F64 },
}
```

Variants can have no payload (`Color.Red`), a tuple payload (`Expr.Literal(3.0)`),
or a struct payload (`Shape.Circle { radius: 5.0 }`).

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

To build a variant value (narrow type):

```vixen
c = Shape.Circle { radius: 5.0 }; // c is of type Shape.Circle
```

To build an enum value (wide type), use the enum as a constructor. Inside
`Shape(...)`, variant names are in scope — no need to qualify them:

```vixen
s = Shape(Circle { radius: 5.0 }); // s is of type Shape
```

A `Shape.Circle` can be passed anywhere a `Shape` is expected:

```vixen
fn area(s: Shape) -> F64 {
  // ...
}

// both work:
area(Shape(Circle { radius: 5.0 }));
area(Shape.Circle { radius: 5.0 });
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
enum Option[T] {
  Some(T),
  None,
}
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
