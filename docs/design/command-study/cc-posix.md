# Command Study: C/C++ POSIX Tool Group

Tools: `cc`/`clang`/`gcc`, `ld`/`lld`/`gold`, `ar`, `ranlib`, `as`, `pkg-config`, `cmake`, `make`, `ninja`.

Model under test:

```
Grammar { flags: [Flag], positionals: [Positional], subcommands: Map<String,Grammar> }
Flag { lead, spelling: Spelling, policy: Policy, kind: Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

Each tool is analyzed against the 6 points: (1) invocation shape, (2) spelling forms,
(3) multiplicity/order, (4) path-bearing slots, (5) model-breakers, (6) caching hazards.

---

## 1. `cc` / `clang` / `gcc` (the C/C++ driver)

This is the hardest and most important tool in the group. The driver is itself a
multiplexer over preprocessor (cpp), compiler (cc1), assembler (as), and linker (ld/collect2).

### 1.1 Invocation shape
**Flat, no subcommands.** There is no subcommand tree at all: `clang [flags] file...`.
But the *mode* is selected by flags, not by a bare token:
- `-c` → compile to `.o` (stop before link)
- `-S` → compile to `.s` (stop before assemble)
- `-E` → preprocess only (stop before compile)
- `-fsyntax-only` → parse only
- no stop-flag → full pipeline through link

`Grammar.subcommands` does **not** apply — dispatch is by mutually-exclusive *flags*,
not by position. This already stresses the model: the "subcommand" is a `Group` of
flags, and the rest of the grammar (which positionals are inputs, whether `-o` is an
object or executable) is *conditioned on which group member is set*. See model-breakers.

### 1.2 Spelling forms actually used
Every spelling in the enum appears, plus several it lacks:

| Form | Examples |
|------|----------|
| Bare | `-c -S -E -g -O -pipe -shared -static -pthread -v -w -Werror` |
| Attached | `-O2 -O3 -Os -Ofast -Og` ; `-DFOO` ; `-Ipath` ; `-Lpath` ; `-lm` ; `-Wall -Wextra -Wno-unused` ; `-fPIC -fno-omit-frame-pointer` ; `-march=` (also equals) ; `-g0/-g1/-g3` ; `-std=` |
| Separated | `-o out.o` ; `-include header.h` ; `-isystem dir` ; `-iquote dir` ; `-idirafter dir` ; `-MF dep.d` ; `-MT target` ; `-MQ target` ; `-Xlinker arg` ; `-Xclang arg` ; `-Xpreprocessor arg` ; `-Xassembler arg` ; `-x c` ; `-z now` ; `-u symbol` |
| Equals | `-std=c11 -std=gnu++20` ; `-march=native` ; `-mtune=` ; `-fvisibility=hidden` ; `-fstack-protector=strong` ; `--target=x86_64-linux-gnu` ; `-fsanitize=address,undefined` ; `-flto=thin` ; `--sysroot=` |
| Passthrough | `-Wl,--as-needed,-rpath,/x` (sep `,` → **ld** grammar) ; `-Wa,-mrelax-relocations` (→ **as**) ; `-Wp,-MD,file` (→ **cpp**) ; `-Xlinker --gc-sections` (single token → ld) |
| Response files | `@argfile` / `@/abs/path` — argfile contents are spliced in-place, recursively (`@` inside `@file`). **Not in Spelling enum.** |
| `--` end-of-flags | clang accepts `--` to terminate option parsing; subsequent tokens are inputs. **Not in Spelling enum.** |
| Count / accumulation | `-v` (driver verbosity; repeated raises it on some tools). Mostly accumulate-not-count here, but `-O` with no digit == `-O1`. |

**Forms the Spelling enum lacks:**
- **Response files (`@file`)** — a *splice* directive, recursive, value is a PathIn whose
  *contents* are more argv. Canonicalization must expand them (and their contents are
  cache-key-relevant). No spelling models "read argv from a file."
- **`--` end-of-options** sentinel.
- **Multi-target passthrough chains** — `-Wl,a,-Wl,b` and `-Xlinker a -Xlinker b`
  both feed *one* downstream grammar but interleaved with driver flags. Passthrough(sep)
  models one flag's payload; it doesn't model "all `-Wl,`/`-Xlinker`/`-z`/`-u`
  tokens concatenate, in source order, into a single ld argv."

### 1.3 Multiplicity & order semantics → Policy
| Flag class | Real behavior | Policy needed | Gap |
|---|---|---|---|
| `-O0/-O1/-O2/-O3/-Os/-Ofast/-Og` | last wins | `Group("opt")` | ok |
| `-g0/-g/-g3`, `-std=` | last wins | `Group` / `Once` | ok |
| `-o` | last wins (+lint dup) | `Once` | ok |
| `-D FOO` / `-DFOO=1` | **order matters vs `-U`**: `-DFOO -UFOO` ≠ `-UFOO -DFOO` | — | **DedupUnordered WRONG.** -D/-U form an *ordered macro program*. Model says `-D` is a sorted set; reality is a sequenced define/undef stream. |
| `-I` / `-isystem` / `-iquote` / `-L` | dedup-keep-first, **order is the search path** | `DedupOrdered` | ok, but `-I` and `-isystem` are *different ordered lists* that interleave by category, not by source position |
| `-l` | **NOT dedup-safe** | `Positional`-ish | **DedupOrdered WRONG for `-l`.** `-lfoo -lbar -lfoo` is sometimes required (circular static archives). Model claims `-l` is DedupOrdered; canonicalizing away the repeat breaks the link. |
| `-W…` / `-Wno-…` / `-Werror=…` | **accumulate with last-wins-by-name negation**: `-Wall -Wno-unused -Wunused` | — | **NO POLICY FITS.** Need "namespaced toggle accumulation": each warning name resolves to its last on/off/error mention; the canonical form is the resolved per-name state, not the raw list. Same for `-f…/-fno-…`, `-m…/-mno-…`. |
| `-include` | accumulate, ordered, dups meaningful | `DedupOrdered` (no dedup) | ok-ish |
| inputs (`.c .o .a`) | never reorder/dedup | `Positional` | ok |

### 1.4 Path-bearing slots
- PathIn(file): positional `.c/.cpp/.o/.a/.so` inputs; `-include h`; `@file`; `--sysroot=` (dir); `-isysroot`.
- PathIn(dir): `-I -L -isystem -iquote -idirafter -B` (compiler exec dir), `-F` (framework dir, Darwin).
- PathOut: `-o`; `-MF` (dep file); `--serialize-diagnostics file`; `-save-temps=` writes to cwd/dir.
- Include kind: `-I`, `-isystem`, `-iquote`, `-idirafter` — but these are *distinct search categories*, not one Include list.
- Lib kind: `-l` (name→resolved-path), `-L` (searchdir). The `-l` → actual `libfoo.a` resolution depends on `-L` order + `-static`/`-Bstatic`; the resolved file is what must be content-hashed, not the literal `-lfoo`.
- Define kind: `-D`/`-U`.
- **Sneaky paths:** `@response-file` (content-hashed), `-fprofile-use=path`, `-fprofile-generate=dir`, `-fdebug-prefix-map=OLD=NEW` (path *rewrite*, see hazards), `-ffile-prefix-map=`, `-frandom-seed=`, `--config file`, env-valued: `CPATH`, `C_INCLUDE_PATH`, `LIBRARY_PATH`, `SDKROOT`, `COMPILER_PATH`, `GCC_EXEC_PREFIX` all inject include/lib/exec dirs **invisibly** (not on argv at all).

**New Kind needed:**
- `PathMap` (an `OLD=NEW` path-rewrite pair, e.g. `-fdebug-prefix-map`): both sides are paths but neither is opened; they exist to *neutralize* absolute paths in output. Canonicalizer must understand it both as a value and as a determinism control.
- `ArgFile` (a PathIn whose contents are recursively re-parsed as argv).
- `LibName` (a `-l` name that resolves against an ordered search path to a concrete archive/shared object — distinct from PathIn because the literal text isn't a path).

### 1.5 Model-breakers
- **Conditional grammar by mode flag.** `-c`/`-S`/`-E`/`-fsyntax-only` change whether `-o` is an object/asm/preproc/exe and whether link flags (`-l`, `-Wl,`) are even legal. The grammar after the mode flag is *different*. Recursive `subcommands` can't express "dispatch on a flag that may appear anywhere in argv."
- **`-x lang` is a stateful mode switch over positionals.** `-x c foo.txt -x c++ bar.txt -x none baz.s` — `-x` sets the language for *subsequent* inputs until changed. The meaning of each positional depends on the most recent `-x`. No Policy models "flag mutates interpretation of following positionals."
- **`-Wl,`/`-Xlinker`/`-Wa,`/`-Xclang` are full sub-languages**, not a flat sep-split. `-Wl,--start-group,-la,-lb,--end-group` carries ld grouping with its own ordered semantics. Passthrough(sep) only splits; it doesn't recurse into ld's grammar (which itself has Separated/Attached forms).
- **`-D`/`-U` ordered interaction** (above): not a set.
- **`-W`/`-f`/`-m` namespaced last-wins toggles** (above): no policy.
- **Driver-vs-frontend ambiguity:** some spellings exist at two levels; `-O` to driver maps to multiple cc1 flags. The canonical *driver* argv and canonical *cc1* argv differ; cache key wants the lowered one but lint wants the driver one.

### 1.6 Caching hazards
- `-frandom-seed=N` — if absent, gcc derives a "random" seed from the output filename / PID; symbol-mangling of anonymous namespaces and `.gcda` naming depend on it. **Must pin or mark impure.**
- `__DATE__`/`__TIME__`/`__TIMESTAMP__` macros embed wall-clock into output. Flag `-Wdate-time` warns; presence in TU makes output time-dependent → impure unless `SOURCE_DATE_EPOCH` is set (gcc/clang honor it).
- **Absolute path leakage** into `DW_AT_comp_dir`, `__FILE__`, debug line tables — neutralized by `-fdebug-prefix-map`/`-ffile-prefix-map`/`-fmacro-prefix-map`. Canonicalizer must *require* these (or strip cwd) for a stable key.
- `-g` debug info embeds cwd + absolute source paths.
- Temp paths: `-save-temps`, internal `/tmp/ccXXXXXX.s`; `-pipe` removes them. The temp names are nondeterministic but normally not in output.
- `LIBRARY_PATH`/`CPATH`/`SDKROOT` env vars silently change include/lib resolution → must be folded into the cache key even though not on argv.
- `-march=native` resolves to the *build host's* CPU → non-portable, host-dependent canonical form. Should be marked host-impure or expanded to the concrete `-march=<arch>`.
- LTO (`-flto`) defers codegen to link and emits objects containing bitcode + a content-addressed but timestamped module hash.

---

## 2. `ld` / `lld` / `gold` (linkers)

Three implementations of one (GNU-bfd) CLI surface, plus lld's own dialects (`ld.lld`,
`ld64.lld` for Mach-O, `lld-link` for COFF, `wasm-ld`). gold is GNU-bfd-compatible.

### 2.1 Invocation shape
**Flat.** `ld [options] objfile...`. lld dispatches *implementation by argv[0]/`-flavor`*:
`ld.lld` (ELF) vs `ld64.lld` (Mach-O) vs `lld-link` (COFF, MSVC-style) vs `wasm-ld`.
`lld -flavor gnu` selects flavor — this is a **flavor switch that changes the entire
grammar** (GNU vs MSVC `/FLAG` spelling). Not subcommands; a top-level dialect selector.

### 2.2 Spelling forms
| Form | Examples |
|------|----------|
| Bare | `-shared -static -r -s -S -n -N --gc-sections --as-needed --no-as-needed --start-group --end-group -Bstatic -Bdynamic -Bsymbolic --eh-frame-hdr --build-id` |
| Attached | `-lm -lpthread` ; `-Lpath` ; `-O2` (linker opt level) ; `-znow`(also `-z now`) |
| Separated | `-o out` ; `-e main` (entry) ; `-T script.ld` ; `-u sym` ; `-z now` ; `-m elf_x86_64` ; `--dynamic-linker /lib/ld.so` ; `-rpath dir` ; `-soname name` ; `-plugin path` ; `-Map file` |
| Equals | `--hash-style=gnu` ; `--build-id=sha1` ; `-rpath=dir` ; `--sysroot=` ; `--version-script=file` (also sep) ; `--gc-sections` ; long opts accept both `--opt val` and `--opt=val` |
| Passthrough | `--start-group … --end-group` is itself a *bracketed sublanguage* over `-l`/object positionals; `-plugin-opt=` feeds the LTO plugin a nested option string |
| Response files | `@file` (GNU + lld). lld-link: `@rsp`. **Not in enum.** |
| Windows `/FLAG` | `lld-link /OUT:foo.exe /DEBUG /SUBSYSTEM:console /LIBPATH:dir foo.obj` — **entire MSVC dialect.** Spelling enum has nothing for `/FLAG:value`. |
| Repeated `--` | lld/ld treat lone `-` and `--` specially; GNU `ld` uses `--` rarely. |

### 2.3 Multiplicity & order
- `-l` and object positionals: **order is the entire semantics** (symbol resolution is single-pass left-to-right for archives). `Positional`. Dedup is *forbidden*.
- `--start-group/--end-group`: a *re-scan bracket* around a span of `-l`/objects → nested ordered region. No policy models "bracketed positional span."
- `-L`: `DedupOrdered` (search path).
- `-rpath`: accumulates, ordered; dups should dedup-keep-first.
- `-z keyword`: accumulate set of keywords (`now`, `relro`, `noexecstack`, `defs`). `DedupUnordered` over keywords — but `-z` is Separated so the *value* is the set member, not the flag.
- `-O` linker opt: `Group`/last-wins.
- `-u sym`: accumulate set.
- `--wrap=sym`, `--defsym a=b`: accumulate; `--defsym` is an ordered assignment program.

### 2.4 Path-bearing slots
- PathIn(file): object/archive positionals; `-T linkscript`; `--version-script`; `--dynamic-list`; `-plugin path`; `--retain-symbols-file`; `@file`.
- PathIn(dir): `-L`, `--sysroot`, `-rpath`/`-rpath-link` (dir, but **baked into output** — see hazards).
- PathOut: `-o`, `-Map mapfile`, `--emit-relocs`.
- Lib: `-l`, `-L`.
- **Sneaky:** `-rpath` is a dir that is *embedded into the binary* (DT_RPATH) — it's a PathIn used as literal output content; absolute → non-relocatable + leaks. `--sysroot` rewrites all path resolution.

### 2.5 Model-breakers
- **`--start-group`/`--end-group` bracket sublanguage** over positionals.
- **MSVC `/FLAG:value` dialect** (lld-link) — a whole second spelling grammar selected by flavor.
- **Flavor switch (`-flavor`/argv[0])** picks an entirely different Grammar — conditional top-level grammar, not a subcommand.
- **`-Bstatic`/`-Bdynamic` are mode toggles affecting subsequent `-l`**: `-Bstatic -lfoo -Bdynamic -lbar` — like clang's `-x`, a stateful switch mutating how following positionals resolve. No policy.
- Linker scripts (`-T`) are a Turing-ish DSL whose *content* changes layout; pure content-hash works but the model has no notion that this PathIn is "executable config."

### 2.6 Caching hazards
- `--build-id` default embeds a hash; `--build-id=uuid`/`=md5` adds entropy or content-hash. `--build-id=none` for determinism.
- **rpath / soname absolute paths** leak build layout into the binary.
- gold/lld emit `.comment` with linker version string.
- `-Map` mapfile contains absolute paths + addresses.
- Mach-O (`ld64`) embeds an `LC_UUID` derived from content+timestamp; `-no_uuid`/`ZERO_AR_DATE` to neutralize.
- Parallel section layout in lld is deterministic, but `--threads` count can change tie-breaking in some versions → pin or verify.

---

## 3. `ar` (archiver)

### 3.1 Invocation shape
**Pseudo-subcommand via a packed modifier string**, not a token: `ar` takes a leading
*key+modifiers* word like `rcs`, `qcs`, `dv`, `tv`, `xo`. The first non-dash arg is a
**concatenated mode string**: one operation key (`r/q/d/m/p/t/x/s`) + modifier letters
(`c v u s a b i N P T D U`). E.g. `ar rcs libfoo.a a.o b.o`. Modern binutils also
accepts `ar -r -c -s` and `ar --thin`. GNU `ar` has an `M` mode = **MRI script** read
from stdin (a sublanguage).

`Grammar.subcommands` partly applies if you treat `r/q/d/t/x/p/m/s` as subcommands —
**but they are packed into one token with modifiers**, which the dispatch-on-bare-token
model cannot parse. This is the canonical *count/packed-flag* breaker.

### 3.2 Spelling forms
- **Packed mode string** `rcsuD` — neither bare, attached, nor equals. A single token = operation + N modifiers. **Not in enum.**
- Dashed long: `--thin`, `--output=dir`, `--plugin name`, `--record-libdeps`.
- Positionals: archive name, then member files. `count`-like only in that modifiers stack.

### 3.3 Multiplicity & order
- Exactly one operation key per invocation (`Group`, mandatory).
- Modifiers form a set with positional sub-meaning: `a`/`b`/`i` take a *member name argument* that follows (relative-position insert): `ar rab existing.o lib.a new.o`. Order of members in the archive **is preserved and meaningful** (`Positional`).
- `D` (deterministic) vs `U` (real uid/gid/mtime) — last-wins toggle pair.

### 3.4 Path-bearing slots
- PathOut: archive file (also PathIn when updating — **same slot is both**, model has no "PathInOut").
- PathIn(file): member `.o` files.
- PathIn(dir): `--output=dir` (extraction dir).
- `--plugin path` (LTO plugin).

### 3.5 Model-breakers
- **Packed operation+modifier token** (`rcs`) — cannot be a bare subcommand token nor a flag.
- **MRI script mode (`M`)** reads a command DSL from stdin — sublanguage with no argv presence.
- **Positional member-relative insert** (`a`/`b`/`i` + anchor member name).
- Archive is simultaneously input and output (in-place update).

### 3.6 Caching hazards
- **Default `ar` is NONDETERMINISTIC**: embeds per-member mtime, uid, gid, mode. The `D` modifier (or binutils `--enable-deterministic-archives` / `ar rcsD`) zeroes them. `ranlib -D` likewise. `ZERO_AR_DATE=1` env neutralizes Apple `ar`. **This is the single biggest cache hazard in the group** — must force `D`.
- Member *insertion order* affects bytes even with `D`.

---

## 4. `ranlib`

### 4.1 Invocation shape
**Flat, single positional.** `ranlib [options] archive` — generates/updates the archive
symbol index (`__.SYMDEF`). Effectively `ar s`.

### 4.2 Spelling forms
- Bare: `-D` (deterministic), `-U` (real timestamps), `-t` (touch/update only), `-v`/`-V`.
- Long: `--help`, `--version`, `--plugin name`.
- One positional: the archive.

### 4.3 Multiplicity & order
- `-D`/`-U`: last-wins toggle pair (`Group`).
- Single positional archive (`Positional`, count=1).

### 4.4 Path-bearing slots
- PathOut/PathInOut: the archive (read + rewritten index).
- `--plugin path`.

### 4.5 Model-breakers
- Same in-place PathInOut issue as `ar`.

### 4.6 Caching hazards
- Same determinism hazard: symbol index entries carry timestamps unless `-D` / `ZERO_AR_DATE`.

---

## 5. `as` (assembler)

### 5.1 Invocation shape
**Flat.** `as [options] file...`. GNU `as` (gas); clang's integrated assembler invoked
as `clang -cc1as` or via `-Wa,`.

### 5.2 Spelling forms
- Bare: `-g` (debug), `--gstabs`, `--gdwarf-5`, `-n`, `-Z`, `--fatal-warnings`, `--noexecstack`.
- Separated: `-o out.o`, `-I dir` (include search), `--defsym sym=val` (also `=`), `-march=`(equals), `-mcpu=`.
- Equals: `-march=armv8-a`, `--MD file`, `--debug-prefix-map OLD=NEW`.
- Passthrough target: `as` is itself the *target* of clang/gcc `-Wa,`.
- `@file` response files (GNU). **Not in enum.**

### 5.3 Multiplicity & order
- `-I`: `DedupOrdered` (include path for `.include`).
- `--defsym a=b`: ordered assignment accumulation (a tiny define program, same as ld `--defsym`).
- `-o`: `Once`.
- inputs: `Positional`.

### 5.4 Path-bearing slots
- PathIn(file): `.s`/`.S` inputs; files referenced by `.include`/`.incbin` *inside* the asm (invisible to argv).
- PathIn(dir): `-I`.
- PathOut: `-o`, `--MD depfile`.
- PathMap: `--debug-prefix-map OLD=NEW` (same as cc).

### 5.5 Model-breakers
- `.include "path"` inside source = path dependency not on argv (same class as `#include`).
- `--defsym` ordered assignment program.

