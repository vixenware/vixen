# command-model — roadmap

`command-model` describes a CLI tool's argument grammar, parses a raw argv into
structure, and reduces it to a **canonical effective command**: the resolved
identity a content-addressed cache keys on, and the basis for linting,
completion, docs, and provenance over every command a build runs.

The thesis (see `docs/design/command-model.md`): a build system's number-one job
is running commands, so Vixen understands *every* command. This document records
what that understanding buys, what's built, and what's left.

## Status snapshot

Built and tested (zero deps, 19 tests, clippy-clean):

- **Parse + canonicalize**: argv → structured occurrences/positionals/unknowns →
  one canonical argv (reorderings/dups/alias-spellings collapse).
- **Effective-command closure**: identity = canonical argv + declared env the
  tool consumes + path-kind inputs (argv paths, env paths, bubbled-up
  passthrough paths) + impurity marks. Env is explicit-only; no ambient
  inheritance.
- **Axes**: `Spelling` (Bare/Attached/Separated/Suffixed/Passthrough),
  `Policy` (Once/Group/DedupOrdered/DedupUnordered/OrderedKeepAll/DefineProgram/
  Positional), `Kind` (Str/PathIn/PathOut/Include/LibSearchDir/Lib/Define).
- **Recursive passthrough**: `-Wl,` is an `ld` sub-command, canonicalized
  recursively.
- **Mode-by-flag guarded productions, first cut**: the legal flag set depends on
  parse state (`-l` after `-c` is an impurity).
- **Grammars**: `CC`, `LD`, `CL` (cc/clang, ld, cl.exe/clang-cl).

## Capability targets

What the model is *for*. Status: ✅ built · 🟡 enabled, small additive work · 🔴 gap needing design.

1. **Parse argv → structured, role-per-arg, validity, canonical.** ✅ core is
   built. 🟡 expose a public role-tagged `ParsedCommand` (every token with its
   resolved flag + role + value) — the data exists internally.
