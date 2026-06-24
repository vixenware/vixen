+++
title = "Open language kernel strategy"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-06-24"
+++

This records the current strategy for the Vixen language kernel: put the reusable
language substrate where it can share machinery with Facet, Fable, Weavy, Styx,
and the rest of the open tooling, while keeping the production build host and
runtime boundary inside Vixen.

The goal is not to make a toy public language and a separate private language.
The goal is one language kernel with one parser, one syntax tree, one diagnostic
model, one formatter/LSP path, one lowering story, and one eventual Weavy/JIT
execution surface. Products provide hosts.

## Thesis

Vixen needs the same infrastructure that Fable and the Facet ecosystem are
already growing:

- incremental parsing and reparsing
- syntax trees, diagnostics, highlighting, formatting, and LSP plumbing
- type-aware analysis over Facet-shaped values
- lowering into Fable/Weavy-shaped IR
- interpreter/JIT observability, testing, fuzzing, and profiling
- a REPL/debugging surface that can inspect live typed state

Keeping that kernel open and close to Facet shortens the loop from "the language
found a missing primitive or bad invariant" to "the shared substrate is fixed".
It also keeps Fable, facet-json, facet-hash, Styx, and Vixen from each inventing
parallel versions of the same compiler-tooling stack.

## Open Kernel

The open kernel belongs in the Facet monorepo once the next slice is ready. That
is where the common substrate lives and where cross-crate pressure is useful.

Open pieces:

- grammar, lexer/parser, concrete syntax tree, AST, and error recovery
- diagnostics, spans, source maps, formatter, highlighter, and LSP primitives
- semantic model for the language core: binding, modules, type checking, effects
  that are part of the language, and canonical IR
- Facet type integration: describing, inspecting, constructing, transforming, and
  validating Facet-shaped values
- host-function trait surface and capability model, without Vixen's production
  host implementations
- lowering to Fable and/or directly to Weavy where that is the right substrate
- a small reference host that is useful for tests, examples, fuzzing, and docs
- conformance tests, fuzzers, golden diagnostics, and cross-runner oracle tests

This is enough to make the language real, toolable, testable, and reusable
without putting the product host in the open repository.

## Proprietary Host

The proprietary product surface stays in Vixen. It is where the hard operational
value lives:

- demand-driven build graph, scheduling, invalidation, persistence, and cache
  policy
- production virtual filesystem, sandboxing, FSKit integration, Windows/Linux
  isolation, and remote execution boundaries
- host capability probing and verification against live machines
- product host functions for Cargo, Xcode, MSVC, package managers, caches,
  credentials, secrets, policies, CI, cloud control planes, and customer
  integrations
- app/service deployment, billing, tenancy, organization model, UI, and
  proprietary observability
- the production REPL/debugger connection to live application state

The host can depend on the open kernel. The kernel should not grow product
knowledge just because the product has a convenient implementation detail.

## Boundary

The reusable boundary is "language kernel and typed execution substrate", not
"build system".

Open:

- parse this program
- represent this syntax
- report this diagnostic
- type-check this expression against these Facet-visible types
- lower this expression into canonical IR
- execute this IR against a supplied host trait object
- serialize, hash, compare, and inspect values through the shared substrate

Closed:

- choose which build nodes run
- decide when cached results are valid
- route work to executors
- mount and police a production sandbox
- implement product host functions
- expose customer/project state
- handle credentials and tenant policy

The open kernel may define the host ABI and capability vocabulary. The private
host decides what capabilities exist in the product, how they are implemented,
and which customer or executor is allowed to use them.

## Why The Boundary Is Defensible

Syntax can be copied. A parser can be rewritten. That is not the moat.

The difficult part is making a demand-driven build product that can safely and
quickly evaluate code against real host state, prove cache correctness, isolate
work, observe failures, explain results, and survive platform-specific details.
That stays in the host.

Opening the kernel is still valuable because it makes the language canonical. It
lets the parser, LSP, formatter, diagnostics, IR, Weavy lowering, and Facet value
integration mature under real use instead of hiding behind one product runtime.
It also gives Vixen a better language faster, because every bug found by Fable,
facet-json, Styx, or a tiny reference host can improve the same substrate.

## Near Path

1. Keep hardening the shared substrate in Facet: Weavy correctness, fuzzing,
   format coverage, and interpreter/JIT observability.
2. Start the open Vixen language kernel in the Facet monorepo with grammar,
   parser, CST/AST, diagnostics, and a tiny reference host.
3. Share language-tooling pieces with Fable instead of building separate parser,
   LSP, REPL, and diagnostic stacks.
4. Lower a deliberately small core into Fable/Weavy and prove that values can be
   inspected, transformed, hashed, serialized, and executed through the same
   substrate.
5. Keep the Vixen production host in this repository, consuming the open kernel
   and providing the real build-system capabilities.

## Non-Goals

- Do not move the production scheduler, cache policy, VFS, sandbox, executor
  placement, or customer integrations into the Facet monorepo.
- Do not encode Vixen's proprietary host functions as open-kernel builtins.
- Do not stabilize a host ABI before the reference host and production host have
  both exercised it.
- Do not fork the language into "open Fable-ish scripting" and "private Vixen
  build language".
- Do not block language-kernel progress on the full production runtime.

## Open Questions

- What is the exact crate layout in the Facet monorepo?
- Is Fable always an intermediate layer, or does Vixen lower some core forms
  directly to Weavy?
- What is the smallest useful host trait surface for the reference host?
- Which diagnostics and LSP pieces should be shared immediately with Fable?
- What belongs in cross-repo conformance tests versus Vixen-only product tests?
