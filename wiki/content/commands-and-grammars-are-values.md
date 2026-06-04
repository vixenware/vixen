+++
title = "Commands & grammars are values; the grammar-forcing resolver"

[extra]
type = "Task"
status = "todo"
priority = "high"
tags = ["exec", "grammar", "capability", "types", "type-checking", "lsp"]
created = "2026-06-03"
+++

Distilled from a long working session (and ~seven wrong turns) so the next pass
starts from the model instead of re-deriving it. Companion to
[[Types are values]] and [[Executor abilities]].

## The one rule: representation follows structure, never implementation language

Whether a value's fields and methods are computed by Rust or written in Vixen is
an **implementation detail**. It is *never* a reason to give something a special
representation — not a dedicated `Value` variant at runtime, not a special `Ty`
kind, not an "opaque" anything. A type is classified by its structure: fields →
struct, variants → enum.

Corollaries (all the same rule):

- **`Host` is a struct** — `struct Host { os, arch }` with methods (`which`, …).
  `os`/`arch` are ordinary fields the runtime fills; the methods have Rust
  bodies. It is a `Value::Struct`, **not** `Value::Host`, **not** `type Host;`.
  (Done — commit `b474bdcd`.)
- **`Command` is a struct** — `{ grammar, binary, fingerprint, exemptions }`. A
  capability constructs Commands ("advertising a capability is defining its
  commands"). It should be a `Value::Struct`, not `Value::Command`. The runtime's
  `command_bundle` already accepts the struct/record form.
- **`Grammar` is a struct/enum** — its flags, modes, positionals, and value
  domains are ordinary nested data. Not `Value::Grammar` / `Value::Grammars`.
- **Nothing here is opaque.** Reaching for an opaque holder the instant something
  is Rust-backed was the single most-repeated mistake. Kill the instinct.

## Grammars are rich validators, not types

A grammar is **not** a static type, and it is **richer than any type could
describe**. It carries:

- **value domains** — `-O`'s value is one of `{0,1,2,3,s,z,g,fast}`, not "a
  String" or even a fixed enum;
- **mode-dependent legality** — a flag legal while compiling, illegal while
  linking;
- **lints, cross-flag policies**, and crucially **authored diagnostics** — `-O9`
  yields *"9 isn't an optimization level; valid: 0/1/2/3/s/z/g"*, a message a
  type system cannot author.

A capability **computes** the grammar by probing — it runs the tool and reads its
version (a tool's CLI changes between versions). A static type cannot hold a
probed, richer-than-types validator; a **value** can. That is the real force of
"types are values": the thing that classifies (types) the arguments *exceeds*
what types express, so it must be a first-class value.

## The feature: the grammar-forcing CLI resolver

`@[ ${tc.cc} -c main.c -o out/main ]` is validated by **forcing `tc.cc`'s
grammar value** and running its full validation against the literal — value
domains, modes, lints, diagnostics. "Typing the holes" (a `-o` slot demands a
`Path`) is the thinnest slice; the rest is the grammar's own validator.

- **Best-effort / pending.** Forceable in the snapshot → a compile-time check
  with full guarantees; not forceable (offline, no runners) → a *pending* check,
  never an error. Strongest when connected, never *wrong*, only sometimes
  *incomplete*.
- **The LSP is a client of the live engine**, not a re-implementation of the
  checker. Hover-to-evaluate and grammar-completion are one gesture: force a
  value in the live graph. In-process today through `vx-runtime`.
- Concretely: `infer_cli_literal` projects/forces the leading hole's grammar
  value, then runs the existing structural + per-hole validation. Today only the
  static `@grammar[name]` form validates; the `${cmd}` forcing path is the
  **unbuilt "later pass."** Building it is the feature.

## Build plan (capability core → content-identified hello-world)

The driver is **compile a real hello-world C program**, then the caching
mechanism. Capabilities are required because a compiler is *content-identified*,
not a path — its fingerprint is half the cache key (`cc` at `/usr/bin/cc` keyed
on the path would silently serve stale `.o` after a compiler upgrade).

1. ✅ **Host** — `struct Host { os, arch }` + `which` (commits `6b092f4a`,
   `b474bdcd`).
2. **Commands/grammars as value-structs** — collapse `Value::Command` /
   `Grammar` / `Grammars` to `Value::Struct`; `command_bundle` already takes the
   record form, so it is mostly deletion.
3. **The cc probe → `CToolchain`** — `fn probe(host: Host) -> Option[CToolchain]`
   that `host.which("cc")`s, computes the grammar (run cc, read its version), and
   returns `CToolchain { cc: Command, … }` carrying the obligations the probe
   recorded.
4. **The grammar-forcing resolver** — `infer_cli_literal` forces the leading
   hole's grammar value; best-effort / pending. This runs in the **engine**, not
   `vx-ir`: checking forces values, so the checker lives with the evaluator — see
   [[Checking is forcing]] for the (settled) structural decision and migration.
5. **Content-identified exec + input mounting** — `ExecCommand` carries the
   fingerprint; the executor materializes the read-set (the deferred `run_exec`
   piece in `observe.rs`); a real `cc` compiles a mounted `hello.c`.
6. **Deferred** — verification recheck (obligations rechecked at exec,
   speculative, drift poisons); the cache (gated behind
   [[Design cache key + sandbox together before shipping cache code]]); the fleet
   (daemon, Vox registry, frontmatter advertisement, `where`-routing — a local
   in-process probe stands in for now).

## Dead-ends — do not re-walk

- *"Rust-backed ⇒ opaque / special `Value` variant / special `Ty` kind."* No.
  Structure decides representation.
- *"A command's **type** statically carries its grammar."* No — the grammar is a
  runtime value, forced at check time.
- *"Specify the command and the grammar separately"* (the `@grammar[…]` named
  form as the general case). No — the command bundles its grammar; `@grammar[…]`
  is the static special case.
- *"`which` returns `Option`."* No — it is fallible (poisons on absent), and `?`
  catches it into a `Result`. Host paths are `String` (the real filesystem), not
  VFS `Path`.
