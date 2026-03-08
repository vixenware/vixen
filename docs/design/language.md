# Vixen language

Vixen is a strongly typed language. Every value has a type.

## Conventions

All types are written in UpperCamelCase. Every binding is written in snake_case.

This is enforced by the Vixen evaluator. 

## Scalar types

Vixen supports Scalar types out of the box:
  * U64, I64, F64
  * String 

Vixen supports number and string literals:

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

## Functions

Freestanding functions can be defined with `fn`:

```vixen
fn add(a: U64, b: U64) -> U64 { a + b }

ten = add(2, 8)
```

Functions can take keyword arguments:

```vixen
fn greet(name: String, { cheerful: Bool = false }) -> String {
  // Well we haven't defined ifs but they work just like in Rust
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

```

## Closures

Vixen has support for closures

```vixen
fn repeat(amount: U64, f: Fn[])
```