### 5.6 Caching hazards
- DWARF `.file`/`comp_dir` absolute path leakage → `--debug-prefix-map`.
- gas emits producer string with version; `.note.GNU-stack` defaults.
- `SOURCE_DATE_EPOCH` honored for any embedded timestamp.

---

## 6. `pkg-config`

### 6.1 Invocation shape
**Flat, query tool.** `pkg-config [options] pkgname...`. Multiple package names are
positionals; the *option set* selects what to print. No subcommands.

### 6.2 Spelling forms
- Bare query: `--cflags --libs --libs-only-L --libs-only-l --libs-only-other --cflags-only-I --cflags-only-other --modversion --static --exists --print-errors --silence-errors --print-requires --print-variables --list-all --uninstalled --print-provides`.
- Equals/separated: `--variable=NAME` (print one var), `--define-variable=NAME=VALUE` (an *assignment*), `--atleast-version=V`, `--exact-version=V`, `--max-version=V`, `--atleast-pkgconfig-version=V`, `--with-path=dir`, `--define-prefix`.
- Positionals: package names (and version constraints can be a 3-token positional triple: `pkg-config 'libfoo >= 1.2'` — operator embedded in one positional).

### 6.3 Multiplicity & order
- `--define-variable=N=V`: accumulate ordered assignments (a define program → same shape as cc `-D`).
- Output `--libs`/`--cflags`: pkg-config *itself canonicalizes* — it dedups `-I`/`-L`/`-l` "maintaining proper ordering" and merges across the transitive dep graph. So pkg-config's output is the *canonical argv fragment* feeding cc/ld — a canonicalizer producing input for another canonicalizer.
- Package positionals: order affects merge/dedup but result is order-stabilized.

