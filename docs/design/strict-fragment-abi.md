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

## Representation Goal

The goal is **not** to invent one representation for the runtime and a
different representation for compiled strict fragments.

The goal is:

- for every type admitted to the strict fragment ABI, define one canonical
  **payload representation**
- have both the runtime and compiled strict fragments use that same payload
  representation
- allow the runtime to carry additional tracking data **outside** that payload
  representation when needed

So the intended model is:

- shared payload layout
- optional runtime-only envelope

Examples of runtime-only envelope data might include:

- reference counts
- tracing/GC metadata
- dynamic type tags
- provenance/debug metadata
- pending/lazy bookkeeping

Compiled strict fragments should not need to know about that envelope if they
can operate directly on the payload representation.

This means the preferred direction is:

- minimize semantic reshaping at the strict fragment boundary
- keep ABI lowering/lifting mostly structural
- avoid separate “runtime layout” versus “JIT layout” for the same fragment-visible type

In short:

> strict-fragment-visible types should have a shared canonical payload
> representation across the runtime and JIT; runtime-only tracking may exist
> around that payload, but not redefine it.

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

Given the representation goal above, the preferred end state is actually
stronger than “accept marshaling cost”:

- use the same payload representation inside and outside compiled strict fragments
- pay boundary cost only for:
  - dynamic envelope handling
  - ownership transitions
  - forcing/copying where semantics require it

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

## String Target Model

`String` is the first fragment-visible type that needs an explicit ownership
story, so the target model is stated here directly.

### Canonical payload

The canonical payload representation of `String` is:

- `ptr: u64`
- `len: u64`

meaning:

- `ptr` points to immutable UTF-8 bytes
- `len` is the byte length

This payload representation is the same inside and outside compiled strict
fragments.

The runtime may carry additional tracking data around that payload, but not
replace it with a different fragment-visible shape.

### Runtime envelope

Outside compiled strict fragments, a runtime string value may additionally
carry envelope metadata such as:

- static/literal provenance
- borrowed provenance
- transient-region provenance
- retained/reference-counted provenance

That metadata is outside the canonical payload.

### Parameter rule

When a `String` crosses into a strict fragment as a parameter:

- the caller passes the canonical payload `[ptr, len]`
- the bytes are borrowed for the duration of the strict fragment call
- the fragment must treat them as immutable
- the fragment must not retain raw pointers past the call boundary

### Return rule

When a strict fragment returns a `String`, the returned payload is guaranteed
to remain valid until the end of the current evaluation frame.

This is the core ownership rule.

That means a returned string may be backed by:

- static bytes
- bytes borrowed from an input that remains live for the frame
- bytes allocated in the current transient evaluation region

The caller may immediately:

- treat the result as a valid runtime `String`
- copy/promote it if longer retention is needed

The caller does not need to distinguish those backing cases just to consume the
result within the current frame.

### Consequence

This removes the need for the strict fragment ABI itself to encode a separate
ownership handle in the `String` payload.

The ABI remains:

- `[ptr, len]`

and ownership/lifetime is supplied by the surrounding evaluation frame.

#### Parameter semantics

For string parameters:

- `ptr` points to UTF-8 bytes
- `len` is the byte length
- the bytes are borrowed for the duration of the strict fragment call

#### Return semantics

For string returns:

- the compiled fragment returns `[ptr, len]`
- the returned payload is valid until the end of the current evaluation frame

That means:

- the caller may use the result immediately as a runtime string
- the caller may copy/promote it if it must outlive the frame
- the caller does not need a separate ownership token at the ABI layer

The remaining unresolved part is the exact implementation strategy for:

- transient evaluation regions
- promotion out of the frame
- retained/reference-counted ownership when longer lifetime is needed

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

But “separate” here should not be read as “unrelated”.

The intended direction is:

- runtime representation may include a richer envelope
- fragment-visible payload layout should still match the strict fragment ABI

The strict fragment ABI only defines:

- how selected values cross into compiled strict fragments
- how results cross back out

These are different concerns.

## Current Unresolved Questions

### 1. Returned string ownership

The frame-valid rule above is the intended semantic rule.

What remains unresolved is the implementation strategy underneath that rule:

- exact transient region mechanism
- promotion/retention mechanism
- how runtime envelope metadata records provenance when needed

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
