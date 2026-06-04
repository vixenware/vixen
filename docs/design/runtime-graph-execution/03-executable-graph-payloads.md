# Runtime graph execution - executable graph payloads

This document is self-contained. It describes how to make `DemandGraph` an
executable artifact instead of an inspectable snapshot that still relies on
source expressions.

## Complete task context

The Vixen runtime must execute graph nodes. The analyzer may inspect VIR, but the
graph executor cannot recover `ExprId` or match `ExprKind`. Therefore graph nodes
must carry all structured information required for execution. Display labels are
for humans and playgrounds only. If runtime semantics depend on `label`, the graph
is not executable enough.

The current graph shape contains `NodeKind`, `label`, type string, and input node
ids. This was enough for visualization and some scalar lowering, but not enough
for full runtime execution. This phase replaces label/source dependence with
structured payloads.

## Payload design principles

- Payloads are typed data, not display strings.
- Names used for display should not define runtime behavior.
- Layout is positional where possible. Struct field names live in type/layout
  metadata and analyzer payloads, not in ad-hoc runtime maps.
- A graph node must be executable without VIR.
- If a graph node cannot yet be represented, analyzer should emit an explicit
  unsupported/unresolved executable node.
- Boundary nodes must declare their materialization profile.
- Kernel nodes must identify the kernel region or carry a structured kernel IR
  handle.

## Required graph payload families

### Constants

String labels must not carry constant semantics. The graph representation now
uses `ConstValue` for constants:

```text
ConstValue::U64(u64)
ConstValue::Bool(bool)
ConstValue::Str(String)
ConstValue::Unit
ConstValue::Float(String)
ConstValue::Unsupported(String)
```

`vx-runtime-core` and executable lowering consume those payloads directly. They
do not parse labels like `"42"` or `"true"`. The analyzer is still allowed to
parse VIR literals while translating source into graph nodes.

Current limits:

- Float literals are structured but not executable in core yet.
- Unsupported literal spellings stay explicit as graph payloads instead of
  silently becoming display-label semantics.
- Strings are represented as graph constants, but the broader string/method
  runtime remains part of the pure-execution phase.

### Lazy data

Lazy data node payload:

```text
LazyDataPayload {
  layout: ProductLayoutId or inline layout,
  fields: Vec<NodeId>
}
```

Kinds:

- tuple
- array
- struct
- record/map later
- enum payload struct

Demand rule: constructing lazy data returns a node output containing field node
references; it does not demand fields.

### Projection

Projection payload:

```text
ProjectPayload {
  container: NodeId,
  slot: u32,
  projection_kind: Index | Field | MethodReceiverLayout, as needed
}
```

Demand rule: demand enough of the container to select layout, then demand only
the selected slot. Known-constructor projection can directly need the selected
field.

### Variants

Variant construction payload:

```text
VariantPayload {
  enum_layout: EnumLayoutId,
  tag: VariantTag,
  fields: Vec<NodeId>
}
```

Unit variants are values, not params. This is important because an earlier graph
bug treated bare unit variant names as `Param`, which confused lowering.

Pattern matching should inspect the variant tag and demand payload fields only
when the selected arm reads them.

Current progress:

- Graph has `NodeKind::Variant { tag, field_names }` with payload fields stored
  as input node refs.
- Core constructs `NodeOutput::Variant` without demanding payload fields.
- Projection works over variant payload fields, so payload reads demand only the
  selected field node.
- `MatchPattern::Variant { tag }` selects by variant tag in graph core.
- Analyzer binds variant match payload fields through projection nodes while
  building the selected arm body graph.

### Functions and calls

Function entry payload:

```text
FunctionPayload {
  code_identity: CodeIdentity,
  graph_template: GraphTemplateId or body graph builder,
  parameter_layout: Vec<PatternBinding>,
}
```

Call node payload:

```text
CallPayload {
  callee: NodeId or FunctionId,
  args: Vec<NodeId>
}
```

Demand rule: demand callee enough to know which function/closure to enter, then
enter the function graph with argument nodes bound as params. Do not force
arguments merely because a call exists.

Current progress:

- Statically resolved direct calls to function items are expanded at analysis
  time. The expanded graph binds parameter patterns to argument node refs.
- Closure calls and named functions used as values also expand when the analyzer
  can resolve the callable template.
- This is intentionally not the final payload/template system. Recursion and
  returned callable values still fail explicitly as unsupported graph shapes.

### Closures

Closure payload:

```text
ClosurePayload {
  function: FunctionId or graph template,
  captures: Vec<CaptureSlot { binding/layout, node }>
}
```

Closure construction should not evaluate captures. Captures are node references.

Current progress:

- Graph has a source-blind `Closure` node kind whose inputs are capture node refs.
- `DemandGraph::demand_reachable` does not demand closure captures merely because
  the closure value exists.
- Analyzer-local callable templates expand known closure/function-value calls
  into graph nodes. The executable graph still does not store VIR ids.

### Match and control flow

Control flow belongs in eager kernels when scalar/lowerable. For graph-level
dynamic matching, payload must be structured:

```text
MatchPayload {
  scrutinee: NodeId,
  arms: Vec<ArmPayload { pattern, body }>
}
```

Demand rule: demand scrutinee enough to select an arm. Demand only selected arm
body. Pattern binding creates param/binding node refs into the selected body.

Current progress:

- Graph has `NodeKind::Match { arms }` with source-blind `MatchPattern`
  payloads.
- Core supports literal constants, wildcard, and binding patterns.
- Core supports variant-tag patterns.
- Analyzer binds simple binding patterns to the scrutinee node while building the
  selected arm body graph.