### 6.4 Path-bearing slots
- PathIn(dir): `--with-path=dir`; **`PKG_CONFIG_PATH` / `PKG_CONFIG_LIBDIR` env** (colon-list of dirs holding `.pc` files — the entire input is env-valued, not argv).
- PathIn(file): the resolved `.pc` files (content-hash these — they *are* the inputs).
- `--define-prefix` rewrites the `${prefix}` var from the `.pc` file location (path-relative reinterpretation).

### 6.5 Model-breakers
- **Version-constraint positionals**: `'gtk+-3.0 >= 3.24'` packs name+operator+version into positional stream; the operator binds the surrounding tokens. Positional policy has no notion of relational operators.
- **Output is itself canonical argv for another tool** — pkg-config is a *grammar that emits a grammar fragment*. The model treats argv as flat text; here a slot's value is "expand `$(pkg-config --libs x)` into ld flags," a nested grammar produced at runtime.
- Env (`PKG_CONFIG_PATH`) is the primary input surface — no argv slot at all.

### 6.6 Caching hazards
- Output embeds **absolute prefix paths** from `.pc` files (`-I/usr/local/include`, `-L/opt/...`) → host-layout leakage into downstream cache keys.
- `.pc` file resolution depends on `PKG_CONFIG_PATH`/`PKG_CONFIG_LIBDIR`/`PKG_CONFIG_SYSROOT_DIR` env → must fold into key.
- `--define-prefix` makes output depend on the *install location* of pkg-config itself.

