# 012: Lower Eager Kernel Islands To Weavy

Status: Needs rewrite for the Weavy direction. The old `vx-analysis` / `vx-jit`
plan below is obsolete; do not revive it. The surviving work is to rebuild
interpretation and JIT from the current graph-runtime model on the shared Weavy
lowered-program substrate in `/Users/amos/oss/facet/weavy`.

## Goal

Lower eager, pure kernel islands from the graph runtime into a Weavy program that
can be executed by both the Weavy interpreter runner and the copy-and-patch JIT.

This keeps one semantics source for interpreted and native execution. The old
span-keyed AST walk and the old `KernelOp`/`ExecutableBody` strict-lowering path
were attempts to reach that shape before the runtime and language boundary had
settled.

## Motivation

The direction still matters because Vixen has mixed programs: orchestration,
cache boundaries, and `exec` stay graph-shaped, while local pure interiors should
be executable as eager kernels. The delicate part is the boundary:

- kernel discovery belongs to the current graph/runtime model, not a restored
  bridge analysis crate
- interpretation and native codegen must share the same lowered representation
- recursion, `if`, and `match` are kernel-internal only when they do not force a
  thunk or cross an intrinsic/capability boundary
- the interpreter is the oracle for the JIT, not a parallel evaluator

## Rewrite Target

- Define the graph/runtime subset that may lower into a kernel island.
- Lower that subset into Weavy's `Lowered` / dense-block program shape.
- Express calls and recursive structure with Weavy blocks and block calls instead
  of Rust recursion or a Vixen-specific callable API.
- Run the lowered program with the Weavy runner first, then compile the same
  representation through the Weavy copy-and-patch surface once the ABI is ready.
- Add oracle-style tests that compare graph execution, Weavy interpretation, and
  native execution for the same lowered kernel when native execution exists.

## Acceptance

- A simple scalar kernel lowers from the graph-runtime representation to Weavy
  and executes through the interpreter path.
- A mixed program keeps graph boundaries graph-shaped while lowering only its
  eager pure interior.
- The lowered representation used for interpretation is the same representation
  handed to the JIT path.