- Analyzer binds variant payload patterns through projection nodes. Broader
  tuple/struct/array destructuring patterns remain explicit unsupported graph
  shapes in graph-level match.

### Fail and bomb

Bomb payload is trivial:

```text
Bomb
```

Demanding it returns poison.

Fail payload:

```text
FailPayload {
  message_or_error: Option<NodeId>
}
```

Demanding `fail` may demand the message/error node only to construct the poison.

Fallible catch payload:

```text
FalliblePayload {
  inner: NodeId,
  caught_failure: NodeId,
}
```

Current progress:

- Graph has `NodeKind::Fallible { caught_failure }` and `NodeKind::CaughtFailure`.
- Core poison carries typed graph error output plus failure context, not only a
  display string.
- Driver handles a catch-mode step by demanding the inner node and memoizing the
  paired caught-failure node if that demand poisons.
- Core reifies success as `Ok(inner)` and caught poison as
  `Err(Failure { err, context })` through graph outputs.
- `fail f` over a caught `Failure` re-raises the same typed `err` and context.

### Methods and intrinsics

Method payload:

```text
MethodPayload {
  receiver: NodeId,
  method: MethodId,
  args: Vec<NodeId>
}
```

Pure methods can lower into kernels or pure nodes. Effectful methods become
boundary nodes. Host methods such as `which`/`exec` are boundary-ish and should
not be hidden inside source eval.

Current progress:

- `receiver.len()` with no arguments is recognized as graph `KernelOp::StrLen`.
- Graph core demands only the receiver and returns its byte length when the
  receiver resolves to `NodeOutput::Str`.
- `host.which(name)` and `host.exec(args)` are recognized as
  `NodeKind::HostMethod { method: HostMethod::Which | HostMethod::Exec }`.
  Core demands their receiver/direct argument inputs and then reports a boundary.
  Runtime graph execution materializes those typed host boundaries in the driver
  path instead of invoking source-level method dispatch.
- `host.exec` returns a graph `NodeOutput::Struct` with field nodes for
  `exit_code`, `stdout`, `stderr`, and `fingerprint`. Projection demands only the
  selected field.

### Imports

Import payload:

```text
ImportPayload {
  package: PackageCoordinate,
  kind: GrammarPackage | Module | ...
}
```

Grammar-package imports currently lower to graph lazy records: the package value
has one field per grammar, and each field is a graph value with `package` and
`name` fields. General module/package imports remain dynamic enough that some
imports may stay boundary/unresolved until package resolution semantics are fully
graphified.

### Boundary nodes

Boundary payload:

```text
BoundaryPayload {
  kind: Exec | Http | Acquire | HostWhich | HostExec | ...
  upfront_inputs: Vec<NodeId>,
  progressive_inputs: Vec<NodeId>,
  produced_outputs: OutputLayout,
}
```

Demand rule: boundary execution is driven by legal boundary materialization
demand, not arbitrary internal force.

Current progress:

- External body arguments are seeded as graph outputs. Struct/tuple/array/variant
  fields become boundary nodes backed by the supplied native/resolved public
  thunks.
- The driver loop can materialize external field boundaries and typed host method
  boundaries, then resume the original demand.
- Graph-visible executor-local output trees use `NodeOutput::OutputTree` and
  cross to `WireValue::Tree` at observer wire crossing.
- Backtick command literals now emit graph `Boundary` nodes with runtime-local
  metadata for CLI templates and observer bundles. The execution crates still see
  only graph nodes and boundary requests; `vx-runtime` owns the VIR-derived
  observer bundle sidecar.
- Demand on a backtick boundary materializes only request-shaping input nodes
  before dispatching observe. A cold exec in an undemanded lazy-data field stays
  undemanded.
- `tree.file(path)` now emits a graph boundary with runtime-local metadata.
  Remote tree handles are stored in the runtime boundary context keyed by graph
  node; demanding `.file(path)` pulls only that path through `force_remote`.

### Kernel nodes

Kernel payload:

```text
KernelPayload {
  kernel_id: KernelId,
  strict_inputs: Vec<NodeId>,
  output_layout: AbiLayout,
}
```

The core/driver may call the kernel interpreter when strict inputs are ready.
The kernel contains eager pure control flow. It does not contain lazy data
construction or boundaries.

Current implemented executable kernel payloads:

- `KernelOp::Assert`: demands one boolean input, returns unit on `true`, poisons
  on `false`.
- `KernelOp::AssertEq`: demands two inputs, compares their resolved graph outputs,
  returns unit on equality, poisons on inequality.

These are explicit graph payloads emitted by the analyzer for prelude
`assert(...)` and `assert_eq(...)` calls. Core executes them without consulting
VIR or source expressions.

## Analyzer responsibilities

The analyzer must translate VIR into executable graph payloads. If it cannot
translate a construct, it should produce a typed unsupported/unresolved node. It
must not rely on executor code looking back at VIR.

Analyzer may keep expression maps for diagnostics and source-to-graph display.
Those maps must not cross into core/driver execution.

## Migration order

1. Add structured constant payloads while keeping labels for display.
2. Add structured lazy data/projection payloads.
3. Add variant payloads and unit variant construction.
4. Add function/call/closure payloads.
5. Add match/control payloads not represented as kernels.
6. Add method/import payloads.
7. Add boundary payloads.
8. Add kernel-region payloads that identify executable kernels without `ExprId`.
9. Remove semantic parsing of labels.

## Acceptance criteria

- Core can execute basic graph nodes without parsing labels.
- Driver can schedule nodes without source maps.
- Analyzer emits explicit unsupported graph payloads for unknown constructs.
- No execution crate stores `ExprId` or `BodyId`.
- Playground/display labels remain available but cannot affect runtime results.
