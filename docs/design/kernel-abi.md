# Kernel ABI

The boundary between the demand-driven runtime value world and compiled
**kernels** — eager, pure islands of Vixen computation compiled to native code.
(Earlier drafts called these "kernels"; the word was confusing and is
dropped. "Kernel" is the term.)

**Status: we are rebuilding this path on Weavy.** The earlier `vx-jit` was
deleted because it was built on an unsettled language. The language is now
settled (capabilities-as-objects, no effects, the constraint / accumulator /
resolution primitives), which makes the kernel story tractable — see
[Partition](#partition). The intended replacement lowers eager kernels to the
shared Weavy program/block representation from `~/oss/facet`, so the interpreter
oracle and copy-and-patch JIT consume one artifact. This doc is the design; it is
not yet normative spec text.

## Terms

- **kernel**: an eager, pure, local fragment of Vixen computation that the
  evaluator runs to completion once entered — an eager island in the lazy
  demand sea. It touches no capability (`Host` / `Process` / `Tree` / `Accum`),
  calls no intrinsic, and forces no thunk. It can be interpreted or compiled
  independently of the larger orchestrated evaluation.
- **runtime value**: the orchestrator's value representation — the dynamic,
  value-semantics-first world that carries Vixen values across evaluation and
  RPC boundaries.
- **ABI word**: one machine-sized argument/return slot in the kernel calling
  convention. Today this is `u64`.

## Why This Exists

The demand-driven evaluator does not run everything through the same direct
calling convention a compiled kernel uses. That mismatch is real and expected:

- Vixen values have value semantics.
- The runtime value world is dynamic.
- Strings, arrays, maps, trees, closures, and enums/structs are not native Rust
  ABI values in any stable external sense.

So the kernel ABI is not trying to match Rust crate APIs, the C ABI for
arbitrary authored types, or the full runtime representation. It provides:

1. a small, explicit calling convention for compiled kernels
2. a formal lowering/lifting boundary between runtime values and compiled code
3. a layout model that grows to structs and enums without more ad hoc cases

A kernel is also the unit shipped to a remote executor (an `exec` observer, a
`pubgrub_solve` domain closure): it travels as canonical IR and is compiled — or
interpreted — wherever it lands. So the kernel ABI doubles as the exec / RPC
value boundary; one mechanism, not two.

## Partition

Finding kernels is cheap because the runtime already draws the line. The cache
policy splits intrinsics (`Persist` — `exec` / `acquire` / `Http` / `parse` /
`pubgrub_solve`, the native boundary nodes) from pure user computation
(`InMemory` — interpreted, memoised per evaluation). **That split is the kernel
boundary**: a kernel is a maximal pure `InMemory` region, and the capability
model guarantees its purity by construction — a fragment touching no capability,
calling no intrinsic, and forcing no thunk is pure, and all three are visible in
the types. So the partition is a type-level question, not a heuristic dataflow
pass (this is what the deleted `vx-analysis` was straining to compute).

The evaluator owns the boundaries: it forces a kernel's inputs, schedules the
kernel once they are ready and its result is demanded, runs it eagerly to
completion, and lifts the result back. Kernels never force, never call
intrinsics, never see a capability. Control flow (`if` / `match` / recursion) is
kernel-internal as long as no branch crosses a boundary; a forcing or
intrinsic-calling branch ends the kernel there. (An accumulator fixpoint's
`merge` is a kernel; the fixpoint driver and any fetch between iterations are
orchestration.)

The hot kernels in practice: `pubgrub_solve`'s `dependencies` / `prefer` /
`available_versions` closures (called many times per solve), accumulator merges
(feature union, MVS max) over large graphs, constraint evaluation, cfg-expr
evaluation, `SemVer` comparison, string munging.

## Codegen: copy-and-patch

Kernels compile with **copy-and-patch**, not a full backend like Cranelift: a
precompiled stencil per kernel op (machine-code templates with holes for
operands and branch targets), copied and patched at runtime. No optimizing
backend, no heavy register allocation — millisecond compiles, good-enough code.
The kernel ABI (word-flattened, shared payload) is the stencil calling
convention.

This fits the workload (many small pure fragments, cheap-fast codegen wanted),
aligns with the copy-and-patch direction vox RPC is taking, and keeps kernels
shippable as IR — each machine copy-and-patches locally, so native code is never
shipped across heterogeneous executors. Tiering: interpret by default, compile a
kernel only once it is hot; copy-and-patch makes the threshold low.

## Representation Goal

The goal is **not** to invent one representation for the runtime and a
different representation for compiled kernels.

The goal is:

- for every type admitted to the kernel ABI, define one canonical
  **payload representation**
- have both the runtime and compiled kernels use that same payload
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

Compiled kernels should not need to know about that envelope if they
can operate directly on the payload representation.

This means the preferred direction is:

- minimize semantic reshaping at the kernel boundary
- keep ABI lowering/lifting mostly structural
- avoid separate “runtime layout” versus “JIT layout” for the same kernel-visible type

In short:

> kernel-visible types should have a shared canonical payload
> representation across the runtime and JIT; runtime-only tracking may exist
> around that payload, but not redefine it.

## Cost Model

Yes, there is marshaling cost at this boundary if the kernel ABI does
not match the runtime representation exactly.

That cost is unavoidable in some form, because the runtime world is dynamic and
value-semantics-first. We are not trying to make runtime values be Rust-native
ABI values.

The right optimization target is therefore not:

- "zero cost against arbitrary outside code"

It is:

- minimize crossings
- make each crossing explicit and mechanically simple
- compile larger kernels so the boundary is crossed rarely
- keep the ABI layout rules stable and predictable

Given the representation goal above, the preferred end state is actually
stronger than “accept marshaling cost”:

- use the same payload representation inside and outside compiled kernels
- pay boundary cost only for:
  - dynamic envelope handling
  - ownership transitions
  - forcing/copying where semantics require it

In practice:

- paying one marshaling step at kernel entry/exit is acceptable
- paying that cost for every internal operation would not be

So the design pressure is to make kernels large enough that the
boundary is coarse, not microscopic.

## Current Implemented ABI Domain

Today the implemented kernel ABI only covers:

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

`String` is the first kernel-visible type that needs an explicit ownership
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
replace it with a different kernel-visible shape.

### Runtime envelope

Outside compiled kernels, a runtime string value may additionally
carry envelope metadata such as:

- static/literal provenance
- borrowed provenance
- transient-region provenance
- retained/reference-counted provenance

That metadata is outside the canonical payload.

### Parameter rule

When a `String` crosses into a kernel as a parameter:

- the caller passes the canonical payload `[ptr, len]`
- the bytes are borrowed for the duration of the kernel call
- the fragment must treat them as immutable
- the fragment must not retain raw pointers past the call boundary

### Return rule

When a kernel returns a `String`, the returned payload is guaranteed
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

This removes the need for the kernel ABI itself to encode a separate
ownership handle in the `String` payload.

The ABI remains:

- `[ptr, len]`

and ownership/lifetime is supplied by the surrounding evaluation frame.

#### Parameter semantics

For string parameters:

- `ptr` points to UTF-8 bytes
- `len` is the byte length
- the bytes are borrowed for the duration of the kernel call

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

At the kernel boundary, the runtime does two operations:

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

- semantic type is preserved by the kernel type information
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

- kernel type -> ABI layout descriptor

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

The kernel ABI should not be confused with the orchestrator/runtime
representation.

The runtime may use:

- persistent structures
- reference-counted strings
- dynamic tagged values
- content-addressed tree/blob handles

But “separate” here should not be read as “unrelated”.

The intended direction is:

- runtime representation may include a richer envelope
- kernel-visible payload layout should still match the kernel ABI

The kernel ABI only defines:

- how selected values cross into compiled kernels
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
- future kernel-visible handles

### 3. Runtime value bridge

We need a first-class runtime value <-> kernel lowering layer instead
of letting the orchestrator own ad hoc conversions.

## Near-Term Plan

Rebuild order, now that the language is settled:

1. **Partition pass** — mark maximal pure `InMemory` kernels in the IR (no
   capability, no intrinsic, no force), reusing the cache-policy boundary.
2. **Kernel interpreter** — run kernels directly first; correctness before speed.
3. **Layout descriptors** — formalize struct (field concat), enum (tag + payload),
   and tree / blob handle layouts; drive lowering / lifting by descriptor, not by
   hand-written per-signature matching.
4. **Value↔ABI bridge** — a dedicated lowering / lifting layer the evaluator
   calls, including returned-string ownership (transient region vs. promotion).
5. **Copy-and-patch backend** — stencil per kernel op; compile hot kernels,
   interpret the rest (tiering).
6. **Unify with closure shipping** — the same kernel IR + ABI is what ships to
   executors for `exec` observers and `pubgrub` domain closures.

Keep the existing `U64` / `Bool` / `String` ABI; everything above generalizes it.
