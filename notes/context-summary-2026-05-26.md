# Vixen context summary — 2026-05-26

Written at the end of a long session that converged on architectural shape but didn't ship build-semantics code yet. Future-me (or any other agent) should read this before continuing the substance discussion.

## What just shipped this session

The diagnostics arc, end-to-end:

- `IrDiagnostic` (vx-ir) and `ParseDiagnostic` (vx-parser) are structured enums. **No strings inside variants.** Every name is a `SourceSpan`, every structural ref is an ID (`ItemId`, `TypeId`, `BindingId`), every kind is an enum (`BinOp`, `CapSet`, `RelatedKind`).
- Each diagnostic has `primary_span()`, `related_spans()`, `to_report(env) -> margin::Report`. The renderer (vx-diagnostics) is universal — only knows `margin::Report`.
- **Anonymous types are first-class.** `TypeEnv` interns every `Ty`; old `TypeId` (def index) renamed to `TypeDefId`, new `TypeId` indexes `Vec<Ty>`. `Ty`'s recursive fields became `Vec<TypeId>` / `TypeId`. Diagnostic carries `TypeId`. This was the "anonymous types are just types, not second-class" refactor.
- Daemon RPC: `EvalStrictFunctionFailure { message: String }` is now `{ diagnostics: margin::Diagnostics }`. Full structured Source + Report on the wire. `CompileError` variants carry `Box<TypeEnv>` so the renderer can resolve TypeIds at the boundary.
- vx CLI: `vx eval` failure renders via `margin-term` + `VixenSyntaxHighlighter` (tree-sitter-vixen + arborium themes). Light/dark theme auto-detect via `terminal_light::luma()` with `VX_THEME` env override, OnceLock-cached (documented trade-off — fine for short-lived CLI, will need invalidation if we ever add a TUI/REPL).
- vx-lsp: `related_information` on diagnostics from `IrDiagnostic::related_spans()`. Code actions / editor jumps to param decl sites, slot decl sites, candidate methods, etc.
- Sweep: resolver dedups `Unresolved` diagnostics by `(source, name)` (no more N "unresolved name `config`" for N field accesses); 2 pre-existing `too_many_arguments` clippy warnings fixed (BoundaryAccumulator struct, ExecBoundaryLayout struct); dead `examples/ariadne_arborium_vixen.rs` and its 5 dev-deps deleted; one TypeId(0) placeholder in a vx-lsp test replaced with real interned Ty.

Workspace state: **338 tests, all passing**, clippy clean.

Tip of main: `9533d623` (or possibly later — check `git log` if reading this in a new session).

## Architectural decisions that landed (already done)

**A. Structured diagnostics, zero strings inside variants.** Every piece of info with a source origin is a span. Every structural ref is an ID. Strings appear only in the renderer's output and in synthesized prose (HintKind enum, not freeform text).

**B. Anonymous types have TypeIds.** Function/Tuple/Array/Map/Stream/Optional/Scalar/Unit/Param/Infer/Poison all interned in `TypeEnv`. No nominal/anonymous distinction at the reference layer.

**C. Wire ships structured diagnostics, not strings.** `margin::Diagnostics` (which has Facet derives) crosses the daemon-CLI boundary. Stringification happens at the last mile (CLI render, LSP message-formatter).

**D. The cluster (vx-vfs, vx-vfsd, vx-store, vx-runner, landlock-exec, etc.) stays in monorepo.** We considered splitting into a vfs/sandbox repo. Decision: don't split until build-semantics work has stabilized the VFSD/store/runner interface. Pre-stabilization splits cause the same pain as the lang/lsp/runtime split earlier this session (which we then re-merged).

## Outstanding architectural decisions (not made yet)

These are the BIG ones the conversation surfaced but didn't resolve:

### 1. Cache key correctness for exec

The language is demand-driven with automatic memoization. Caching is not optional — it's how the language works. Two exec calls with the same key MUST produce the same result, or the cache is silently wrong.

