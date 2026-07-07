+++
title = "gingembre — templating"
+++

gingembre (French for ginger) is the templating engine: text with holes,
filled from typed data — the layer that turns values into HTML, config files,
generated source, anything shaped like "document with computed parts."

Every ecosystem has a templating engine; gingembre exists because this
constellation holds templates to the same standards as code:

- **Templates compile.** A template lowers to
  [weavy](@/constellation/weavy.md) like any other program — stencil-compiled,
  fast, and *debuggable*: the JIT's debug info maps machine code back to the
  template source, down to which sub-expression of a line you're in. A
  template that misbehaves can be stepped through in a debugger, which is a
  sentence rarely written about templating engines.
- **Provenance survives.** Output knows which template expression produced it.
  For build systems this is not a luxury — "which input wrote this line of the
  generated file" is the difference between debugging and archaeology.
- **Data is typed.** Template inputs are schema'd values, not stringly maps —
  the same type vocabulary as everything else, so a template's contract is
  checkable like a function's.

In the constellation, gingembre renders where dodeca serves — and generated-
file steps inside builds (config generation, code generation from data) get a
templating primitive whose reads and inputs the machine can track like any
other computation's.
