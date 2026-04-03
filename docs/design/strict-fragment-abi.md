# Strict Fragment ABI

This is draft design material for the boundary between:

- the orchestrator/runtime value world
- compiled strict fragments lowered through `vx-jit`

This is not yet normative spec text.

## Terms

- **strict fragment**: a strict, local fragment of Vixen computation that can be
  interpreted or JIT-compiled independently of the larger orchestrated
  evaluation.
- **runtime value**: the orchestrator's value representation. This is the
  dynamic, implementation-level value world that carries Vixen values across
  evaluation and RPC boundaries.
- **ABI word**: one machine-sized argument/return slot in the current compiled
  calling convention. Today this is `u64`.

## Why This Exists

The orchestrator/runtime does not evaluate everything through the same direct
calling convention as compiled strict fragments.

That mismatch is real and expected:

- Vixen values have value semantics.
- The orchestrator/runtime value world is dynamic.
- Strings, arrays, maps, trees, closures, and future enums/structs are not
  native Rust ABI values in any stable external sense.

So the strict fragment ABI is not trying to match:

- Rust crate APIs
- C ABI for arbitrary authored types
- the full orchestrator runtime representation

It is trying to provide:

1. a small, explicit calling convention for compiled strict fragments
2. a formal lowering/lifting boundary between runtime values and compiled code
3. a layout model that can grow to structs and enums without more ad hoc cases

## Cost Model

Yes, there is marshaling cost at this boundary if the strict fragment ABI does
not match the runtime representation exactly.

That cost is unavoidable in some form, because the runtime world is dynamic and
value-semantics-first. We are not trying to make runtime values be Rust-native
ABI values.

The right optimization target is therefore not:

- "zero cost against arbitrary outside code"

It is:

- minimize crossings
- make each crossing explicit and mechanically simple
- compile larger strict fragments so the boundary is crossed rarely
- keep the ABI layout rules stable and predictable

In practice:

- paying one marshaling step at strict fragment entry/exit is acceptable
- paying that cost for every internal operation would not be

So the design pressure is to make strict fragments large enough that the
boundary is coarse, not microscopic.

## Current Implemented ABI Domain

Today the implemented strict fragment ABI only covers:

- `U64`
- `Bool`
- `String`

This is a deliberately narrow current bridge, not the final ABI.

## Current Implemented Layout Rules

### ABI Word

One ABI word is one `u64` slot.

Arguments are flattened into a sequence of ABI words.
Returns are flattened into one or more ABI words.

### `U64`

Layout:

- size in ABI words: `1`
- word sequence: `[value]`

### `Bool`

Layout:

- size in ABI words: `1`
- word sequence: `[0 | 1]`

Any other returned word value is invalid.

### `String`

Current ABI layout:

- size in ABI words: `2`
- word sequence: `[ptr, len]`

#### Parameter semantics

For string parameters:

- `ptr` points to UTF-8 bytes
- `len` is the byte length
- the bytes are borrowed for the duration of the strict fragment call

#### Return semantics

For string returns:

- the compiled fragment returns `[ptr, len]`
- this is currently treated as an **unmanaged returned string**

That means:

- the caller may copy the bytes into an owned runtime string
- the caller cannot yet generally prove who owns the backing storage
- the caller cannot yet generally free that storage safely

This is the main unresolved part of the current ABI.

## Current Execution Model

At the strict fragment boundary, the runtime does two operations:

1. **lower runtime values to ABI words**
2. **lift ABI words back to runtime values**

Current lowering:

- `U64` -> `1` word
- `Bool` -> `1` word
- `String` -> `2` words (`ptr`, `len`)

Current lifting:

- `1` word -> `U64` or `Bool` depending on the fragment return type
- `2` words -> unmanaged string return for `String`

The important point is:

- semantic type is preserved by the strict fragment type information
- only the call boundary is word-flattened

This is not "everything is a `U64`".

It is:

- every ABI-visible value has a layout in ABI words

## Planned General Model

The current ad hoc cases should grow into a formal layout model.

The next step should be an explicit layout description, conceptually something
like:

- scalar layouts
- product layouts
- sum layouts
- string/blob/tree handle layouts

The key property is:

- strict fragment type -> ABI layout descriptor

Then lowering/lifting is driven by layout, not by hand-written per-signature
matching.

## Planned Layout Rules

These are the intended rules, not all implemented yet.

### Struct

A struct layout is the concatenation of its field layouts in declaration order.

Example:

```text
struct Pair {
  a: U64,
  b: Bool,
}
```

would flatten as:

- `[a, b]`

where `b` is encoded as `0 | 1`.

### Enum

An enum layout requires:

- a discriminant/tag
- a payload layout

The likely shape is:

- `[tag, payload...]`

where `payload...` has enough ABI words for the largest variant payload.

This is not implemented yet, but it is the correct formal direction.

### Nested Types

Nested structs/enums flatten recursively according to their layout descriptors.

## Runtime Representation Is Separate

The strict fragment ABI should not be confused with the orchestrator/runtime
representation.

The runtime may use:

- persistent structures
- reference-counted strings
- dynamic tagged values
- content-addressed tree/blob handles

The strict fragment ABI only defines:

- how selected values cross into compiled strict fragments
- how results cross back out

These are different concerns.

## Current Unresolved Questions

### 1. Returned string ownership

This is the biggest unresolved ABI question today.

We need a formal rule for string results returned by compiled strict fragments:

- borrowed return
- transient owned return
- caller-freed return
- allocator-paired return
- or another explicit ownership model

Right now we only have:

- unmanaged `[ptr, len]`
- copy-out is possible
- ownership is not yet formalized

### 2. Full layout descriptors

We need an explicit layout representation for:

- structs
- enums
- future fragment-visible handles

### 3. Runtime value bridge

We need a first-class runtime value <-> strict fragment lowering layer instead
of letting the orchestrator own ad hoc conversions.

## Near-Term Plan

1. keep the current implemented ABI for `U64`, `Bool`, and `String`
2. formalize returned string ownership
3. define layout descriptors for structs and enums
4. move the runtime value lowering/lifting logic behind a dedicated bridge layer
5. use that bridge from the orchestrator when calling compiled strict fragments