What goes into the key?
- The binary hash (rustc's bytes)
- argv
- Some subset of env (which?)
- Mounted Tree hashes
- stdin hash
- Working directory (sandbox-relative)
- ??? the transitive deps the binary reads at runtime (libc, /etc/passwd, /proc/cpuinfo, ???)

**This is fundamentally tied to the sandbox design.** The sandbox has to be tight enough that nothing outside the key can affect output. If rustc can read `/etc/something` we didn't hash, the cache is wrong. Nix solves this with FHS-free, store-path-only, sealed-/proc-and-/etc sandboxing. Vixen needs equivalent property.

**The cache key and the sandbox are one decision, not two.** Worth writing a design doc that nails them down together before any cache code lands.

### 2. Trust model for shared cache

Even if cache keys are correct, anyone who can write `(key, value)` pairs poisons the cache for everyone who reads. Cache poisoning attack.

Options considered:
- **Per-user / per-org cache** (no sharing) — loses the value prop
- **Signed entries** (Nix substituter model) — producer signs, consumer checks against trust list. Trust is human-decided.
- **Reproducibility quorum** — N independent producers, accept iff they agree. Requires bit-reproducibility.
- **Attested execution (TEE)** — SGX/SEV/Nitro/Apple Secure Enclave. Hardware as trust root. Strongest, doesn't deploy everywhere.
- **Local-recompute verify** — defeats the purpose

Suggested shape (not yet ratified):
- **Trust is a property of a PATH INTO the store, not of the store itself.**
- Local CAS store is unsigned (you put things in it).
- Anything from elsewhere flows through a *gate* that checks signatures/attestations before insertion.
- Cache stores `(key, value, [signature chain])`. Daemon's config decides which signatures are acceptable.
- Caching ships **WITH** the trust gate from day 1. Not "cache first, retrofit signing later" — that's how you get CVE-grade poisoning.

### 3. Process model

Earlier in the session I floated: `Process` is NOT first-class (no mid-run polling, no live stdout backpressure observation). Just exec → run → result. Reason: deterministic caching is incompatible with live observation of process state.

Amos pushed back later — we ALSO need streams (proc.stdout as `Stream[Blob]`, observer returning `Stream[T]`, parse-lines, parse-JSON). So the answer is more nuanced:
- The PROCESS itself isn't observable as a mutable handle
- But ITS OUTPUTS (stdout, stderr) are streams that can be consumed and transformed
- The observer is a closure that gets `(Process, Tree)` and returns SOMETHING — including `Stream[T]` which streams back values while the remote work is still running
- This is the right shape — let me know if I'm misremembering when this comes back up

### 4. Executor identity / class / selection / fan-out

Today: `executor_class("default")` is essentially a placeholder. Everything runs on the local host. We know we need:
- Real executor classes (linux-x86_64, darwin-arm64, etc.)
- Multiple executors per class
- Selection / fan-out (build for all platforms)
- Closure shipping with capabilities to the chosen executor
- Stream-return from a still-running remote computation

Not designed yet.

## Where we landed on next steps

**The whole session converged on "drive by primitive, not by user scenario."**

I had proposed M1 = "compile hello.rs with caching, watch it fail, fix what's needed." Amos pushed back: hello.rs is too narrow, exercises a tiny slice, succeeds quickly, teaches us nothing about the harder primitives.

The right driver is the primitive set we KNOW we need:

1. **Streams as runtime values** (`exec → Stream[Blob]`, observer returns `Stream[T]`)
2. **Stream ops**: `lines`, `parse_json[T]`, etc.
3. **Closure shipping** (even with one executor, formalize what `(host) => host.which(...)` does architecturally)
4. **Executor class / selection / fan-out** (register multiple executors, exercise selection)
5. **TreeEntry / file-into-sandbox** (independent)
6. **Cache key derivation + sandbox design** (last; gates correctness)
7. **HTTP fetch + tarball extract** (independent)
8. **Multi-source dep resolution** (eventual)

Each is independently implementable with its own narrow failing test. M1 / M2 / M3 are integration checkpoints, not implementation drivers.

**The next-next question Amos raised, which we didn't resolve before pausing:** what are the cache semantics RIGHT NOW for exec? Answer found by survey: **exec is never cached today, on purpose-by-default.** No key derivation, no cache lookup, no cache write. That's the right state pre-decision — better than a half-baked cache that's silently wrong.

## Meta-tracking situation

We talked about how to track this kind of multi-thread architectural work durably (not just in chat logs / Linear / agent context). Converged:

- Use **dodeca** (Amos's static site gen) as substrate
- **Issue [bearcove/dodeca#268](https://github.com/bearcove/dodeca/issues/268)** filed: serve rendered markdown alongside HTML so agents can consume the wiki
- **Issue [bearcove/dodeca#269](https://github.com/bearcove/dodeca/issues/269)** filed: first-class frontmatter schemas via styx (with `@link(@T)` as the one dodeca-specific addition for typed cross-page refs)
- Once those land, `vixen/wiki/` becomes the home for Vision / Milestone / Decision / Question / Experiment / ArchitectureNote / Glossary pages. Until then, NO bootstrapping — risk of producing slop that won't match the eventual schema.

**Slop prevention**: agreed agent writes scaffolding (schema, templates, validator), Amos writes substance (Vision, Decisions, Milestone narratives) — agent never authors the IDEAS, just helps shape them into the corpus format.

## Open questions to pick up next session

Top of stack, in order:

1. **Should we write a design doc on "cache key correctness + sandbox design"** before writing any cache code? My instinct: yes, this is the kind of decision that wants to be written down, not chat-resolved. Amos's instinct?
2. **Which primitive to drive first?** Streams was my top choice (foundational + interesting + observable in tests). Executor classes also defensible (THE foundational thing for the "remote execution is first-class" pitch). Closure shipping is the deepest architectural decision.
3. **Hosted vfsd in tests.** Current test infra creates daemon configs that omit vfsd. Anything beyond "stdout collect" requires the hosted vfsd. Need a `write_daemon_config_with_vfsd` helper.
4. **The two integration tests named `eval_auto_starts_daemon_and_runs_rustc_hello`** don't actually run rustc. They eval `fn one() -> U64 ( 1 )`. Misnamed from earlier project ambitions. Either rename or replace with a real rustc-running test.

## File state

- Tip of main: `9533d623` (verify with `git log`)
- Outstanding uncommitted changes: should be none (this doc when committed will be the next change)
- All cargo nextest tests passing, clippy clean

## Conversation texture notes (for fresh-context me)

- Amos pushes back hard on lazy framing ("real syntax" when I meant YAML-shaped mockup; "Tested with exec [...]" when streams aren't actually settled yet). Look at code before claiming things.
- Don't generate vision/strategy text. Amos called it "slop." Agent role is scaffolding + validation, not authorship.
- Multiple-choice questions are condescending. Amos is an engineer. Commit to a position, defend it, accept push-back.
- Visible payoff is NOT the criterion for ordering work. Architectural correctness is. "Get to first user-facing demo" was rejected as a tiebreaker.
- Reverts are forbidden. Negative results get committed and discussed, not unwound.
- Style: dense, direct, no preamble, no recap of what they said back at them. Match their energy.
