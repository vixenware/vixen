# Command model — every exec is a described, canonical command (draft)

Draft design material, **not normative spec**. Updated after the CLI-grammar
study (`docs/design/command-study/`): the model below is the *post-study* shape.

## Thesis

The number-one job of a build system is running commands, so Vixen understands
*every* command it runs: each `exec` is described by a tool **grammar**,
parsed/constructed against it, and reduced to a **canonical identity**.
Understanding the command is the substrate everything else stands on:

- **Caching.** The key is the canonical *effective command* (§3) — reorderings,
  duplicate flags, alias spellings, and env/file inputs all fold to one
  identity. Canonical alignment is a **correctness** requirement, not a nicety.
- **Linting.** The canonicalizer warns for free: `-O0` overridden by `-O2`,
  duplicate `-DFOO`, conflicting flags.
- **Provenance / observability.** "What ran" is structured data — inputs,
  outputs, flags-by-meaning.
- **Tooling.** Full grammars give best-in-class shell completions and an LSP over
  build commands. (Payoff, not motivation.)

Settled. The CLI-grammar study (10 tool groups) existed to find the subtleties
*before* committing the formalism — it did. What follows is what survived; the
verdict and per-tool evidence are in `command-study/SYNTHESIS.md` and the sibling
reports.

## What the study changed

The first sketch — recursive `Grammar { flags, positionals, subcommands: Map }` +
per-flag `Policy` + canonical-argv — holds **additively** for parsing and the
simple-majority of flags, but three structural assumptions were false. They are
**replaced, not patched**.

### 1. Grammar: guarded productions, not a closed subcommand map

Dispatch is not "match the next bare token in a closed `Map`." The active grammar
is a function of accumulated parse state:

- **Mode is often a flag or flag-value** (`cc -c`, `link /lib`, `tsc -b`,
  `ghc -e`), reshaping which flags are legal and what positionals mean — 8/10
  groups.
- **Boundaries** (`--`, mode tokens, plugins, ext dispatch) hand the tail to a
  dynamically-selected, often statically-unknowable **foreign** tool, nesting ≥3
  deep (`stack → cabal → ghc → ld`) — 9/10 groups.

So a grammar is built from **guarded productions**: a predicate over accumulated
state selects the next production — PEG-with-semantic-predicates / a small
attribute grammar — plus an explicit **`OpenForeign`** edge for "the rest belongs
to a tool resolved at runtime." The closed `Map` is just the degenerate case
(guards that test one bare token).

### 2. Canonicalization: local policy + command-level reducers + coupling

Per-flag `Policy` survives for most flags. But canonicalization is, in general, a
**reduction over the whole parsed command**, not a tag read off one flag:

- **Cross-flag coupling** needs edges *between* flags — alias collapse
  (`-O` ≡ `-Copt-level=3`), shared ordered accumulators (`-I`/`-isystem`/
  `-iquote`), propagation (`-race` ⇒ `-covermode=atomic`), mutual exclusion
  (`-race`/`-msan`) — 8/10 groups.
- **Open-keyspace folds** need user-supplied **reducers** against a
  lattice/algebra — rustc's lint lattice (`forbid` floor, `--cap-lints` clamp),
  ghc's set/unset/promote/demote warning algebra, `go mod`'s edit-script replay.

So `Policy = small local enum + Reducer(fn)` over an explicit **alias/coupling
graph**. The reducers are domain logic and live in the tool's package
(`@vixen/rust` supplies rustc's lint lattice) — the same "domain logic in
packages" line as the grammar itself.

### 3. Identity: the resolved effective command, not the argv

The cache key is **not** a canonical argv string. It is the **resolved effective
command**: canonical argv + the explicit env the tool consumes + expanded
response/config/lock files + inputs discovered from file contents + impurity
marks.

**On env — there is no ambient leakage to defend against.** Vixen never inherits
the host environment; an exec's env is always explicit and already
content-addressed (a value the build constructs). The "invisible env surface" the
study catalogued (`CL`, `NODE_OPTIONS`, `CGO_*`, `MAKEFLAGS`) is a flaw of
host-env-*inheriting* build systems, not ours. What the grammar adds is
**declaring which env channels a tool reads**, so the toolchain provides them
explicitly, they enter the identity, and we can lint when a behavior-affecting
channel is unset. Explicit by construction; the grammar makes the dependency
declarable.

**This is the capability invariant at the command layer.** "No ambient effects,
every input content-addressed" — the object-capability model — *is* #3 one level
down: a command's true inputs (argv, the env it reads, the files it expands, the
inputs it discovers) are captured into its identity, or the command confesses
impurity. The command model and the capability model are one discipline.

## Additive axes (enum growth, not new shape)

Everything else is additive on existing axes — ~25 `Spelling`, ~30 `Policy`, ~35
`Kind` variants, tabulated in `command-study/SYNTHESIS.md` §1–3. Highlights:

- **`Spelling`** grows to cover `DualLead` (`/` vs `-`), colon/glued/
  negation-suffix forms, optional values, embedded `k=v`, dotted-path names,
  response files (`@file`), toolchain selectors (`+nightly`), end-of-options →
  nested grammar, foreign-argv strings.
- **`Kind`** grows to distinguish path sub-kinds the cache must treat
  differently: `PathOutDir`, `PathInOut` (in-place rewrite — keys on
  input-content + ops → output), `ArgFileRef` (contents are argv),
  `IndirectInputs` (graph discovered from a file), `LibName`/`LibSearchDir`/
  `Framework`, `PathMap` (prefix remap), `MountPoint`/`LogicalPath` (string-only,
  never hashed), `Exe` (the resolved binary's identity is itself an input),
  `EnvPath`, `Url(impure-network)`, `ForeignProgram` (uncanonicalizable →
  impure).
- **`Passthrough(sep)` → `Passthrough(payload: Grammar)`**, recursive, for the
  2–3-level value sub-languages (`-Wl,` → ld argv; codesign requirement DSL;
  SBPL).

## Canonicalization hazards

Per command, the canonicalizer must either **neutralize** a nondeterminism source
(inject a determinism flag, normalize/redact a value) or **mark the command
impure** (key on inputs, never assert output-byte equality). The catalogue —
build-time stamps, host/CPU dependence (`-march=native`, `+toolchain`),
absolute-path leakage, parallelism-order, network/secrets, stateful temp dirs —
is `command-study/SYNTHESIS.md` §6.

## The escape hatch is impure and punished

A command with no grammar — a one-off tool, an undescribed one, or a hazard we
can't neutralize — is allowed only as an explicit **impurity confession**: opaque
to canonicalization, it forfeits clean caching, lint, and structured provenance,
and it is deliberately onerous to reach for. The expected path is always a
described command. Exact spelling TBD.

## Orthogonal / later

- **Completions + LSP** over build commands fall out of having full grammars,
  best-in-class, for free. Tracked, not built yet.
- A **custom syntax for CLI-argument literals** may make sense, but it's
  orthogonal to the model and deferred.

## Building it

Build the `cc` grammar against **libpng** (`crates/fixtures/libpng.vx`) first — it
stresses the spelling/policy/kind axes and the #2 coupling — then generalize. The
ten study reports under `command-study/` are the reference corpus.