2. **Structured → executable command line.** ✅ inherent (`emit` inverts parse;
   canonical argv is runnable). Caveat: canonical reordering must stay
   semantically safe (that's what `Policy` is for); link-order is the known
   incomplete case.
3. **Generate shell completions (bash/zsh/…).** 🟡 the grammar *is* the source;
   needs a generator + **value domains** (enumerable/typed flag values).
4. **LSP autocomplete of invocations.** 🟡 same grammar + a "legal-at-cursor"
   query; guarded productions make it mode-aware. Needs value domains.
5. **Docs on every option (incl. positionals), LSP hover.** 🟡 flags need a
   `doc` field. 🔴 positionals need the **positional schema** (study B7).
6. **Validate invocations — diagnostics, where/why/how-to-fix.** 🟡 structured
   lints + impurities exist; needs **spans** (token positions, already tracked
   internally) + fix suggestions.

Most of 3–6 are gated on a small shared set of additive features (below), not on
new core architecture.

## Additive features (shared by 3–6)

- `doc: &str` (summary + long) on `Flag` / `EnvDecl` / positionals.
- **Value domains**: a flag's value space — a fixed enum (`-std` ∈ {c89,c99,…}),
  a path/dir, a lib name, or "from this command" — for completion + validation.
- **Diagnostic spans**: thread the token position (already in `Occurrence`) into
  every `Lint` / `Impurity`.
- **Positional schema**: positionals as a typed, possibly mode-dependent
  sequence, not generic `Source` inputs.
- **Public `ParsedCommand`**: the role-tagged structure (#1).
- **Construct / builder API**: build a `Command` from intent and emit canonical
  argv — the inverse direction, the bridge to `@vixen/c`.

## Structural pillars still ahead

From the CLI-grammar study (`docs/design/command-study/`), in descending weight:

- **Reducers + coupling graph** — `Policy = enum + Reducer(fn)` over an
  alias/coupling graph: the `-W` set/unset/promote/demote algebra, alias
  collapse (`-O`≡`-Copt-level=3`), cross-flag exclusion, lint lattices.
- **`OpenForeign` edge** — the deeper guarded production: the tail after
  `--`/plugins dispatches into a grammar resolved at runtime
  (`bindgen -- <clang>`, `protoc --plugin`).
- **Discovered-from-content inputs** — `@file` response-file expansion,
  debug-map fan-out: real inputs found inside a file's contents, not on argv.
- **Caching-hazard neutralizers** — the study's §6 catalogue: detect and inject
  fixes for `__DATE__`, `-march=native`, abs-path leakage, ar mtimes, or mark
  impure.
- **Link-order semantics** — object/`-l` interleaving.
- **Windows env separator** (`;` vs `:`).

## What this unlocks (dream bigger)

The structured, canonical, bidirectional model is more than a parser:

- **"Why did this rebuild?" at the command level.** Diff two
  `EffectiveCommand`s → "this `-D` changed, this input's hash moved." The cache
  points at the exact flag/input that broke reuse. Make/cargo can't.
- **Cross-dialect translation.** Parse a gcc command, re-emit the equivalent
  MSVC command. Mechanical, via the role-tagged structure. For cross-compile,
  for wrapping zigbuild/xwin, for migrating build scripts.
- **Determinism auditing + auto-neutralization** (study §6). Reproducible-builds
  superpower: detect non-determinism and inject the fix.
- **Policy/security queries over the whole build.** "Every command that disables
  stack protection / touches the network / uses a banned flag." The build
  becomes queryable — the SBOM/compliance story, nearly free.
- **Ingest `compile_commands.json` / build logs.** The *parse* direction lets
  Vixen import a foreign CMake/Make/Bazel build's actual commands and cache
  them — onboarding without a rewrite.
- **"Explain this command."** Paste any argv → human breakdown of every flag and
  what it reads/writes. Man-page killer; same hover machinery as #5.
- **Transparent tool wrapping/interception** — parse → rewrite → re-emit is the
  basis for caching wrappers, sandboxing, instrumentation.

## Decision: grammars are Vixen code; build comprehensively

**Authoring language — decided.** Grammars are authored in **Vixen code**
(`.vx`), distributed through the package manager like any `@vixen/*` package.
Rationale: a grammar is mostly declarative data (clean as Vixen records/maps,
which already read styx-ish) but its distinguishing parts — guards (mode/state
predicates), reducers (the `-W` algebra, lint lattices, define programs), and
OpenForeign edges — are genuinely computational, and a declarative format (styx)
hits a wall exactly there. Vixen code clears it; no new language *syntax* is
needed (reuse records + the constraint-expression algebra for guards + a
`Grammar`/`Flag`/`Policy` type library).

**Guards and reducers stay structural / introspectable** — reusing Vixen's
constraint-expression machinery (operators yield introspectable values, not
opaque closures). Not to preserve standalone-ness, but because the consumer
features (completion, hover-docs, explain, translate, diff) *functionally
require* introspectable predicates. A genuinely Turing-complete reducer falls
back to runtime-evaluated Vixen IR.

**Runtime coupling is acceptable.** We are making this *for* Vixen and Vixen
*for* this; we won't contort the design to keep `command-model` standalone if
that costs more than it's worth. The Rust crate is the engine; grammars are
Vixen values that lower to its `Grammar` form.

**Scope: comprehensive now, not MVP slices.** CLI argument schemes have been
stable for 20–30 years — this is not a moving target. So we build the *complete*
model up front (every pillar and capability target above), so we don't redesign
later. New *flags* get added as tools grow; the *architecture* is built whole
now. The "first cut" / "future work" framing elsewhere in this doc is superseded:
all of it is in scope.

## The hard, unglamorous cost

The real investment nobody's costed: **authoring and maintaining accurate
grammars at scale.** These CLIs are genuinely nightmarish (cargo, rustc, MSVC,
ghc's warning algebra), and they drift as tools evolve. We will need:

- a grammar-authoring format that's pleasant (not raw Rust),
- generation from `--help` / man pages / the study reports as a starting point,
- **differential testing** of each grammar against the real tool: does our
  parse+emit match what the tool accepts? does our impurity detection match its
  errors?

That testing is what makes everything above trustworthy. Plan for it now.
