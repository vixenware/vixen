# Command study: the Rust tool group (cargo, rustc, rustup)

Reference document for the Vixen recursive tool grammar. Studied against rustc/cargo/rustup
1.95.0 (2026-04) on the local machine — all flag inventories below are extracted from the
actual `--help`, `-C help`, `-W help`, and `--help -v` output of the installed toolchain.

Model under test:

```
Grammar { flags, positionals, subcommands: Map<token, Grammar> }   // recursive
Flag { lead, spelling: Spelling, policy: Policy, kind: Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

---

## TOOL 1 — `rustc`

The compiler. This is where the grammar model is stressed hardest: rustc is a single flat
command (no subcommands) but a very rich flag language with **three nested sub-languages**
(`-C`, `-W/-A/-D/-F`, `-Z`, `--json`, `--print`, `--emit`) plus the `@path` argfile.

### 1. Invocation shape

- **No subcommands.** `rustc [OPTIONS] INPUT`. Recursive `Grammar.subcommands` is *unused*
  for rustc — a degenerate, single-node grammar. There is exactly one positional: the input
  `.rs` file (or `-` for stdin).
- Deepest nesting is therefore not in the subcommand tree but in the **flag value grammars**
  (see §2 passthrough): `-Cllvm-args=...` and `-Clink-args=...` hold *another* whitespace- or
  comma-separated argument vector that is itself a sub-grammar (LLVM's / the linker's).

### 2. Spelling forms actually used

| Form | rustc example | Notes |
|---|---|---|
| Bare | `-g`, `-O`, `-v`, `-V`, `--test` | `-g`==`-Cdebuginfo=2`, `-O`==`-Copt-level=3` (alias-to-other-flag) |
| Attached | `-Lnative=/x`, `-lstatic=foo`, `-Cdebuginfo=2` (no space after `-C`) | `-C`, `-L`, `-l`, `-A/-W/-D/-F` all accept glued value |
| Separated | `-o out.o`, `-L /x`, `-l m`, `--extern serde libserde.rlib` | most long opts |
| Equals | `--edition=2021`, `--target=x86_64-...`, `--crate-name=foo`, `--remap-path-prefix=/a=/b` | |
| Equals-inside-value | `--extern serde=/p/libserde.rlib`, `--cfg feature="x"`, `-Cmetadata=abc`, `--emit obj=out.o` | value itself contains `=` — **two-level equals** |
| Passthrough | `-Cllvm-args=-x -y`, `-Clink-args=-Wl,-z,now`, `-Clink-arg=...`, `-Cpasses=...`, `-Cremark=all` | space-separated *inside one value*, NOT a `,` like `-Wl,` |
| Comma-list inside value | `--crate-type=bin,lib`, `--emit=obj,metadata`, `-Clinker-features=+lld,-x`, `-Ctarget-feature=+avx2,-sse`, `--remap-path-scope=macro,debuginfo` | |
| `--` end-of-flags | not meaningfully used by rustc (single positional) | |
| `@path` argfile | `rustc @args.txt` — read newline-separated options from a file | **NOT in Spelling enum** |
| count flags | `-v`/`-vv` (verbose); not a true 0..n counter like cargo `-vvv` | |

**Forms the Spelling enum lacks:**
- **`@path` response/argfile** (`@args.txt`). Newline-separated opts read from a file. The file
  *is itself a path-bearing slot* and its **contents change the cache key**. No Spelling/Kind
  models "the argument vector is partly stored in a file."
- **Two-level Equals**: `--extern NAME=PATH`, `--emit TYPE=FILE`, `--cfg NAME="VALUE"`,
  `--remap-path-prefix FROM=TO`. The `Equals` spelling treats `=` as the flag/value separator,
  but here the *value* is itself a `key=value` pair (and one side is a path). Needs a structured
  value, not a `Str`.
- **Passthrough with a *space* separator stored in a single token** (`-Cllvm-args=-x -y`). The
  model's `Passthrough(sep)` assumes a separator char like `,` (e.g. `-Wl,a,b`); here the nested
  vector is space-joined inside one shell token.

### 3. Multiplicity & order semantics → Policy needs

| Flag class | Real flags | Policy needed | Gap |
|---|---|---|---|
| Last-wins scalar | `-o`, `--out-dir`, `--crate-name`, `--edition`, `--target`, `--error-format` | `Once` | ok |
| Grouped/aliased opt-level | `-O` vs `-Copt-level=N`; `-g` vs `-Cdebuginfo=N` | `Group` — **but the alias lives in a *different flag* (`-O`) and inside a sub-language (`-C`)**. Last-wins must span `-O`, `-Cdebuginfo`, AND `-Copt-level`. | **last-wins-across-aliases-and-sublanguages** — Group is keyed by name, can't see that `-O` and `-Copt-level` collide |
| Lint levels | `-W/-A/-D/-F/--force-warn/--cap-lints <lint>` | accumulate, **last-level-wins per lint name, with priority** (`forbid` can't be downgraded; `--cap-lints` clamps all) | **conditional/priority dedup** — not Once, not DedupUnordered. `-Wunused -Aunused` → `unused` ends *allow*; but `-Funused -Aunused` → still *forbid*. Order + level-priority both matter. No Policy expresses this. |
| Search paths | `-L [KIND=]PATH` | `DedupOrdered` (order is link-search-semantic) | ok-ish, but KIND prefix (`-Ldependency=`, `-Lnative=`) means dedup key is `(kind,path)` |
| Native libs | `-l [KIND[:MODIFIERS]=]NAME[:RENAME]` | `DedupOrdered` (link order matters) | dedup key is the whole structured spec, not the bare name; `-lstatic=a` ≠ `-ldylib=a` |
| cfg | `--cfg name`, `--cfg 'feature="x"'` | `DedupUnordered` (a set) | values contain quotes/`=`; canonical form must normalize `feature="x"` |
| extern | `--extern NAME=PATH` | `DedupUnordered` keyed by NAME, **last-wins per NAME** | hybrid: set over NAME but last-wins on PATH |
| `-C` codegen | `-Ckey=val` | each key is independently `Once` (last-wins) | the *namespace* `-C` holds dozens of independent Once-flags; model has no "flag whose key is part of the value" |
| `--emit`, `--print` | comma list, repeatable | `DedupUnordered` over the type tokens | `--emit a --emit b` == `--emit a,b` |
| verbose | `-v`, `-vv` | count-ish | low-cardinality, doesn't affect output bytes → should be **dropped from cache key** |

### 4. Path-bearing slots

PathOut: `-o FILE`, `--out-dir DIR` (a PathOut **directory**), `--emit TYPE=FILE`,
`-Cprofile-generate=DIR`, `-Cincremental=DIR`.
PathIn: `INPUT.rs`, `--extern NAME=PATH` (rlib/rmeta), `-Cprofile-use=foo.profdata`,
`@argfile`, `--sysroot PATH`, `--target /path/to/custom-target.json` (target spec is a *file* when
it ends in `.json`!).
Include-like search dirs: `-L [KIND=]PATH`.
Lib: `-l [KIND:MODIFIERS=]NAME[:RENAME]`.

**Sneaky:**
- `--target` is normally a tuple string (`x86_64-unknown-linux-gnu`) **but is a JSON file path**
  when custom. Same flag, two Kinds depending on value. Model can't switch Kind on value.
- `--out-dir` is a PathOut **directory**, not a file — `PathOut` has no `dir?` discriminator the
  way `PathIn(dir?)` does.
- `--extern serde=PATH` and `--emit obj=PATH` — the path is the *RHS of an embedded `=`*.
- `-Clinker=cc` and `-Car=...` name **executables found on PATH** — path-bearing but unrooted.
- `@argfile` — the file contents are arguments; must be read, hashed, and ideally inlined before
  canonicalization.

**New Kind needed:** `PathOutDir`; `TargetSpec` (tuple-or-json-file); `ArgFile` (read+inline);
`ExternSpec`/`EmitSpec` (name=path structured); `Exe` (PATH-resolved tool).

### 5. Model-breakers (rustc)

1. **Lint priority resolution** (`-D/-A/-W/-F/--cap-lints/--force-warn`): canonical level for a
   lint depends on *all* occurrences AND a fixed priority lattice (forbid > others; cap clamps).
   Not Once, not any Dedup. A genuine reduction function, not a Policy.
2. **Cross-flag aliasing**: `-O` ≡ `-Copt-level=3`, `-g` ≡ `-Cdebuginfo=2`. To canonicalize you
   must *expand aliases into the `-C` namespace* before last-wins. The grammar has no alias edges.
3. **`-C`/`-Z` as open namespaces**: `-Ckey=value` where `key` is one of ~60 keys, each with its
   own value grammar (bool `yes/no`, enum, int, space-list, comma-list, path). One Flag node
   can't hold a sub-map of typed keys.
4. **Value-dependent Kind**: `--target` (string vs json file). Kind is not statically determinable.
5. **Sub-languages 3 levels deep**: `rustc -Clink-args=-Wl,-rpath,/x -Clld` → rustc value →
   linker argv → `-Wl,` → ld argv. `Passthrough(sep)` is single-level.
6. **`--json=diagnostic-short,artifacts`**: a flag whose value is a comma-set that reconfigures
   the *meaning/format of other output*, affecting nothing in the artifact — pure-vs-impure split
   inside one flag.

### 6. Caching hazards (rustc)

- **`-Cmetadata` / `-Cextra-filename`**: deliberately perturb symbol hashes & output filenames.
  Must be part of the key (they change output) but are themselves often derived from a hash.
- **`-Cincremental=DIR`**: incremental dir contents are nondeterministic across runs; presence
  makes the build **impure** unless the dir is excluded and reproducibility is otherwise asserted.
- **`-Cprofile-generate` / instrument-coverage**: emit profile data keyed to absolute paths.
- **Absolute path leakage into debuginfo & panic messages** — the canonical defense is
  `--remap-path-prefix=/abs/=./` and `--remap-path-scope`. The canonicalizer should *inject/verify*
  remap to neutralize CWD/$HOME leakage.
- **`-Clink-args` embedding tmp/`$RANDOM` linker scripts**, `-Csave-temps` writing tmp.
- **`@argfile` mutation**: file changes silently change argv → must hash file content into key.
- **`-Ctarget-cpu=native`**: result depends on the *build host CPU* → impure / host-keyed.
- **Diagnostic/verbose flags** (`--error-format`, `--json`, `-v`, `--diagnostic-width`, `--color`):
  affect stderr only, not artifacts → must be stripped from the artifact cache key.
- **`SOURCE_DATE_EPOCH`** env and embedded timestamps in some emit types.

---

## TOOL 2 — `cargo`

The package manager / build orchestrator. ~40 builtin subcommands + arbitrary `cargo-<x>`
external subcommands (the local box has insta, nextest, llvm-cov, hakari, miri, machete…).

### 1. Invocation shape

- **Two levels of builtin nesting**, plus a third via external subcommands and `--`:
  - depth 1: `cargo build`, `cargo test`, `cargo run`
  - depth 2: `cargo owner add`, `cargo set-version` (via cargo-edit), and notably:
    **`cargo +toolchain build`** — the `+toolchain` token is a *pseudo-flag positional* consumed
    before subcommand dispatch.
  - depth 3 (effective): `cargo run --bin x -- --flag` and especially
    **`cargo test -- --test-threads=1 --nocapture`** → the `--` hands a whole new argv to the
    *test harness* (libtest), which is **a different grammar entirely**. Deepest real nesting:
    `cargo test [cargo opts] -- [libtest opts]` then libtest itself has `--`? no — but
    `cargo run -- prog-args` where prog-args may again contain `--`.
- Recursive `Grammar.subcommands` **covers the builtin tree** but does **not** cover:
  - `+toolchain` (a leading channel selector, not a subcommand, not a flag).
  - external `cargo-foo` subcommands (open set, unknown grammar — must fall through to an opaque
    passthrough grammar).
  - the post-`--` argv belonging to a *foreign* grammar (libtest / the user binary).

### 2. Spelling forms actually used

| Form | cargo example |
|---|---|
| Bare | `--release`/`-r`, `--workspace`, `--all-features`, `--locked`, `--offline`, `--frozen`, `--no-default-features` |
| count | `-v`, `-vv` (`--verbose...` is a real 0..n counter; `-vv` triggers build.rs output) |
| Separated | `-p foo`, `--bin x`, `--example y`, `-j 4`, `--target-dir dir`, `--manifest-path Cargo.toml` |
| Equals | `--message-format=json`, `--color=always`, `--profile=release` |
| Comma/space list | `-F serde,tokio` or `-F "serde tokio"` (`--features` accepts **both** comma and space!) |
| Optional-value | `--bin` (all) vs `--bin NAME`; `-p` vs `-p SPEC`; `--target` vs `--target TRIPLE` — flags whose argument is optional (`[<NAME>]`) |
| `--config K=V` or `--config path.toml` | `--config build.jobs=2` **or** `--config conf.toml` — value is *either inline toml or a file path* |
| `-Z` namespace | `-Zunstable-options`, `-Zbuild-std=core` (nightly sub-language) |
| `+toolchain` | `cargo +nightly build` — leading channel selector |
| `--` passthrough | `cargo run -- arg`, `cargo test -- --nocapture` |
| `-C DIRECTORY` | change dir before anything (nightly) — a Bare-lead PathIn dir |

**Forms the Spelling enum lacks:**
- **`+toolchain` leading selector** — neither flag nor subcommand; positionally first, optional.
- **Optional-argument flags** (`--bin [NAME]`, `-p [SPEC]`): `--bin x` vs bare `--bin` mean
  different things. Spelling::Separated assumes the value is mandatory.
- **`--features` accepts comma OR space inside one token** — two separators for one flag.
- **`--config` value is inline-DSL-or-path** (same value-dependent-Kind problem as rustc `--target`).

### 3. Multiplicity & order semantics → Policy needs

| Flag class | Flags | Policy | Gap |
|---|---|---|---|
| Bool last-wins | `--release`, `--locked` | `Once` | ok |
| `--release` vs `--profile` | `-r` is alias for `--profile=release`; conflicts with `--profile=dev` | `Group` | cross-flag alias (same as rustc `-O`) |
| Repeatable selectors | `-p`/`--package`, `--bin`, `--example`, `--test`, `--exclude` | **DedupUnordered set** (`-p a -p b`) | ok |
| Features | `-F`/`--features` (repeatable, comma/space) | DedupUnordered (union of a set) | must split on both separators then dedup |
| `--config` overrides | `--config k=v` repeatable | **last-wins per key**, like rustc `-C` | namespaced last-wins |
| Test/bin name filter | `[TESTNAME]` positional | `Positional` | but it's a *filter substring*, order-irrelevant |
| `-j/--jobs` | scalar | `Once` | value `0`/negative special-cased |

### 4. Path-bearing slots

- `--manifest-path Cargo.toml` (PathIn), `--target-dir DIR` (PathOutDir),
  `--artifact-dir PATH` (PathOutDir), `--out-dir` (deprecated), `-C DIRECTORY` (PathIn dir,
  changes resolution of everything after), `--config conf.toml` (PathIn when not `k=v`),
  `--target /custom.json` (TargetSpec, same as rustc).
- **Sneaky:** the *real* path universe is implicit — `Cargo.toml`, `Cargo.lock`, `src/`,
  `build.rs`, the entire crate-graph source, `.cargo/config.toml`, `target/` — none appear in
  argv. A faithful cache key must hash the **resolved source closure**, which cargo computes, not
  the argv. `cargo` is the case where *argv alone is radically insufficient* for a cache key.
- `--features` values can name features defined in `Cargo.toml` (no path, but affects closure).

### 5. Model-breakers (cargo)

1. **`+toolchain`** — out-of-band selector that rebinds which `rustc`/`cargo` runs.
2. **External subcommands** — open set; `cargo nextest run` dispatches to an *unknown* grammar
   that cargo never sees. `subcommands: Map` is closed.
3. **The `--` boundary hands argv to a foreign grammar** (libtest, criterion, the user's `main`).
   These must be parsed by a *different* Grammar selected at runtime by which binary runs.
4. **Argv is not the cache input** — the dependency/source closure is. The grammar canonicalizes
   argv but the cache key genuinely depends on resolved `Cargo.lock` + sources + env + RUSTFLAGS.
5. **`RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS`/`build.rustflags` env+config inject flags** into the
   downstream rustc grammar from *outside argv*.

### 6. Caching hazards (cargo)

- **`build.rs` runs arbitrary code** → can read clock, env, network, `$RANDOM`, emit
  `cargo:rerun-if-*` — fundamentally impure; must be treated as a black box keyed on declared
  inputs only.
- **`RUSTFLAGS` / `CARGO_*` env** silently alter the build; must be folded into the key.
- **`--timings` / `--unit-graph`** emit reports with **timestamps** (impure output, but doesn't
  affect artifacts → strip from artifact key).
- **Parallel job order** (`-j`) → nondeterministic stderr/diagnostic ordering; the *artifacts*
  are deterministic, the *logs* are not.
- **Absolute paths from `target-dir`/registry cache** leak into rmeta — needs remap (cargo passes
  `--remap-path-prefix` only under `-Ztrim-paths`/`trim-paths` profile).
- **`--offline`/`--frozen`/`--locked`** change network behavior but should not change the key if
  the lockfile is identical — they are *purity assertions*, not key inputs.
- **`net.git-fetch-with-cli`, registry mtimes**, `~/.cargo` global config bleed.

---

## TOOL 3 — `rustup`

The toolchain multiplexer. Pure orchestration — almost no path/cache semantics, but the
**deepest genuine subcommand tree** of the three.

### 1. Invocation shape — deepest nesting of the group

- depth 2 is the norm; depth 3 exists:
  - `rustup toolchain install <name>` / `link` / `list` / `uninstall`
  - `rustup target add <triple>` / `remove` / `list`
  - `rustup component add <name>` / `remove` / `list`
  - `rustup override set <toolchain>` / `unset` / `list`
  - `rustup self update` / `uninstall` / `upgrade-data`
  - `rustup show active-toolchain` / `home` / `profile`  ← **depth 3** (`rustup show profile`)
- `rustup run <toolchain> <command>...` — depth 2, then **`<command>...` is an arbitrary foreign
  argv** (any program), same foreign-grammar problem as cargo's `--`.
- `+toolchain` leading selector also works: `rustup +nightly show`.
- Recursive `Grammar.subcommands` **covers this cleanly** — rustup is the best fit for the model.
  The only escape hatches are `rustup run <tc> <cmd>...` and `+toolchain`.

### 2. Spelling forms

| Form | example |
|---|---|
| Bare | `-v`/`--verbose`, `-q`/`--quiet`, `--force`, `--no-self-update`, `--path` (rustup doc), `--book`/`--reference`/`--nomicon` (doc topics as bare flags) |
| Separated | `--toolchain stable` (on `which`/`doc`), `--install` |
| `+toolchain` | `rustup +nightly component list` |
| Variadic positional | `rustup update <tc>...`, `rustup run <tc> <cmd>...` |
| Channel-name grammar | `stable`, `nightly-2014-12-18`, `1.8.0`, `nightly-x86_64-unknown-linux-gnu` — `<channel>[-<date>][-<host>]` structured positional |

No `@argfile`, no equals-values, no count beyond `-v`. Simplest of the three.

**Forms the Spelling enum lacks:** `+toolchain` (again); the **structured channel-name positional**
(`channel-date-host`) is a `Positional` whose *internal structure* matters but the model treats
positionals as opaque `Str`.

### 3. Multiplicity & order

- `rustup update tc1 tc2` — variadic, set-like (`DedupUnordered`).
- `rustup doc --book --reference` — the doc-topic bools are **mutually exclusive** (`Group`).
- Most flags `Once`.
- Order rarely matters; this tool is the friendliest to the existing Policy set.

### 4. Path-bearing slots

- `rustup toolchain link <name> <path>` — a PathIn **dir** (custom toolchain root).
- `rustup which <cmd>` — resolves to a path but takes a command name (Exe).
- `rustup doc --path` — prints a path (PathOut-ish).
- `rustup override set <tc>` — implicitly keyed to **CWD** (the override is recorded against the
  current directory, not an argv path) → a hidden path slot.
- Otherwise paths are in `$RUSTUP_HOME`, `$CARGO_HOME` env, not argv.

### 5. Model-breakers (rustup)

1. **`rustup run <tc> <cmd>...`** — foreign argv (same class as cargo `--`).
2. **`+toolchain`** leading selector.
3. **CWD-implicit state** (`override set` binds to cwd) — argv doesn't carry the path.
4. **Structured channel name** as a positional whose substructure (`-date-host`) is semantic.

### 6. Caching hazards (rustup)

- rustup is **stateful, not a pure compiler** — installs/uninstalls mutate global `$RUSTUP_HOME`.
  Caching is largely N/A; the relevant hazard is that **which toolchain `+x`/override/default
  resolves to is ambient state** (date-stamped nightlies!). `rustup run nightly cargo build`
  resolves `nightly` to *whatever is installed today* — the effective toolchain version must be
  resolved and folded into any downstream cache key, because `nightly` is not a stable identity.
- `rustup update` reaches the network nondeterministically.

---

## Cross-tool synthesis — what the model must grow

**New Spellings:**
- `ToolchainSelector` (`+stable` leading token) — shared by cargo & rustup.
- `ArgFile` (`@path`) — rustc; reads & inlines argv from a file (content-hashed).
- `OptionalValue` (`--bin [NAME]`) — presence-vs-value distinction.
- `EmbeddedKV` (two-level: `--extern NAME=PATH`, `--config K=V`, `--emit T=F`,
  `--remap-path-prefix FROM=TO`).
- `MultiSep` (`--features` accepts both `,` and ` `).

**New Policies:**
- `LintLattice` — level resolution with priority (forbid pins; cap-lints clamps) over all
  occurrences. Neither last-wins nor dedup.
- `NamespacedOnce` — `-Ckey=val`/`--config k=v`: independent last-wins per embedded key.
- `AliasGroup` — last-wins across flags that are aliases of one canonical key (`-O`↔`-Copt-level`,
  `-g`↔`-Cdebuginfo`, `-r`↔`--profile=release`).
- `KeyedLastWins` — `--extern NAME=PATH`: set over NAME, last-wins over PATH.

**New Kinds:**
- `PathOutDir` (`--out-dir`, `--target-dir`, `--artifact-dir`).
- `TargetSpec` / `ValueDependentPath` (`--target`, `--config`: tuple-or-file).
- `Exe` (PATH-resolved tool: `-Clinker`, `rustup which`).
- `ArgFileRef`, `ExternRef`, `EmitRef` (structured name=path).
- `ProfileData` (`-Cprofile-use=*.profdata`).

**The two structural model-breakers** (cannot be patched with new enum variants):
1. **Foreign/open grammars at the `--`, `cargo-<ext>`, and `rustup run` boundaries** — the
   subcommand map is closed and the post-boundary argv belongs to a *different* tool whose grammar
   must be selected dynamically (often unknowable statically).
2. **Argv is not the cache key for cargo** — the real key is the resolved source+lock+env closure
   that cargo computes internally; the grammar can canonicalize argv but cannot see the inputs
   that actually determine the output.
