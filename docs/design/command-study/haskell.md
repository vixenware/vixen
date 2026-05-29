# Command study: the Haskell tool group (ghc, cabal, stack)

Studied against the Vixen command model:

```
Grammar { flags: [Flag], positionals: [Positional], subcommands: Map<String,Grammar> }
Flag    { lead, spelling: Spelling, policy: Policy, kind: Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

The Haskell group is the single hardest stress test in any toolchain study, because GHC's
flag namespace is *not a flat set of flags* — it is a layered prefix algebra (`-opt*`, `-pgm*`,
`-W*`/`-Wno-*`/`-Werror=*`, `-f*`/`-fno-*`, `-d*`, `-X*`/`-XNo*`, `-O0..2`, `-package*`) where
the *prefix itself* determines policy, the *suffix* is an arbitrary sub-language, and **order is
load-bearing across the entire argv**. cabal and stack then wrap GHC and forward opaque blobs to
it, which means the canonicalizer must understand a *grammar-within-a-grammar* to the GHC layer or
it cannot canonicalize the wrapper at all.

---

## 1. GHC — `ghc` / `ghci` / `runghc` / `ghc-pkg`

### 1.1 Invocation shape (subcommand tree)

GHC is **modal, not subcommand-dispatched**. There is no bare-token subcommand; the *mode* is
selected by a flag and the rest of the grammar is conditional on it:

- `--make` (default when given Haskell files): multi-module build, follows imports.
- `-c`: compile/typecheck only, no link.
- `-E`: preprocess only. `-S`: stop after asm. `-M`: emit Makefile dependencies.
- `-e EXPR`: evaluate one expression (REPL-ish, batch).
- `--interactive` (= `ghci`): the REPL, which has its **own internal command language** (`:load`,
  `:type`, `:set`, `:browse`, `:!shell`) reachable only at runtime, not via argv.
- Pure query modes: `--show-iface FILE`, `--info`, `--version`, `--supported-extensions`,
  `--print-libdir`, `--abi-hash`, `--frontend MODULE`.

**Does recursive `Grammar.subcommands` cover it? No.** There is no bare-token dispatch — the
"subcommand" is a *flag* (`-c` vs `--make` vs `-E` vs `--interactive`), and these are **mutually
exclusive mode flags** that *reshape which other flags are legal and what positionals mean*
(`-e` makes the positional an expression, not a file; `--show-iface` makes it a `.hi` path).
This is a **mode flag selecting a conditional sub-grammar**, which `Grammar` (bare-token dispatch
only) cannot represent. Deepest "nesting" is really the **`-opt*` passthrough → full external
compiler/linker grammar** (see §1.2), one level, but the inner grammar is unbounded.

`ghc-pkg` *is* a real subcommand tree: `ghc-pkg {register,unregister,expose,hide,list,find-module,
describe,dump,recache} [pkg]` — depth 2, covered fine by `Grammar.subcommands`.

### 1.2 Spelling forms actually used

GHC uses **every spelling in the enum and several it lacks**:

| Form | Real GHC examples |
|------|-------------------|
| Bare | `-c`, `-S`, `-E`, `--make`, `-threaded`, `-dynamic`, `-prof`, `-Wall`, `-Werror` |
| Attached | `-O2`, `-DFOO`, `-DFOO=1`, `-lm`, `-Iinclude`, `-package-id base-4.x`, `-XOverloadedStrings`, `-fmax-simplifier-iterations=4` |
| Separated | `-o out`, `-odir build`, `-hidir build`, `-package text`, `-main-is Main` |
| Equals | `-fmax-simplifier-iterations=4`, `-fllvm-fill-undef-with-garbage`, `-Werror=incomplete-patterns`, `--frontend=M`, `-frandom-seed=12` |
| Passthrough | `-optl-Wl,--gc-sections`, `-optc-DFOO`, `-optP-include`, `-opta-mavx2` |
| Response file | **`ghc @response-file`** (GNU-style; confirmed in user's guide). Recursively expanded; can `@`-nest. |
| `--` end-of-flags | Used by `runghc -- args` and `ghc -e ... --` to separate prog args. |
| count flags | `-v`, `-v0..-v4` (verbosity; both attached-number AND repeatable-ish). `-ddump-*` accumulate. |

**Forms the `Spelling` enum lacks for GHC:**

1. **Prefix-negation pairs** — `-fstrictness` / `-fno-strictness`, `-XOverloadedStrings` /
   `-XNoOverloadedStrings`, `-Wfoo` / `-Wno-foo` / `-Werror=foo` / `-Wwarn=foo`. The `no`/`error=`/
   `warn=` is **infixed into the spelling**, and the polarity is a *value*, not a separate flag.
   `Spelling` has no notion of "this spelling carries a tri-state polarity for a named knob."
2. **Double-prefix passthrough** — `-optl-Wl,--gc-sections` is `Passthrough(-optl)` whose *payload*
   is itself `Passthrough(-Wl,)` with `,` sep. The enum's `Passthrough(sep)` is single-level; GHC
   needs **nested passthrough** (GHC-arg → linker-arg → ld comma-list).
3. **Suffix-as-knob-name** — `-fmax-simplifier-iterations=4`: the flag *name* is open-ended
   (`-f<word>[=val]`). There is no enumerable flag; the canonicalizer must treat `-f`/`-d`/`-X`/`-W`
   as **open prefix families** with a parsed `(knob, polarity, value?)` triple.

### 1.3 Multiplicity & order semantics → Policy

| Flag class | Required policy | Why / what's missing |
|------------|-----------------|----------------------|
| `-O0/-O1/-O2` (also `-O` = `-O1`) | `Group("opt")` last-wins | Fits `Group`. |
| `-i<dir>` (search path, colon-list), `-I<dir>` (cpp include), `-L<dir>`, `-l<lib>` | `DedupOrdered` | order is semantic for link/search; fits. **But** `-i` is a *colon-joined accumulating list* (`-iA:B`), a sub-syntax `DedupOrdered` doesn't model. |
| `-D<sym>[=val]` / `-U<sym>` | **interleaved define/undef** | `DedupUnordered` is WRONG: `-DFOO -UFOO -DFOO` is order-dependent (last define/undef of `FOO` wins). Needs *ordered last-wins-per-symbol*, not a sorted set. |
| `-X<ext>` / `-XNo<ext>` | last-wins-per-extension, **order matters** | not a set: `-XRebindableSyntax -XNoRebindableSyntax` → off. No existing policy = "accumulate, polarity, last-wins per key." |
| `-f<knob>` / `-fno-<knob>` | last-wins-per-knob | same gap as `-X`. |
| `-W<cat>` / `-Wno-<cat>` / `-Werror[=cat]` / `-Wwarn=cat` | **accumulate-with-tri-state**, order matters | the canonical model-breaker (§1.5). `-Wall -Wno-unused-imports -Werror=incomplete-patterns` is three different *operations* on a warning set. |
| `-package`, `-package-id`, `-hide-package`, `-package-env` | ordered accumulate | order matters for shadowing; `DedupOrdered`-ish but with hide/expose verbs. |
| `-optl`, `-optc`, `-optP`, … | **`Positional`-like (never reorder, never dedup)** | these are opaque blobs forwarded verbatim; reordering changes downstream `cc`/`ld` semantics. |
| `-o`, `-odir`, `-hidir`, `-outputdir` | `Once` (last-wins) | fits. |

**Missing policy (general):** *accumulate-with-negation / polarity-keyed-last-wins*. This single
policy is needed for `-W*`, `-X*`, `-f*`, `-D/-U`. None of `Once / Group / DedupUnordered /
DedupOrdered` expresses "a stream of (key, op∈{set,unset,promote-to-error,demote-to-warn}) updates
where final state per key is the last op, but the *flag namespace is open*."

### 1.4 Path-bearing slots

- `PathIn`: source `.hs/.lhs/.hsig` positionals; `@responsefile`; `-i<dir>` search dirs;
  `-I<dir>` cpp dirs; inputs to `--show-iface FILE.hi`.
- `PathOut`: `-o FILE`; `-odir/-hidir/-stubdir/-hpcdir/-dumpdir DIR` (**output directories**, not
  files — `Kind` has `PathOut` but no `PathOutDir`); `-ohi FILE`.
- `Include`: `-I<dir>` ✓.
- `Lib`: `-l<lib>` ✓, `-L<dir>` (lib search dir — actually a *dir*, `Lib` kind conflates "the lib
  name" with "the search dir"); `-framework NAME` / `-framework-path DIR` (macOS) are **new Lib-ish
  kinds** with no analogue.
- `Define`: `-D` ✓, but `-U` (undef) has no kind.
- **Sneaky paths:** `-optP-include FILE` (a path *inside* a passthrough blob — invisible unless the
  passthrough sub-grammar is parsed); response files referenced inside response files; `-tmpdir DIR`;
  `-pgmc /path/to/cc`, `-pgml /path/to/ld` (**the compiler/linker binaries themselves are paths** —
  hashing the toolchain matters for cache correctness); `GHC_PACKAGE_PATH`, `GHC_ENVIRONMENT`
  **env-valued path lists**.

**New Kinds needed:** `PathOutDir` (output directories), `Framework`/`FrameworkPath` (macOS),
`Undef` (`-U`), `ToolBinary` (`-pgm*` — a path whose *content hash gates the whole cache key*),
`PathInList(sep)` (`-i A:B:C`), and `EnvPath` (paths arriving via `GHC_PACKAGE_PATH`).

### 1.5 Model-breakers

1. **Open prefix families.** `-f`, `-fno-`, `-d`, `-X`, `-XNo`, `-W`, `-Wno-`, `-Werror=`,
   `-opt*`, `-pgm*` are not enumerable flags — they are **productions**. The grammar must carry
   *prefix matchers with structured suffix parsers*, not a `[Flag]` list.
2. **Tri-state accumulating warning algebra.** `-Wall` (bulk-enable) then `-Wno-x` (mask) then
   `-Werror=y` (promote) then `-Wwarn=z` (demote): a *fold* over a set, not a flag with a value.
3. **Nested passthrough sub-languages.** `-optl-Wl,-z,now` = GHC→ld→ld-comma-list, ≥2 levels deep,
   beyond the single `-Wl,` the enum hints at.
4. **Mode flags reshape the grammar.** `-e`, `--show-iface`, `--make`, `--interactive`,
   `--frontend` change what positionals *mean* and which flags are legal — a conditional grammar.
5. **Order is globally significant.** GHC explicitly documents "options are evaluated left-to-right";
   `-XFoo` placed after a `{-# LANGUAGE #-}`-affected file, `-package`/`-hide-package` shadowing,
   `-D/-U` interleaving. A canonicalizer that sorts *anything* outside a known-commutative class is
   wrong.
6. **`-pgm*` redefines the meaning of all `-opt*`.** The flags are coupled: `-pgmc clang` changes
   how `-optc` blobs are interpreted. Flag meaning depends on a *prior* flag.

### 1.6 Caching hazards

- **`-frandom-seed=<n>`** — explicit nondeterminism knob (used for symbol/uniques). Must be
  captured or normalized; if absent GHC's unique generation is deterministic-per-invocation but
  parallel `--make` (`-j`) can reorder.
- **`-j[N]` parallelism** — interface/output write *order* and some `-ddump-*` interleaving become
  nondeterministic; dump output is impure under `-j`.
- **Absolute path leakage** — `.hi` files and `-prof` cost-centre stacks embed source paths;
  `-g`/DWARF debug embeds absolute build dirs. `-fbyte-code`/object files may embed `-tmpdir`.
- **Timestamps** — recompilation avoidance uses mtime; `.hi` files store source hashes (good) but
  also build metadata. `__DATE__`/`__TIME__` reachable via `-optP` to CPP.
- **`-tmpdir` / `$TMPDIR`** — temp file names leak into `-ddump-to-file` outputs and sometimes
  panic messages.
- **`GHC_ENVIRONMENT` / `GHC_PACKAGE_PATH`** — ambient env silently changes the package set; the
  cache key must include them or be marked impure.
- **`-dynamic` vs `-static` + RTS linking** — absolute rpath embedding.

---

## 2. cabal (cabal-install)

### 2.1 Invocation shape

A **real two-token-deep subcommand tree**, well covered by `Grammar.subcommands` at the top level:

```
cabal <command> [targets...] [flags]
  build|configure|repl|run|test|bench|haddock|exec|clean|freeze|gen-bounds|outdated
  install|list|list-bin|info|get|init|check|sdist|upload|report
  update|user-config|v1-* (legacy aliases) | v2-* (explicit aliases)
```

Depth is mostly 2. The deepest *real* nesting is `cabal user-config {init,update,diff}` and the
`v1-`/`v2-` alias prefixing (`cabal v2-build` ≡ `cabal build`) — an **alias namespace** that
`subcommands` can model as duplicate keys but that implies a canonical-name rewrite step.

**Target positionals are a sub-language**, not file paths: `cabal build pkgname`,
`cabal build lib:foo`, `cabal build exe:bar`, `cabal build all`, `cabal build ./dir`,
`cabal build pkg:foo:test:spec`. The positional is `[pkg][:ctype:cname]` — a **structured
target selector**, not `PathIn`. `Positional` policy is too flat.

### 2.2 Spelling forms

| Form | cabal examples |
|------|----------------|
| Bare | `--enable-tests`, `--disable-optimization`, `-v`, `--only-dependencies` |
| Equals/Separated | `--with-compiler=ghc-9.8`, `-w ghc`, `--builddir=dist-x`, `--prefix=/opt` |
| Attached short | `-v2`, `-j4`, `-fdev` (`-f` = cabal *package flag*, not GHC's!) |
| Passthrough | `--ghc-options="-O2 -Wall"`, `--ghc-option=-O2`, `--haddock-options=...`, `--test-options=...`, `--constraint="text >=2"` |
| Response file | `cabal @file` / `cabal build @args` (GNU response files, inherited from the option parser). |
| `--` | `cabal run exe -- progargs` and `cabal exec -- prog args`. |
| count | `-v0..-v3` / `--verbose=3`. |

**Forms the enum lacks for cabal:**

1. **`--ghc-options="..."` is a *quoted opaque GHC argv* inside one flag value.** This is
   `Passthrough` but with **shell-word-split** semantics and the *inner grammar is all of §1*. The
   canonicalizer cannot produce a stable key without splitting the string and canonicalizing it as
   GHC argv. `Passthrough(sep=' ')` is necessary but the payload type is "another Grammar," which the
   enum can't express.
2. **`-f<flag>` / `-f-<flag>` / `-fflag1 +flag2 -flag3`** — cabal Cabal-flags: a *signed set* with
   `+`/`-` polarity, given either as repeated `-f` or as one `--flags="+a -b"` string. Polarity-set
   again (no existing policy).
3. **Same flag legal at multiple levels** — `--ghc-options` accepted by `configure`, `build`,
   `repl`, AND in `cabal.project`/`cabal.project.local` (a *config file*, outside argv). Effective
   value = merge of file + argv; canonicalization that only sees argv is incomplete.

### 2.3 Multiplicity & Policy

- `--constraint="..."` — **repeatable, intersected** (cabal docs: "specified constraints are
  intersected"). Not last-wins, not a sorted set — a *semantic merge*. Needs a new
  `AccumulateMerge` policy.
- `--ghc-options` — **repeatable, concatenated in order** then forwarded. `DedupOrdered` would be
  wrong (must not dedup; `-Wall -Wall` differs only cosmetically but reordering breaks).
- `-f`/`--flags` package flags — signed-set, last-wins-per-flag.
- `--with-compiler`, `--builddir`, `--prefix` — `Once`.
- `-j` — `Once`, but value `$ncpus` is **machine-dependent** (hazard).

### 2.4 Path-bearing slots

- `PathOut`/`PathOutDir`: `--builddir=DIR` (= `dist-newstyle`), `--prefix`, `--bindir`, `--libdir`,
  `--datadir`, `--docdir` (**a whole family of install output dirs**).
- `PathIn`: `--project-file=FILE`, `--config-file=FILE`, `cabal.project*` (implicit), `--with-hc-pkg`,
  `--with-compiler=PATH` (a `ToolBinary`).
- Sneaky: `--package-db=PATH` (multiple, ordered, includes the special tokens `global`/`user`/`clear`
  — a path list with **reserved non-path keywords**), `--extra-include-dirs`, `--extra-lib-dirs`,
  `--extra-prog-path`. These are **`Include`/`Lib` dirs forwarded down to GHC**.
- Env: `CABAL_DIR`, `CABAL_CONFIG`, `CABAL_BUILDDIR` — env-valued config/output paths.

`Kind` gap: `--package-db` mixes `PathIn` with literal sentinels (`clear`, `global`, `user`) —
needs `PathOrSentinel`.

### 2.5 Model-breakers

1. **`--ghc-options` / `--haddock-options` / `--test-options` / `--benchmark-options` /
   `--program-options`** all embed *full foreign argv strings*. Canonicalizing cabal **requires
   canonicalizing GHC** (and haddock, and the test exe). Recursive grammar, but across *tool
   boundaries* and *through a shell-quoted string*, not via `subcommands`.
2. **cabal.project + argv merge.** The effective command is `argv ⊕ project files ⊕ ~/.cabal/config`.
   No argv-only canonicalizer is correct.
3. **`per-package` stanzas** in `cabal.project` apply flags *conditionally per package* — a
   conditional grammar keyed on the build graph, far beyond `Grammar`.

### 2.6 Caching hazards

- **`dist-newstyle/` plan.json embeds absolute paths and timestamps.**
- **`-j` defaults to `$ncpus`** — machine-dependent argv if not pinned.
- **`cabal.project.local` / `cabal.project.freeze`** auto-generated, drift.
- **Solver nondeterminism** historically; index-state (`--index-state`) pins it but the *default*
  is "now" — a clock read. `--index-state` MUST be captured.
- **`__DATE__`/`__TIME__`** reachable via forwarded `--ghc-options -optP`.

---

## 3. stack

### 3.1 Invocation shape

The deepest subcommand tree of the three. `Grammar.subcommands` covers the top level; depth reaches
**3**:

```
stack build|test|bench|haddock|install|run|exec|ghci|repl|runghc|eval|script|...
stack ls {snapshots,dependencies,stack-colors,tools}        # depth 2
stack ls snapshots {local,remote}                           # depth 3
stack hpc report [tixfiles...]                               # depth 2
stack config {set,env} ...                                  # depth 2; `set` has resolver/system-ghc subkeys
stack config set {resolver,system-ghc,install-ghc} VALUE    # depth 3
stack docker {pull,reset,cleanup,...}                       # depth 2
stack templates                                             # leaf
stack ide {packages,targets}                                # depth 2
```

**Deepest nesting cited:** `stack config set resolver lts-22.0` and `stack ls snapshots remote` —
**depth 3**. `Grammar.subcommands` (recursive map) covers this cleanly; this is the one place the
recursive model *shines*. `stack exec -- cmd args` and `stack ghci` then drop into foreign grammars.

### 3.2 Spelling forms

| Form | stack examples |
|------|----------------|
| Bare | `--fast`, `--haddock`, `--test`, `--no-run-tests`, `--system-ghc`, `--pedantic` |
| Equals/Separated | `--resolver=lts-22.0`, `--compiler=ghc-9.8`, `--stack-yaml=x.yaml`, `--work-dir=.w` |
| Passthrough | `--ghc-options="..."`, `--ghc-options-once`, `--cabal-verbosity`, `--rts-options` |
| `--`/RTS | `stack exec foo -- args`; **`+RTS ... -RTS`** GHC-runtime block (a *bracketed* sub-argv). |
| Negation pairs | `--[no-]run-tests`, `--[no-]run-benchmarks`, `--[no-]rerun-tests`, `--[no-]system-ghc` — **stack canonicalizes negation as `--no-` prefix** for nearly every boolean flag. |
| Response file | stack uses optparse-applicative which supports `@file` only if enabled (mostly not) — argfiles are weaker here. |

**Forms the enum lacks for stack:**

1. **`--[no-]X` boolean-pair convention applied universally.** Every flag has a `--no-` twin; the
   pair is *one logical tri-state knob*. The `Spelling` enum models neither the pairing nor the
   "`--no-` is the canonical off-spelling."
2. **`+RTS ... -RTS` bracketed block** — a delimited region (not key=value, not `--`-terminated)
   that nests a *whole other argv* (the GHC RTS option language: `-N4 -H1g -A64m`). It can appear
   multiple times and *concatenate*. No `Spelling` variant is a *bracket pair*.
3. **`--flag pkg:flagname` / `--flag pkg:-flagname`** — cabal-flag setting *namespaced by package*
   with `+`/`-`/bare polarity (`--flag foo:+dev`, `--flag foo:*:release`). Triple-structured value.

### 3.3 Multiplicity & Policy

- `--flag pkg:flag` — repeatable, **keyed by (package, flag)**, last-wins-per-key, polarity. New
  policy (keyed-polarity-set), shared with GHC's `-X`/`-f`.
- `--ghc-options` — ordered concatenate (forwarded). `--ghc-options-once` exists *specifically*
  because plain `--ghc-options` would otherwise apply to *every* package — i.e. **the same spelling
  has two accumulation semantics depending on the `-once` suffix.**
- `--extra-include-dirs` / `--extra-lib-dirs` — repeatable `DedupOrdered`.
- `--resolver`, `--compiler`, `--stack-yaml` — `Once`.
- `+RTS` blocks — ordered concatenate, never dedup.

### 3.4 Path-bearing slots

- `PathIn`: `--stack-yaml=FILE`, `--resolver` may be a **URL or local path** (`./snapshot.yaml`),
  positional `.hs` for `stack script`/`runghc`, `stack hpc report TIX...` (tix file paths).
- `PathOutDir`: `--work-dir=DIR` (`.stack-work`), `--local-bin-path=DIR` (install target).
- `ToolBinary`: `--compiler=PATH`, `--with-gcc`, `--with-hpack`.
- Sneaky: `--resolver` accepting URL/commit/path/snapshot-name in one slot (`PathOrSentinel`-style);
  `STACK_ROOT`, `STACK_YAML` env paths; `--extra-path=DIR` prepended to `PATH` for sub-processes.

### 3.5 Model-breakers

1. **`+RTS/-RTS` bracketed sub-argv** — not expressible as any `Spelling`; needs a delimited-region
   primitive nesting another grammar.
2. **`--ghc-options` vs `--ghc-options-once`** — flag identity carries accumulation semantics.
3. **Everything reduces to `stack.yaml` + resolver + argv merge** — like cabal, the effective
   command is config-file-dependent; resolver pins a *whole package snapshot* by name, so the cache
   key must hash the *resolved snapshot*, not the string `lts-22.0`.
4. **`stack exec`/`stack ghci`/`stack runghc`** wrap arbitrary foreign argv after `--`.

### 3.6 Caching hazards

- **Resolver names (`lts-22.0`, `nightly-2026-05-01`) resolve to mutable snapshot contents** until
  pinned via lock file (`stack.yaml.lock`). Must hash the lock, not the name.
- **`.stack-work/` embeds absolute paths**; `--local-bin-path` install leaks build host paths.
- **`-j`/`--jobs` defaults to ncpus** — machine-dependent.
- **`+RTS -N -RTS`** parallelism affects build worker ordering → dump/log ordering nondeterminism.
- **`STACK_ROOT` / `STACK_YAML`** ambient env silently changes resolution.
- Forwarded `--ghc-options` carries every GHC hazard from §1.6.

---

## Cross-cutting summary (what the model must grow)

- **Open prefix-family productions** (`-f`/`-W`/`-X`/`-opt`/`-pgm`/`--no-`/`--flag pkg:`): flags are
  not an enumerable list; the grammar needs *prefix matchers with structured suffix sub-parsers*.
- **Polarity-keyed accumulation** policy: `(key, op)` fold, last-op-per-key, open keyspace — covers
  `-W*`, `-X*`, `-f*`, `-D/-U`, `--flag`, `--[no-]`.
- **Nested / cross-tool passthrough whose payload is itself a Grammar**: `--ghc-options="<ghc argv>"`,
  `-optl-Wl,<ld list>`, `+RTS <rts argv> -RTS`. `Passthrough(sep)` must carry a *sub-grammar*, and one
  level is not enough (≥3 deep: stack→cabal→ghc→ld).
- **Mode flags that reshape the grammar** (`ghc -c/--make/-e/--show-iface`): conditional grammars
  selected by a flag, not a bare token.
- **Config-file ⊕ argv merge**: cabal.project / stack.yaml / ~/.cabal/config / GHC_ENVIRONMENT mean
  an argv-only canonicalizer is *unsound*. Cache keys must include resolved config + lock files.
- **New Kinds**: `PathOutDir`, `ToolBinary` (content-hash gates whole key), `Framework`/`FrameworkPath`,
  `Undef`, `PathInList(sep)`, `EnvPath`, `PathOrSentinel`, `TargetSelector` (cabal `pkg:ctype:cname`).
- **Impurity to neutralize**: `-frandom-seed`, `-j=$ncpus`, mutable resolver names, `--index-state`
  default = now, absolute paths in `.hi`/`.stack-work`/`dist-newstyle`, `__DATE__/__TIME__` via CPP,
  parallel-build dump ordering.
