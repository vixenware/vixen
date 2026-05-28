+++
title = "FAQ: settled decisions"

[extra]
type = "Doc"
updated = "2026-05-28"
+++

These questions come up constantly — from teammates, investors, users, and their grandma. **The answers are settled.** This page exists so we point at it instead of re-litigating the same objection a hundredth time. Changing one of these is a new decision backed by new evidence, not a re-run of the same argument.

The thread through all of them: the objections assume the language is an *ergonomics layer* you could swap for an existing one. It is not — it is a *semantic substrate* whose guarantees (content-addressed values, no ambient effects, demand-driven memoization) are what make the whole build graph cacheable, shippable, and observable. That is not portable onto Python, and it is not something a Rust crate can be.

## "Why a language?"

Because the build *is* a computation graph of typed values, and you need a way to describe that graph that the runtime can hash, memoize, ship to a remote executor, and introspect — uniformly, for every node. That is not a UI concern, it is a semantics concern. The language exists to *enforce* three invariants that make correct universal caching possible:

1. Every value is hashable / comparable / serialisable, so any node's identity is its content.
2. No ambient effects — capabilities are object-types you must be handed, so a build step can't secretly read `/etc/passwd` and invalidate the cache key behind your back.
3. Demand-driven evaluation with memoization at intrinsic boundaries.

Those invariants *are* the language. You can't express computation in a config format (YAML / Styx can't compute a dep graph), and you can't get the invariants from a general-purpose language (see below). The semantics are the product. See [Build graph is also a provenance graph](/architecture-and-infrastructure/#build-graph-is-also-a-provenance-graph) and [Domain logic lives in Vixen](/vixen-language/#domain-logic-lives-in-vixen-not-in-the-runtime).

## "Aren't languages hard?"

The hard parts of languages are: novel semantics research, an optimizing compiler, broad ecosystem / stdlib, and stability commitments. We dodge nearly all of them. The semantics are deliberately small and conservative — pure, no mutable references, no identity, no exceptions; the surface is basically function application plus a few literal forms. The heavy algorithms aren't *in* the language, they're Rust intrinsics. We owe no stability until Tier 3, so it iterates freely as internal substrate for years. And the domain is narrow — build description, not general computation — so the surface stays small. This isn't building Rust or Swift; it's building a typed, cacheable Make whose hard work is delegated to native code.

## "Can't we use Python / TypeScript / Starlark?"

- **Python / TypeScript**: ambient effects everywhere — any function can touch the filesystem or network, so invariant #2 is gone and the cache is unsound by construction. Their values aren't content-addressable, and their closures don't serialise to a canonical IR you can ship to a remote executor and hash. You'd fight the host language forever and *still* not get the properties.
- **Starlark**: this objection actually *proves the thesis*. Google hit exactly this wall — Python wasn't deterministic / hermetic enough for builds — so they built a new language. The doubter reaching for Starlark is conceding "yes, you need a purpose-built build language." Starlark just isn't *ours*: it's untyped (we need types for the capability / ability model and typed parsing into Vixen values), its caching is bolted on at Bazel's action level rather than being intrinsic to value evaluation, and it has no capability object-types, no closures-as-shippable-IR, no streams. The one mainstream "use an existing language" answer is itself a from-scratch language.

## "Can't we just use a Rust crate?"

We do — for the parts that should be crates. The intrinsics (PubGrub, TOML / JSON parsing, hashing, fetch, exec) *are* Rust. The question is where the *orchestration* lives, and there's a category difference: `.vx` logic is **data the runtime evaluates** — inspectable, hashable, shippable, version-pinnable as `@vixen/rust@2.4.1`. A Rust crate is **opaque code**. If cargo-resolution is a crate, then to the cache it's a black box: you hand-write a cache key for it, hand-instrument its tracing, hand-derive its provenance — N bespoke integrations, one per function, done badly. If it's `.vx`, you get content-addressed identity, memoization, provenance edges, and observability for free, once, from the evaluator. Plus a crate runs in-process with ambient authority — no sandbox, no capability story. "Logic in Rust" and "logic in .vx" aren't two implementations of one thing; only one of them is a value you can compute on.

## "Why not just shell out to cargo / sccache / Bazel / Nix?"

Because the value is not "run the build" — every tool does that. The value is that **every step of the build is a typed, content-addressed node in one graph**: dependency resolution, `build.rs`, proc-macro expansion, codegen, link, fetch, template render — all the same shape. Once a step is a node, caching, memoization, provenance, license / origin policy, cost attribution, affected-test selection, time-travel replay, and cross-target fan-out all fall out of **graph traversal**. They are not features bolted onto each step; they are what a node *is*.

Shelling out to an existing tool buys you that tool's one cache layer (sccache caches rustc; Bazel caches its actions; Nix caches derivations) and an opaque blob everywhere else. You cannot ask an opaque blob "what did you read," "what produced this," "re-run just the affected slice." Vixen can, for everything, because everything is a node.

