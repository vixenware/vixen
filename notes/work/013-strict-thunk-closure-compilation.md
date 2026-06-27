# 013: Partial Kernel Compilation In Mixed Graph Programs

Status: Needs rewrite for the Weavy direction. The old `vx-jit`,
`ExecutableBody`, and `KernelKind::Strict` path below is obsolete; the surviving
work is partial kernel lowering from the current graph runtime into Weavy.

## Goal

Allow mixed Vixen programs to keep orchestration and capability boundaries in the
demand graph while lowering their eager pure interiors into Weavy programs that
can run interpreted first and natively later.

## Motivation

The old framing was “compile some parts through `vx-jit` and evaluate the rest”.
That split is still the right product shape, but the units are wrong now. The
runtime no longer has an eager `ExecutableBody` evaluator to decorate with strict
kinds; it has a graph runtime. Partial compilation must therefore be expressed
as graph partitioning plus Weavy lowering.

This matters because most useful Vixen programs are mixed:

- `exec`, fetch, package probing, sandboxing, and cache boundaries stay graph
  nodes
- resolver loops, accumulator merges, constraint checks, cfg evaluation, and
  scalar/string-heavy glue can be eager kernels
- the interpreter and the JIT need one lowered representation so native
  execution has an oracle

## Work

- Prerequisite: 012 rewritten as graph-runtime-to-Weavy kernel lowering.
- Define the mixed-program partition rule: graph boundaries remain graph-shaped;
  eligible eager interiors lower to Weavy.
- Preserve source/span/provenance metadata across the partition so diagnostics and
  graph visualization can explain why a node stayed graph-shaped or entered a
  kernel.
- Execute the lowered kernel through the Weavy interpreter runner as the first
  oracle.
- When native support is present, compile the same lowered program through
  Weavy's copy-and-patch JIT and compare behavior with the interpreter path.
- Add a regression fixture where a top-level graph crosses a runtime boundary,
  but a demanded pure interior is lowered and executed as a kernel.

## Acceptance

- The mixed program still runs on the graph-runtime path.
- At least one pure interior is visible as a lowered Weavy kernel artifact, not a
  restored `KernelKind::Strict` flag.
- Interpreter execution and native execution, when available, agree for the same
  lowered kernel.

## Notes

This is not a reason to restore `vx-jit` or an eager evaluator. It is the partial
execution strategy for the current graph runtime plus Weavy substrate.