---

## 7. `cmake`

### 7.1 Invocation shape
**Genuine multi-level subcommand tree** — the one tool here that uses `subcommands`:
- Configure/generate (default mode): `cmake [opts] <source-dir>` or `cmake -S src -B build`.
- `cmake --build <dir> [--target t] [--config c] [-j N] [-- <native-args>]` — **build mode**, with a `--` passthrough into the native tool (make/ninja).
- `cmake --install <dir> [--prefix p] [--component c] [--config c]`.
- `cmake --open <dir>`.
- `cmake -E <command> ...` — **`-E` is a subcommand namespace**: `cmake -E copy a b`, `cmake -E make_directory`, `cmake -E env VAR=x -- cmd`, `cmake -E tar`, `cmake -E compare_files`, `cmake -E chdir dir cmd...`. Depth-2 subcommands.
- `cmake --find-package`, `cmake -P script.cmake` (script mode), `cmake --workflow --preset name`.
- `ctest`, `cpack` are sibling binaries with their own `-S`/`-C` flags.
- **Deepest nesting:** `cmake -E env FOO=bar -- cmake --build . -- -j4` → `-E env` subcommand whose payload after `--` is *another full cmake invocation* whose `--build` payload after a *second* `--` is native-tool args. **3+ levels of `--` nesting.**