If the goal is to open a restaurant, "you can get a burger at McDonald's for less effort" is not a counter-argument.

## "People won't want to learn a new language."

Mostly they never touch it, and that's the whole tiering design. CCC: zero language, it's cargo. Vixen Build: `vx cargo build`, customers never author `.vx` — *we* write `@vixen/cargo`. The only tier where a customer writes Vixen is Tier 3, and that's an opt-in choice by people who specifically want typed, cached CI with fly-check and provenance — they're reaching for it, not having it forced on them. The business never depends on language adoption; it depends on the language *existing* as substrate. Contrast Bazel forcing Starlark on everyone day one. By the time an external person writes `.vx`, it's had years of internal dogfooding shaping its surface. And when they do learn it, it's small and the payoff is concrete.

## "Won't a new language make things hard to debug / observe?"

Backwards — this is the biggest *win*, not a cost. Because every value is content-addressed and every intrinsic call is a graph node, the build is inspectable data:

- Dump the graph: every node has its inputs-by-hash, output-by-hash, producing intrinsic, timing, executor.
- "Why did this rebuild?" = diff input hashes between two runs; the runtime points at the exact input that changed. Cargo and Make literally cannot answer this — they just rebuild.
- Provenance is a graph query, not a logging campaign. Tracing is intrinsic because the evaluator threads every node — no sprinkled `println`.
- Determinism makes failures replayable by hash.

Compare a Makefile / shell build: opaque subprocess soup, no record of what read what, rebuild-the-world to debug. The language makes the build *more* observable than scripts, by construction. The real cost is you must build language tooling — LSP, good diagnostics, highlighter — which is exactly what `vx-lsp`, the `margin` structured diagnostics, and `tree-sitter-vixen` are.

## "An interpreted language is SUPER slow — my cargo build will be much slower."

Two parts.

First, *what's actually slow in a build, and where does `.vx` sit?* Your wall-clock is rustc + linking + resolution + fetch. The `.vx` code does none of that work — it *describes* the graph and decides what to run. The work is rustc (native subprocess), fetch (native intrinsic), PubGrub (native intrinsic), parsing (native intrinsic). Interpreting the orchestration layer is microseconds-to-milliseconds against *minutes* of compilation. You never interpret the hot loop; rustc is the hot loop and it's the same rustc. The interpreter overhead is a rounding error.

Second, and more important: the build gets *faster*, because caching is the point. Cold, you pay rustc once. Warm, unchanged units are cache hits — no rustc, no re-resolution (same manifest + registry → cached lockfile), no re-fetch. The honest comparison isn't "interpreted vs native cargo," it's "cold cargo vs a build where most of the graph is a cache hit, skipping rustc entirely." That's orders of magnitude; the interpreter cost is in the noise. And if the orchestration layer ever *does* get measurably slow on some 10k-crate monster, that's a runtime perf problem with runtime answers — better intrinsics, JIT when it earns its place (it was removed because it wasn't paying for itself *yet*, not abandoned) — never a reason to move logic out of `.vx`. The floor is rustc speed; the ceiling is skipping rustc. See [Which intrinsics get persisted](/cache-and-trust/#which-intrinsics-get-persisted).

## "Reimplementing cargo's resolver in .vx — features, [patch], target deps, resolver v1/v2/v3 — is a multi-year rabbit hole. Just call cargo's resolver."

The hard algorithms are **Rust intrinsics**, not .vx. PubGrub is an intrinsic; TOML / JSON / YAML parsing is an intrinsic; hashing, fetch, and template render are intrinsics. What lives in `@vixen/rust` is cargo's *domain model* — what a workspace is, how features unify, what `[patch]` means — calling those intrinsics. That glue is small, and because it is .vx it is cached and memoized for free: same `(manifest, registry view, resolver version)` → same lockfile → instant, with no bespoke resolution-cache to write.

And the bar isn't byte-identical output — it's **functional equivalence**: resolve the same versions, build correctly. Matching cargo's exact lockfile text or tie-breaking in every corner is a needlessly high bar for no real benefit, and not a goal.

Calling cargo's resolver hands you back one opaque step you can't cache incrementally, can't trace, can't get provenance from. That defeats the entire point above. The reimplementation is not avoidable overhead — it is **where the product's value is created**. See [PubGrub solves both](/build-and-resolution/#pubgrub-solves-both-cargo-and-vixen-package-resolution) and [Cargo-resolution-as-Vixen, lockfiles-as-outputs](/vixen-language/#cargo-resolution-as-vixen-lockfiles-as-outputs).

## "Does CCC need any of this?"

No — and that's deliberate. CCC is a cargo fork + ccc-backend + Store + a descriptor-keyed `landlock-exec` backfill fleet. It never evaluates Vixen. The language and runtime are Vixen Build's substrate, funded by CCC revenue but off CCC's critical path. Keeping them decoupled is a feature, not an accident. See [CCC → backfill is direct landlock-exec](/ccc/#backfill-is-direct-landlock-exec-not-the-vixen-runtime).
