+++
title = "weavy — the substrate"
weight = 2
+++

weavy is the execution substrate everything compiles to: a shared vocabulary for
*what values look like in memory* and *how programs run over them*. vix lowers
to weavy; [snark](@/constellation/snark.md)'s parsers lower to weavy;
[phon](@/constellation/phon.md)'s codecs lower to weavy. One substrate, many
front ends.

It has three jobs:

**Memory layout.** weavy's descriptors say exactly where a value's bytes are:
field offsets, enum tags (including niche encodings), array strides, which byte
ranges are padding. Layouts come from two authorities — Rust types (whose layout
the Rust compiler dictates and [facet](@/constellation/facet.md) reveals) and
weavy-declared types (whose layout *we* choose, enabling tricks Rust won't do
for you: guaranteed return-value placement, aggressive niches). Consumers never
care which authority produced a descriptor.

**Execution.** A small task VM — frames, calls, jumps, host calls, awaits — with
two lanes: a portable interpreter and a copy-and-patch JIT. Both lanes produce
identical observable traces; the test suite enforces that equivalence
permanently. Tail calls are real jumps. Suspension is first-class, which is how
demand ("this value isn't computed yet — park and resume") runs without threads.

**Observability.** This is the part that earns the "we control the ABI"
decision: weavy *owes* the metadata a toolchain would normally provide. JIT'd
code registers real DWARF — line tables *and* type information (structs,
enums-with-niches, arrays) generated from the same descriptors that drive
execution. You can attach a debugger to build logic mid-run and inspect a
molten value structurally. One descriptor, two outputs: runtime access and
debugger visibility.

## Why not an existing VM?

Because the requirements compose badly with off-the-shelf runtimes: content-
addressed values need byte-exact layout control and padding proofs; demand needs
cheap suspension; the reuse system needs mutation gated on uniqueness checks in
the hot path; and the observability bar ("attach lldb to the build") requires
owning codegen. weavy is small precisely because it does only these things — the
languages above it stay simple because the substrate is exactly shaped for them.