`Grammar.subcommands` covers `-E <cmd>`, `--build`, `--install` *if* you allow a
subcommand to be introduced by a `--word` flag (not just a bare token). The model says
"subcommands dispatch on next bare token" — cmake dispatches on `-E`/`--build`/`-P`, which
are *flags*, plus a bare token (`-E copy`). Mixed dispatch.

### 7.2 Spelling forms
- Bare: `--build --install --fresh --debug-output --trace -N`.
- Attached/Equals: `-DVAR=VALUE` (cache entry — like cc `-D` but **typed**: `-DFOO:BOOL=ON`, `-DPATH:PATH=/x`), `-Wdev/-Wno-dev`, `-Werror=dev`.
- Separated: `-S srcdir -B builddir -G "Ninja" -A x64 -T toolset -C initial-cache.cmake --target t --config Release --prefix /p -j N`.
- Equals: `--preset=name`, `--graphviz=file`.
- `-U` cache-entry removal (globbing): `-UFOO*` — wildcard remove, **interacts with `-D` order**.
- `--` passthrough to native build tool (nested grammar = make/ninja).
- `@file` response (limited).

### 7.3 Multiplicity & order
- `-D` cache vars: **last-wins per name, typed**. Order: later `-D` overrides earlier same-name. `-U` removes (glob). So `-D`/`-U` is an *ordered typed key-value program* — strictly more than `Define`/`DedupUnordered`.
- `-G`, `-A`, `-T`, `-S`, `-B`: `Once`.
- `--target`: accumulates (multiple targets) → ordered list.
- `-j`: `Once`.

