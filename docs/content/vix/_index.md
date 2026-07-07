+++
title = "The vix language"
+++

vix is the language vixen's builds are written in — including vixen's own
understanding of what a Cargo build *is*. It exists because build logic has a
shape that general-purpose languages fight: everything should be memoized,
everything should be incremental, everything should track what it read, and
nobody should have to write any of that by hand.

So vix makes those properties ambient. You write ordinary-looking functional
code; the machine makes it demand-driven, content-addressed, and observable. The
sections below are both guide and reference:

- **[Values and schemas](@/vix/values.md)** — everything is bytes with a schema;
  there is no god-object. Structs, enums, arrays, maps, options, versions.
- **[Functions and demand](@/vix/functions.md)** — calling a function is
  *demanding* a value: memoization, laziness, and what that means for how you
  structure code.
- **[Loops](@/vix/loops.md)** — iteration is tail recursion, and tail recursion
  is a real loop: the machine compiles it to a jump, and your accumulator stays
  hot.
- **[Molten values in practice](@/vix/molten.md)** — write `let xs = xs.push(x)`
  and it mutates in place when that's invisible; the rules, the diagnostics, and
  why you rarely need to think about it.
- **[The standard library](@/vix/std.md)** — what ships: versions (full semver),
  strings, collections.
- **[Builtins](@/vix/builtins.md)** — `toml()`, `json()`, `fetch()`, `exec()` and
  friends: what's a permanent primitive vs. what's scheduled to become ordinary
  library code.
- **[Diagnostics](@/vix/diagnostics.md)** — what the compiler tells you, including
  the performance diagnostics ("this loop fell back to copying, here's why").
