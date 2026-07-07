+++
title = "vixen internals"
+++

The developer half of the vixen documentation: how the machine works, what the
pieces are, the language builds are written in, and how to contribute.

If you're evaluating vixen as a *user* — installing it, building a workspace,
comparing it to cargo or Bazel — you want [the user site](/) instead.

- **[Concepts](/dev/concepts/)** — demand, content addressing, read-sets,
  molten values, hermetic execution, services.
- **[The constellation](/dev/constellation/)** — the map of the pieces.
- **[The vix language](/vix/)** — guide and reference. It's its own
  top-level section now; this developer site keeps the machine internals
  underneath it (lowering, lowering-cache, hashing, the store).
- **[Developing vixen](/dev/developing/)** — invariants, oracles, method, holes.