### 7.4 Path-bearing slots
- PathIn(dir): `-S`, source-dir positional, `--build <dir>`, `--install <dir>`.
- PathOut(dir): `-B` (build dir), `--prefix`.
- PathIn(file): `-C initial-cache`, `-P script`, `-DX:FILEPATH=`, preset files (`CMakePresets.json`).
- Typed paths: `-DFOO:PATH=` / `:FILEPATH=` — the **type tag itself signals path-ness**. Model's Kind can't carry the CMake type.

### 7.5 Model-breakers
- **Multi-level `--` nesting** (cmake → native tool → tool args).
- **`-E env VAR=v -- cmd`** is an exec wrapper: payload after `--` is an *arbitrary command grammar* (any tool in this study).
- **Typed cache entries** `-DNAME:TYPE=VALUE` — value carries an embedded type that determines kind. The Define kind has no type channel.
- **`CMakePresets.json`** supplies flags from a file keyed by preset name — config-file-as-argv-source, like a named response file.
- `-S`/`-B` vs positional source-dir = two spellings for the same slot, last/precedence rules.

### 7.6 Caching hazards
- `CMakeCache.txt` records **absolute paths** (compiler path, source/build dirs) → the build tree is non-relocatable; key must capture or normalize.
- Generated build files embed absolute paths and timestamps; `--fresh` changes behavior based on prior cache state (stateful — output depends on pre-existing build dir).
- Environment (`CC`, `CXX`, `CFLAGS`, `CMAKE_*`, `PATH`) silently steers configure.

---

## 8. `make`

### 8.1 Invocation shape
**Flat, but positionals are a mixed grammar of targets AND variable assignments.**
`make [options] [VAR=value...] [target...]`. No subcommands; but the positional stream
interleaves three kinds: goal targets (`all`, `clean`), command-line variable overrides
(`CC=clang`, `CFLAGS=-O2`), and that's it. GNU make.

### 8.2 Spelling forms
- Bare: `-k -i -n/--dry-run -s -B/--always-make -r -R -p -q -d -w`.
- Separated/Attached: `-j[N]` (**optional attached count**, `-j` alone = unlimited), `-l[N]` (load), `-C dir`, `-f makefile`, `-I dir`, `-O[type]` (output-sync, optional attached), `-W file` (what-if).
- Equals: `--jobs=N`, `--output-sync=target`, `--debug=basic`.
- **Positional `VAR=value`** — looks like Equals but is a *positional*, and **`VAR := v`, `VAR += v`, `VAR ?= v`** assignment operators can appear. Overrides win over makefile (unless `override`).
- `--` end of options.
- `MAKEFLAGS` env re-injects flags (recursive make passes flags via env, **argv ⇄ env round-trip**).

### 8.3 Multiplicity & order
- `-f`: accumulates ordered (multiple makefiles concatenated in order — `DedupOrdered`, no dedup).
- `-I`: `DedupOrdered`.
- `-C`: accumulates and **composes** (`-C a -C b` → cd a then cd b, *relative chaining*). Order-critical path composition — no policy.
- `-j`: `Once`, optional value.
- targets: `Positional`, order = build order requested.
- `VAR=value`: last-wins per var (override), but appears positionally.

### 8.4 Path-bearing slots
- PathIn(file): `-f makefile`, included makefiles (`include` directive — invisible).
- PathIn(dir): `-C` (chdir), `-I` (include search).
- `-o file`/`-W file`: timestamp what-if (file referenced, not opened for content).
- Env-valued: `MAKEFILES`, `VPATH`/`vpath` search, `CPATH` via recipes.

### 8.5 Model-breakers
- **Positional variable assignments with operators** (`CC=clang`, `FOO+=x`) interleaved with target positionals — two semantically distinct positional classes distinguished by containing `=`, plus operator variants. No model for "positional that is a typed assignment."
- **`-C` relative chdir composition.**
- **`MAKEFLAGS` env ⇄ argv** round-tripping (sub-makes inherit flags via env, not argv).
- The Makefile itself is a full programming language; everything cc/ld-relevant is *inside* it, not on make's argv.

### 8.6 Caching hazards
- **Timestamp-driven**: make's entire model is mtime comparison → not content-based; a content cache must ignore make's own staleness logic.
- `-j` parallelism → **nondeterministic recipe output interleaving** on stdout (mitigated by `-O`/`--output-sync`). Build *artifacts* should be deterministic but logs aren't.
- `$(shell ...)`, `$(wildcard)` make output depend on filesystem/host state.

---

## 9. `ninja`

### 9.1 Invocation shape
**Flat for builds; a `-t <tool>` subcommand namespace for everything else.**
`ninja [options] [targets...]` builds. `ninja -t <subtool> [args]` dispatches a real
subcommand tree (confirmed against the manual):
- `ninja -t list` (list tools), `query <path>`, `targets [rule|depth N|all]`,
  `commands [targets]`, `inputs <targets>`, `deps [targets]`, `graph [targets]`,
  `compdb [rules]`, `compdb-targets <targets>`, `recompact`, `restat`, `cleandead`,
  `clean [-g] [-r] [targets|rules]`, `browse <target>`, `rules`, `msvc -- cl ...`.
- **Deepest nesting:** `ninja -t msvc -e envfile -- cl.exe /c x.cpp` — the `msvc` subtool
  takes options then `--` then a *full cl.exe command* (nested grammar). And `-t clean
  -r rule1 rule2` is subtool + mode-flag + positional-rule-list.

`Grammar.subcommands` fits `-t <tool>` **only if a subcommand can be introduced by the
`-t` flag carrying the name as its value** — i.e. dispatch on a *flag value*, not a bare
token. Pure "next bare token" dispatch fails (`-t` is the selector, the tool name is its
argument).

### 9.2 Spelling forms
- Bare: `-v` (verbose), `-n` (dry run), `--quiet`, `--version`.
- Attached/Separated: `-j N` (parallelism), `-k N` (keep-going count, `-k0`=infinite), `-l N` (load avg), `-C dir` (chdir before), `-f build.ninja` (input file), `-d MODE` (debug: `stats`,`explain`,`keepdepfile`,`keeprsp`), `-t TOOL` (tool selector), `-w FLAG[=mode]` (warnings, e.g. `-w dupbuild=err`).
- `-d`/`-w`: **keyword-valued, accumulate** (`-d stats -d explain`).
- targets: positionals.
- **Internal response files (`.rsp`)**: ninja *generates* `@rsp` files for long command lines (`rspfile`/`rspfile_content` in build.ninja) — response files as an *output/intermediate*, not just input.

### 9.3 Multiplicity & order
- `-j`,`-k`,`-l`,`-C`,`-f`: `Once` (last-wins).
- `-d`,`-w`: accumulate keyword set (`DedupUnordered` over keywords; each is `keyword` or `keyword=mode` → namespaced last-wins like cc `-W`).
- targets: `Positional`.
- `-C dir`: single chdir (unlike make's chaining).

### 9.4 Path-bearing slots
- PathIn(file): `-f build.ninja`; `compdb`/`graph` write to stdout (no PathOut flag).
- PathIn(dir): `-C`.
- The real path universe lives **inside `build.ninja`** (a generated file) — ninja's argv barely touches paths; the `.ninja` file content is the cache input.

### 9.5 Model-breakers
- **`-t TOOL` = subcommand selected by a flag's value**, then the rest of argv is that subtool's own grammar (`clean -r`, `compdb rule...`, `msvc -- cl...`).
- **`-t msvc -- <cl command>`** nests a full compiler grammar after `--`.
- **`-w dupbuild=err`** = namespaced toggle with severity (like `-Werror=`), no policy.
- Generated `@rsp` response files as *outputs*.

### 9.6 Caching hazards
- Default ninja output is reproducible, but **`-j`/parallel scheduling reorders stdout** and the `.ninja_log`/`.ninja_deps` capture mtimes + restat state (stateful between runs).
- `ninja -t compdb` emits a JSON DB with **absolute command paths and dirs** → leaks build layout downstream.
- Like make, staleness is mtime-based; restat (`-t restat`) rewrites the log.
- `-d keeprsp`/`-d keepdepfile` leave temp artifacts behind (nondeterministic temp names).

---

## Cross-tool synthesis

### Spelling forms the enum LACKS (used widely)
1. **Response files `@file`** (cc, ld, ar, as, gcc) and **MSVC `@rsp`** (lld-link) — recursive argv splice from a PathIn whose *content* is argv. Also ninja *emits* them.
2. **`--` end-of-options sentinel**, and **multi-level `--` nesting** (cmake/ninja → native tool → tool args).
3. **Windows `/FLAG:value` dialect** (lld-link, and the parallel cl/MSVC world) — selected by a flavor switch.
4. **Packed operation+modifier token** (`ar rcsD`) — operation key + N stacked modifier letters in one token.
5. **Positional typed assignments** (`make CC=clang`, `make FOO+=x`) — Equals-shaped but positional, with `=`/`:=`/`+=`/`?=` operators.
6. **Optional-attached count** (`make -j`, `ninja -j N`, `-jN`).
7. **Flavor/dialect selector** (`lld -flavor`, `cmake -E`, `ninja -t`) — picks a different whole grammar, dispatched by a flag *value*, not a bare token.

### Policies MISSING
- **OrderedDefineProgram** (`-D`/`-U` in cc; `--defsym` in ld/as; `-D`/`-U` in cmake): sequenced define/undef, NOT a sorted set. The model's `DedupUnordered` for `-D` is outright wrong.
- **NamespacedToggle** (`-Wall/-Wno-unused/-Werror=x`; `-f…/-fno-…`; `-m…/-mno-…`; ninja `-w x=err`): per-name last-wins on/off/severity; canonical form is resolved per-name state, not raw list.
- **No-dedup ordered positional `-l`** (linker): repeats are sometimes mandatory; `DedupOrdered` breaks links.
- **Stateful mode switch over following positionals** (`cc -x lang`, `ld -Bstatic/-Bdynamic`, `ar` insert anchors): a flag mutates how *subsequent positionals* are interpreted.
- **Bracketed positional span** (`ld --start-group … --end-group`): a re-scan region.
- **Relative-chain path composition** (`make -C a -C b`).

### Kinds MISSING
- **ArgFile** — PathIn whose content is re-parsed as argv (recursive).
- **LibName** — `-l` name resolved against ordered `-L` search path (+`-Bstatic`) to a concrete archive; literal isn't a path.
- **PathMap** — `OLD=NEW` rewrite pair (`-fdebug-prefix-map`, `--debug-prefix-map`, `-ffile-prefix-map`); both sides paths, neither opened, purpose is determinism.
- **PathInOut** — `ar`/`ranlib` archive: read and rewritten in place.
- **TypedPath** — cmake `-DX:PATH=`/`:FILEPATH=` where a type tag inside the value declares path-ness.
- **EnvPath** — include/lib/exec dirs delivered via env (`CPATH`, `LIBRARY_PATH`, `PKG_CONFIG_PATH`, `SDKROOT`, `MAKEFLAGS`) with no argv slot at all — yet fully cache-key-relevant.
- **EmbeddedPath** — `-rpath`/`-soname`/linker-script paths baked into the *output binary* as literal content (relocatability + leakage concern).

### Genuine model-breakers
- **Conditional grammar by mode flag** (cc `-c/-S/-E` changes which flags are legal and what `-o`/positionals mean).
- **Sub-languages beyond `-Wl,`**: linker scripts (`-T`), MRI scripts (`ar M`), CMake scripts (`-P`, presets), the Makefile language, `build.ninja` — the cc/ld-relevant paths and flags live *inside these files*, off-argv.
- **Tool-produces-grammar**: `pkg-config --libs` emits the canonical `-L/-l` fragment that feeds ld — a canonicalizer feeding a canonicalizer.
- **Env ⇄ argv round-trip** (`MAKEFLAGS`).

### Caching hazards (the canonicalizer MUST neutralize or mark impure)
- **`ar`/`ranlib` nondeterministic member mtime/uid/gid** → force `D` / `ZERO_AR_DATE` / `--enable-deterministic-archives`. (Biggest single hazard.)
- **`__DATE__`/`__TIME__`/`__TIMESTAMP__`** and any embedded timestamp → require `SOURCE_DATE_EPOCH`.
- **Absolute-path leakage** into debug info / `__FILE__` / `DW_AT_comp_dir` / build-id maps / pkg-config output / CMakeCache / compdb → require `-f*-prefix-map`, normalize cwd.
- **`-frandom-seed`** (anon-namespace mangling, gcda naming) → pin.
- **`--build-id` / `LC_UUID`** entropy → pin (`--build-id=none`/`-no_uuid`).
- **`-march=native`** host-CPU-dependent → expand or mark host-impure.
- **Env steering** (`CC/CXX/CFLAGS`, `*_INCLUDE_PATH`, `LIBRARY_PATH`, `PKG_CONFIG_PATH`, `SDKROOT`, `MAKEFLAGS`) → fold into key though off-argv.
- **Parallel stdout interleaving** (`make -j`, `ninja -j`) → output logs non-reproducible; artifacts should be deterministic but verify.
- **rpath/soname absolute paths** baked into binaries → non-relocatable + leakage.

Sources: [ninja manual](https://ninja-build.org/manual.html), [pkg-config(1)](https://linux.die.net/man/1/pkg-config), plus binutils/clang/gcc/cmake/make reference docs.
