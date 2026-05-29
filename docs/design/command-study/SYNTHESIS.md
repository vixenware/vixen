# Command-Model Synthesis — Union of the CLI-Grammar Study

Source digests: `rust`, `cc-posix`, `msvc`, `windows-system`, `apple`, `sandbox`,
`go`, `js`, `haskell`, `codegen`. Per-group detail in the sibling `*.md` files.

This document is the **union** of everything the ten studies demand of the Vixen
command model, deduplicated and ranked. It answers the only question that
matters for the formalism: **does `Grammar { flags, positionals, subcommands }`
+ `Spelling` + `Policy` + `Kind` survive as an additive extension, or does some
tool force a structurally different model?**

Verdict up front: **the additive core survives for parsing and per-flag
canonicalization, but three of its four shape assumptions are false and must be
replaced, not extended.** See the Verdict section.

---

## 1. Required `Spelling` variants

Baseline: `Bare`, `Attached`, `Separated`, `Equals`, `Passthrough(sep)`.

The study collapses the long tail into the following additional variants. Each
is listed with the **forcing tool** (the one that makes it unavoidable, not
merely convenient).

| Spelling | What it is | Forced by |
|---|---|---|
| **DualLead** | `/` and `-` (and `--`) interchangeable for the same flag; canonicalize to one | MSVC `cl /c == -c`; clang-cl mixes `/O2 -Wall` in one argv |
| **ColonAttached** | value after `:` glued to the name, no space, not `=` | MSVC `/OUT:foo.exe`, `/std:c++20`; apple `-only-testing:MyTests`; windows `reg /reg:32`, `light -cultures:...` |
| **GluedNoDelim** | value glued with **zero** delimiter; boundary is option-table-driven | MSVC `/Fooout.obj`, `/Iinc`, `/dNAME`; clang `-O2/-DFOO/-lm` already in Attached but MSVC has no delimiter at all |
| **NegationSuffix** | trailing `-` glued to name flips a bool | MSVC `/permissive-`, `/GR-`, `/Zc:wchar_t-` |
| **PrefixNegationPair** | polarity infixed into the spelling: `-fX`/`-fno-X`, `-XExt`/`-XNoExt`, `-W`/`-Wno-`/`-Werror=`/`-Wwarn=` | Haskell ghc; clang/gcc `-Wno-`; go cgo |
| **OptionalValue** | flag legal both bare and with a value, meaning changes | cargo `--bin`; node `--inspect[=h:p]`; firejail `--private[=dir]`; codesign `--timestamp[=url]`; make/ninja `-j[N]` |
| **EmbeddedKV** | value is itself `key=value`, one side often a path | rustc `--extern serde=lib.rlib`, `--remap-path-prefix /a=/b`; dsymutil `--object-prefix-map /a=/b`; cmake `-DX:BOOL=ON` |
| **ColonKeyEquals** | two-level `--key:subkey=value` | esbuild `--define:X=V`, `--loader:.png=dataurl`; bun |
| **DottedPath name** | flag *name* is a dotted path into a nested object, arbitrary depth | rollup `--output.entryFileNames`; webpack `--env a.b.c=v`; firejail `--caps.drop` |
| **MultiSep** | one token accepts both comma **and** space as internal sep | cargo `-F "serde tokio" == -F serde,tokio` |
| **CommaSubGrammar** | comma-list is a nested mini-grammar inside one token (not opaque passthrough) | MSVC `/OPT:REF,ICF`; clang `-fsanitize=address,undefined`; codesign `-o runtime,kill,hard` |
| **PackedOpModifier** | one token = operation key + stacked modifier letters | `ar rcsD` |
| **LetterSet / BitfieldSuffix** | order-independent set of single chars glued to a flag | msiexec `/qb!`, `/fpecms`, `/l*v` |
| **Count** | multiplicity *is* the value | cbindgen `-vvv`; python `-v`/`-vv`; codesign `-v -v -v`; python `-O`/`-OO` |
| **SeparatedN{n}** | flag consumes N (≥2) following value tokens | bwrap `--bind SRC DEST`, `--overlay A B C`; lipo `-replace arch file`; install_name_tool `-change old new` |
| **VariadicSeparated** | flag opens a run of values until the next option | mt `-manifest a b c -out:m`; codesign positional pid lists |
| **SingleDashLong** | multi-char long flags under a single dash (getopt_long_only) | apple `xcodebuild -project -scheme`; swig `-python -outdir`; lipo `-create -output` |
| **ResponseFile / ArgFile (@path)** | path whose **contents are argv**, spliced recursively | rustc `@args.txt`; clang/ar/ld `@file`; tsc `@args.txt`; pip `-r`; candle `@rsp`; protoc `@args.txt` |
| **ToolchainSelector (leading `+`)** | leading `+name` rebinds which binary runs; neither flag nor subcommand | cargo/rustup `+nightly` |
| **EndOfOptions → nested grammar / stop-parsing** | `--`, `-args`, mode token, or `+RTS…-RTS` hands the tail to a *different* grammar | cargo `test -- …`; cgo `-- <cc argv>`; bindgen `-- <clang>`; go `-args`; ghc `+RTS … -RTS`; powershell `--%` |
| **BracketedSubArgv** | paired delimiters enclosing another tool's argv, repeatable | ghc/stack `+RTS … -RTS` |
| **ForeignArgvString** | flag value is a shell-quoted full argv of another tool (word-split + recurse) | cabal `--ghc-options="…"`; go `-gcflags='pkg=-m -l'`; rustc `-Cllvm-args="-x -y"` |
| **OneTokenForward** | flag forwards exactly one following token uninterpreted | clang-cl `-Xclang -ast-dump`; clang-cl `/clang:-fno-rtti` (ColonGluedForward) |
| **EncodedPayload (base64)** | value is base64 of a foreign program | powershell `-EncodedCommand` |
| **EnvInjectedArgv** | tokens prepended/appended from the environment, invisible in literal argv | MSVC `CL`/`_CL_`; node `NODE_OPTIONS`; make `MAKEFLAGS` |

---

## 2. Required `Policy` variants

Baseline: `Once`, `Group`, `DedupUnordered`, `DedupOrdered`, `Positional`.

| Policy | What it does | Forcing case |
|---|---|---|
| **KeyedLastWins / Map{key}** | dedup by an embedded *key*, value last-wins per key | rustc `--extern serde=…` (twice → last); MSVC `/DFOO=1 /DFOO=2`; esbuild `--define:X=1 --define:X=2`; xcodebuild `SYMROOT=…` |
| **NamespacedOnce** | independent last-wins per key inside an open namespace (~60 keys) | rustc `-C…`, cargo `--config…` |
| **OrderedDefineProgram / OrderSensitiveOverride** | `-D`/`-U` is a *sequenced* define/undef stream, dup+order meaningful — `DedupUnordered` on `-D` is **wrong** | clang `-DFOO=1 -UFOO -DFOO=2`; ghc `-D/-U`; swig |
| **NoDedupOrderedLib** | `-l` repeats are sometimes **mandatory** (circular archives) — dedup breaks the link | cc `-la -lb -la`; link `foo.lib bar.lib foo.lib` |
| **OrderedKeepAll (pipeline)** | ordered, dup-meaningful pipeline; dedup changes behavior | node `-r a -r b -r a`; rollup/webpack `--plugin`; esbuild `--inject` |
| **AccumulateWithNegation / AccumPolarized** | base flag + negated antonym accumulate, last mention of the knob wins | clang `-Wfoo/-Wno-foo`; `-frtti/-fno-rtti`; link `/DEFAULTLIB / /NODEFAULTLIB`; xcodebuild `-only-testing/-skip-testing` |
| **PolarityKeyedAccumulate (tri/quad-state)** | ordered last-op-per-key over an **open** keyspace with ops {set,unset,promote-error,demote-warn} | ghc `-Wall -Wno-x -Werror=y -Wwarn=z`; the warning *algebra* |
| **LintLattice** | resolve over ALL occurrences via a priority lattice; `forbid` can't be downgraded; `--cap-lints` clamps | rustc `-W/-A/-D/-F/--force-warn/--cap-lints` |
| **AliasGroup / LastWinsAcrossAliases** | last-wins must span *different* flag spellings that collide on one canonical key | rustc `-O == -Copt-level=3`, `-g == -Cdebuginfo=2`; cargo `-r == --profile=release`; clang-cl `-O2` then `/Od` |
| **Group (cross-flag) / Exclusive** | conflict between *separate* flags; supplying two is an **error**, not last-wins | go `-race`/`-msan`/`-asan`; msiexec `/i`/`/x`; signtool cert selection; go mod `-fmt`/`-print`/`-json` |
| **KeyedMap-in-positional / KeyedAssign** | order-insensitive, last-wins, sort-by-key — but living in **positional** slots | msiexec `NAME=VALUE` props; WiX `-dName=Value`; make `CC=clang` |
| **CommaSet** | dedup-set *inside* one comma value (sort+dedup the list) | codesign `-o runtime,library,kill`; `--preserve-metadata=…` |
| **UnionQuery** | repeated values are sub-queries, results union | pnpm/bun `--filter` |
| **CrossFlagSet / CrossFlagAccumulate** | two different flag names edit one shared accumulator | npm `--omit`/`--include`; protoc `--go_out`/`--go_opt`; pip `--config-settings` |
| **SharedOrderedAccumulator** | several distinct spellings feed ONE ordered search list | clang `-I/-isystem/-iquote/-idirafter`; MSVC include order |
| **RelativeChainPathComposition** | repeated flag composes cumulatively, order-critical | make `-C a -C b` |
| **StatefulPositionalModeSwitch** | flag mutates interpretation of *subsequent* positionals until changed | clang `-x c a.txt -x c++ b.txt`; ld `-Bstatic … -Bdynamic`; cl `/Tc`/`/Tp` |
| **BracketedPositionalSpan** | re-scan region wrapping a span of positionals | ld `--start-group … --end-group` |
| **StickyModifier** | flag scoped to the *next* flag | bwrap `--perms 0700 --dir /x`, `--overlay-src`; xcodebuild `-headers` binds preceding `-library` |
| **OrderedAccum / OrderedConcatNoDedup** | repeatable flag, order semantic, no dedup (forwarded verbatim) | install_name_tool `-add_rpath A -add_rpath B`; cabal `--ghc-options`; ghc `-optl`; `+RTS` |
| **DedupByKeyWithMerge** | dedup by sub-key, collisions **merge** (e.g. union access rights) | landrun `--ro /x --rwx /x` → union; bwrap repeated bind-to-DEST |
| **KeyedMap with interleaved deletes** | keyed map where delete entries interleave with sets | bwrap `--setenv VAR x … --unsetenv VAR`; sandbox-exec `-D` |
| **EditScriptOrdered (paired add/drop, replay)** | canonical form is the *resulting file*, produced by replaying argv in order — cannot sort | go mod `-require/-droprequire/-replace/...` |
| **AccumulateMerge / Intersect** | repeatable, semantically intersected | cabal `--constraint "text>=2" --constraint "text<3"` |
| **Count / CancelingCount** | repeats net to a (possibly signed) level | pip `-v -q -v`; codesign `-v -v -v` |
| **VerbDependent** | policy/kind not fixed until the subcommand is known | reg `/s` (recurse under `query`, separator-spec under `add`); reg `/f` |
| **ConditionalDefault** | which flags are legal / their defaults depend on flags already seen | bindgen `--allowlist-*` flips default visibility; tsc `-b`; swig `-<lang>` |
| **FlagToFlagValuePropagation** | setting one flag silently rewrites another's canonical value | go `-race` forces `-covermode=atomic`, appends `_race` |
| **SuffixVariantPolicy** | same base spelling, different accumulation by suffix | stack `--ghc-options` vs `--ghc-options-once` |
| **ConsumerRelativeCanonicalName** | canonical spelling depends on who consumes the flag | go rewrites `-cpuprofile` → child `-test.cpuprofile`, drops `-x` |
| **AccumulateAcrossInvocations** | semantics span separate process runs | signtool `/as` (layer signatures) |
| **EnvRegexGovernedLegality** | which flags parse at all is decided by a runtime env regex | cgo: only `-D -U -I -l` unless `CGO_CFLAGS_ALLOW` widens |

---

## 3. Required `Kind` variants

Baseline: `Str`, `PathIn(dir?)`, `PathOut`, `Include`, `Lib`, `Define`.

| Kind | What it is | Forcing case |
|---|---|---|
| **PathOutDir** | output is a **directory**; tool picks the artifact filename | rustc `--out-dir`; cargo `--target-dir`; go `-o ./bin/`; tsc `--outDir`; actool `--compile out/` (→ Assets.car); flatc `-o out/`; ghc `-odir` |
| **PathInOut / InOutPath** | one slot read **and** rewritten in place | ar/ranlib archives; mt `-updateresource`; signtool sign; install_name_tool; codesign; gofmt `-w`; go vet `-fix` |
| **ArgFileRef / ResponseFile / PathRecursive** | file whose **contents are argv**; content must be hashed + expanded recursively | rustc `@args.txt`; libtool `-filelist`; pip `-r`; tsc `@`; protoc `@` |
| **IndirectInputs / TransitiveInputSet / DebugMapFanout** | true inputs are a graph discovered from a file's contents, never on argv | dsymutil (OSO `.o` via debug map); WiX light `File/@Source` via `-b`; rc `.rc` includes; cgo build closure; cargo (Cargo.lock + build.rs) |
| **ValueDependentPath / TargetSpec / NameOrPath** | same flag is literal Str OR path depending on the value | rustc `--target triple` vs `./custom.json`; cargo `--config k=v` vs `conf.toml`; xcrun `--sdk iphoneos` vs `/path` |
| **ModuleSpecifier / PackageSpec** | bare-name OR path OR git/registry; only the path case is mountable | node `-r ts-node/register` vs `./local.js`; npm `react` vs `./local` vs `github:…` |
| **ImportPath / Checksummed** | resolved via module cache + checksum file, not a local path | go `example.com/foo/v2`; go mod `-require=…@v0.1.0` |
| **Glob / Pattern** | expands at runtime to a set; key on the expansion, not the literal | go `./...`; `-coverpkg ./internal/...` |
| **Overlay** | path whose contents are a *map* of further paths to hash | go `-overlay overlay.json` |
| **UrlBase / StrPathLike** | path-shaped but a URL prefix / literal emitted into output — **must NOT** be mounted or hashed | vite `--base`; esbuild `--public-path`; flatc `--include-prefix` |
| **Url(impure-network)** | path-shaped slot is a network URL; contacting it is impure | signtool `/tr`; pip `--index-url`; codesign `--timestamp=url` |
| **LibName** | `-lfoo` resolves against ordered `-L` (+ static/dynamic state) to a concrete archive; hash the resolved file | cc `-lm`; clang-cl `-l` |
| **LibSearchDir** | library search dir, a distinct namespace from include dir | link `/LIBPATH:`; clang-cl `-L` |
| **Framework / FrameworkPath** | macOS link inputs unlike `-l`/`-L` | ghc/clang `-framework Cocoa -framework-path` |
| **PathInList(sep)** | colon/sep-joined accumulating search-path list inside one token | ghc `-isrc:gen:vendor`; `GHC_PACKAGE_PATH` |
| **PathList** | one value carries multiple `;`-separated paths to split + hash each | msiexec `TRANSFORMS=a.mst;b.mst`; light `-cultures:en-us;fr-fr` |
| **PathMap** | `OLD=NEW` rewrite pair; neither side opened; exists to neutralize abs paths for determinism | cc `-fdebug-prefix-map`; `-ffile-prefix-map`; dsymutil `--object-prefix-map` |
| **EmbeddedPath** | path baked into the output binary as literal content (relocatability/leak concern) | ld `-rpath`, `-soname` |
| **LogicalPath / LogicalMember** | path-shaped token resolved at load time or inside an archive — **must NOT** be hashed/mounted | install_name_tool `@rpath/…`, `@loader_path/…`; lib `/EXTRACT:member.obj`; dumpbin `/IMPORTS:foo.dll` (filter) |
| **TypedPath** | a type tag embedded in the value declares path-ness | cmake `-DBOOST_ROOT:PATH=…`, `-DCFG:FILEPATH=…` |
| **DefinePath** | a define/property value that is a content-bearing path | candle `-dImageDir=src\images`; msiexec `TRANSFORMS=…` |
| **Exe / ToolBinary / PathExe** | names an executable (PATH-resolved or absolute) whose content/version gates the whole key | rustc `-Clinker=cc`; ghc `-pgmc/-pgml`; cabal `--with-compiler`; protoc `--plugin=…`; bindgen libclang |
| **ProfileData** | PGO/coverage input that makes the build path-keyed | rustc `-Cprofile-use=foo.profdata` |
| **RegPath** | hierarchical non-filesystem namespace (the real I/O is key+subtree) | reg `HKLM\Software\…` |
| **MountPoint** | virtual in-sandbox destination path; canonicalize as string, never mount/hash | bwrap `--bind SRC DEST` (DEST); firejail whitelist/tmpfs targets |
| **Grant(mode)** | path + r/w/x bitmask, deduped by path with mode-union | landrun `--rox /usr/lib`; `--rw /tmp` ∪ `--ro /tmp` |
| **Fd** | value is an inherited FD integer; the bytes behind it are out-of-band | bwrap `--args FD`, `--seccomp FD`, `--bind-data FD DEST` |
| **EnvPath / EnvSearchPath / EnvValuedPath** | path-bearing inputs delivered via env, no argv slot, yet fully key-relevant | `CPATH`, `LIBRARY_PATH`, `SDKROOT`, `PKG_CONFIG_PATH`, `GHC_PACKAGE_PATH`, `CGO_LDFLAGS`, `PYTHONPATH`, `MAKEFLAGS` |
| **EnvPair** | `KEY=VALUE` payload riding inside another slot, sometimes split across two tokens | sandbox-exec `-D HOME=/x`; bwrap `--setenv HOME /x` |
| **Stdin sentinel (`-`)** | bare `-` means stdin, not a path | powershell `-File -`; python `-`; pip `-` |
| **ResourceLocator** | path + `;#`-resource-id micro-grammar; only the path part is a real input | mt `-inputresource:foo.dll;#1` |
| **PathOrSentinel** | path OR a reserved keyword that must not be hashed as a path | cabal `--package-db=clear|global|./db`; stack `--resolver=lts-22.0|./snap.yaml` |
| **PathOrName** | literal path OR a name/GUID resolved elsewhere | candle `-ext WixUtilExtension`; msiexec `/x {GUID|path}` |
| **EmbeddedDSL / Requirement / PolicyScript** | value is a whole sub-language with its own parser + normal form + include closure | codesign `-r 'anchor apple …'`; sandbox-exec SBPL; firejail profiles; gofmt `-r 'a[b:len(a)]->a[b:]'` |
| **Undef** | removes a prior define; interleaves with `-D` as ordered last-op-per-symbol | ghc/clang `-UFOO` |
| **TargetSelector** | structured `[pkg][:ctype:cname][:all]` selector, not a path | cabal `build pkg:foo:test:spec` |
| **ForeignProgram** | value is a Turing-complete program (cannot canonicalize) | powershell `-Command`, `-EncodedCommand` |

---

## 4. Genuine model-breakers (ranked by tool-count)

These are the shapes the recursive `Grammar { flags, positionals, subcommands:
Map<String,Grammar> }` + per-flag `Policy`/`Kind` **cannot express**. Ranked by
how many of the ten groups hit them. These are the open formalism decisions.

### B1. Mode/grammar selected by a FLAG (or flag *value*), appearing anywhere — **8/10 groups**
`subcommands` is a closed `Map` dispatched on the *next bare token*. But the
mode selector is repeatedly a **flag**, and it reshapes which flags are legal,
what positionals mean, and whether other flags exist at all.
- cc `-c`/`-S`/`-E` (changes `-o` meaning, makes `-l` illegal); rust none-bare;
  MSVC `link /lib`/`/dump`, `cl /link`; apple `lipo -create` vs `-thin`,
  `codesign -s/-v/-d`; ghc `-c`/`--make`/`-e`/`--show-iface`; tsc `-b`; node
  `-e`/`--test`; ninja `-t compdb`, lld `-flavor`; reg verb-dependent flags;
  bindgen allowlist; swig `-<lang>`.
- **Implication:** the grammar must be a function of accumulated mode state, not
  a static tree. This is *conditional productions* — see Verdict.

### B2. Foreign / open grammar at a boundary (`--`, mode token, plugin, ext) — **9/10 groups**
The post-boundary argv belongs to a **different tool** whose grammar is selected
dynamically and is often statically unknowable.
- cargo `test -- <libtest>`; cgo `-- <cc>`; bindgen `-- <clang>`; cmake `-E env
  -- … -- -j4` (3 levels); cabal/stack `--ghc-options` → ghc → ld → `-Wl,`;
  sandbox `[wrapper] -- COMMAND [args]`; python `-m`/`-c`/script; protoc
  `--foo_out` → `protoc-gen-foo`; go `tool <name>`, `-vettool`; js
  `run <script>`, `npx`/`bunx`.
- `subcommands: Map<String,Grammar>` is **closed**; this needs an *open* edge to
  a grammar resolved at runtime (and recursion depth ≥3 across tool boundaries).

### B3. The argv is NOT the cache key — invisible input surface — **9/10 groups**
The real key is argv **plus** env, config files, lock files, and content
discovered from files. An argv-only canonicalizer is *unsound*.
- cargo: Cargo.lock + RUSTFLAGS + build.rs closure; cabal/stack:
  cabal.project / stack.yaml.lock / `GHC_ENVIRONMENT`; cc/pkg-config: `CPATH`,
  `LIBRARY_PATH`, `PKG_CONFIG_PATH`; MSVC `CL`/`_CL_`/`INCLUDE`/`LIB`; node
  `NODE_OPTIONS`/`--env-file`; go `CGO_*`/`GOFLAGS`; make `MAKEFLAGS`; cgo
  `CGO_CFLAGS_ALLOW`; WiX `-b` discovery; dsymutil OSO fanout.
- **Implication:** the canonical key is a *resolved-effective-command* object,
  not a canonical argv string. Env and discovered-input closures are first-class.

### B4. Cross-flag coupling (alias / shared accumulator / propagation / scope) — **8/10 groups**
Canonicalization needs edges *between* flags, which the per-flag `Policy` model
forbids.
- rustc `-O`→`-Copt-level`; cross-flag exclusion go `-race/-msan`; shared include
  order clang `-I/-isystem`; npm `--omit/--include`; protoc `--go_out/--go_opt`;
  adjacency apple `-library`/`-headers`; sticky bwrap `--perms/--dir`; ghc
  `-pgmc` changes how `-optc` is read; go `-race` rewrites `-covermode`.
- Needs an **alias/coupling graph** layered over the flag set.

### B5. Reduction-function policies over an open keyspace — **5/10 groups**
The canonical value of a knob is a *fold* over every occurrence against a
lattice/algebra — no per-flag `Policy` enum value expresses the join.
- rustc lint lattice (`forbid` floor, `--cap-lints` clamp); ghc warning algebra
  (set/unset/promote/demote); clang `-D/-U` ordered program; go mod edit script
  (replay → resulting file); sandbox grant merge (mode-union).
- These are *user-supplied reducers*, not enum tags.

### B6. Value sub-languages that are themselves grammars — **7/10 groups**
`Passthrough(sep)` is single-level and opaque. Real values nest 2–3 levels and
have their own normal forms.
- `-Wl,` → ld argv; rustc `-Clink-args=-Wl,…` (3 levels); MSVC `/OPT:REF,ICF`;
  codesign requirement DSL; sandbox SBPL/profile include-closure; esbuild
  `--define:k=v`; protoc `optlist:path`; powershell program; gofmt rewrite rule;
  pnpm filter query language.
- Needs `Passthrough(payload: Grammar | DSL-parser)`, recursively.

### B7. Positional schema is open / kind is position-dependent — **6/10 groups**
`positionals: [Positional]` assumes a closed, statically-typed schema.
- xcodebuild arbitrary `KEY=VALUE` build settings; msiexec `NAME=VALUE` props;
  make `CC=clang`; clang `-x` retags following positionals; cl `/Tc`/`/Tp`; ar
  packed-modifier anchors; codesign positional is path-or-pid by mode.
- Positionals need their own (conditional, keyed) sub-grammar.

### B8. In-place mutation: input path == output path — **5/10 groups**
`PathIn` xor `PathOut` cannot express read-then-rewrite. Caching must key on
*input-content + ops → output-content*, never on the mutated path.
- ar/ranlib; signtool; install_name_tool; codesign; mt; gofmt `-w`; go vet
  `-fix`. (Covered as Kind `PathInOut` but listed here because it breaks the
  cache-key *shape*, not just the kind enum.)

### B9. Leading selector / ambient state rebinds the binary — **3/10 groups**
`+toolchain`, xcode-select / `DEVELOPER_DIR`, `nightly`→today's build: neither
flag nor subcommand; rebinds *which executable runs* and resolves to mutable
ambient state.
- cargo/rustup `+nightly`, `rustup override` (keyed to cwd, no argv path); apple
  `xcrun`/`DEVELOPER_DIR`/`SDKROOT`; stack/cabal resolver names.

---

## 5. Verdict

**The model holds as an additive extension for (a) tokenizing argv into
structure and (b) per-flag canonicalization of the *simple majority* of flags.
It does NOT hold as stated for the cache-key contract.** Three of the four
structural assumptions are false and must be replaced, not extended:

1. **`subcommands: Map<String,Grammar>` (closed, bare-token dispatch) is
   wrong.** Dispatch happens on flags and flag-values (B1), and on *open* runtime
   edges into foreign grammars (B2). This requires **conditional productions**:
   the active grammar is a function of accumulated parse state. That is strictly
   more than a context-free recursive map — it is a small attribute grammar / a
   PEG with semantic predicates. **This is the one place the formalism must
   change shape, not just gain variants.**

2. **`Policy` as a per-flag enum is wrong.** B4 (cross-flag coupling) and B5
   (reduction over an open keyspace) mean canonicalization is a **graph
   reduction over the whole parsed command**, parameterized by user-supplied
   reducers and an alias/coupling graph — not a tag you read off one flag. Most
   of §2 fits as enum variants, but `LintLattice`, the warning algebra,
   `EditScriptOrdered`, `CrossFlagSet`, and `AliasGroup` are *functions*, not
   tags. Model `Policy` as `enum simple-cases + Reducer(fn)` with an explicit
   coupling layer.

3. **Canonical *argv* is not the cache key; the canonical *effective command* is
   (B3).** Env channels, config/lock files, response-file expansion, and
   discovered transitive inputs (B2/B3 + Kinds `EnvPath`, `ArgFileRef`,
   `IndirectInputs`, `Overlay`) must be first-class key inputs. The
   canonicalizer's output type is a resolved-input closure + a normalized argv,
   not a string.

Everything else — every Spelling in §1, most Policies in §2, every Kind in §3 —
is genuinely **additive**: more enum variants on the existing axes.
`Passthrough` must become recursive (`payload: Grammar`) to cover B6, but that
is a generalization of an existing axis, not a new formalism.

**Recommended shape:** keep `Grammar`/`Spelling`/`Kind` as the additive base;
(1) replace `subcommands: Map` with **guarded productions** (predicate over
accumulated state → grammar, including an `OpenForeign` edge); (2) split `Policy`
into per-flag *local* policies plus a *command-level reducer + coupling graph*;
(3) make the canonicalizer emit a **resolved effective-command closure** (argv +
env + expanded files + discovered inputs + impurity marks), not a canonical
argv. With those three changes the model is sound; without (1) and (2) it is not
even expressible.

---

## 6. Caching-hazard catalogue (must neutralize or mark impure)

The canonicalizer must, for every command, either **neutralize** the hazard
(inject a determinism flag / normalize the value) or **mark the command
impure** (key on inputs, never assert output-byte equality).

### Wall-clock / build-time stamps
- `__DATE__`/`__TIME__`/`__TIMESTAMP__` (cc, clang-cl) — honor `SOURCE_DATE_EPOCH`.
- PE `TimeDateStamp`, PDB age (cl/link) — require `/Brepro`.
- Mach-O `LC_UUID` from content+time (ld64) — `-no_uuid`/`ZERO_AR_DATE`.
- ELF `--build-id` entropy (ld) — `--build-id=none`.
- ar/ranlib/lib member mtime+uid+gid+mode — force deterministic mode (`ar D`,
  `ranlib -D`, `ZERO_AR_DATE`, `--enable-deterministic-archives`).
- go `-buildvcs` (VCS rev + dirty + commit time) — `=false`; `-ldflags -X`
  date/SHA injection is undetectable inside passthrough.
- WiX MSI build timestamp + auto GUIDs (`Id="*"`, PackageCode) — pin or impure.
- rc version-info build dates; mt resource rewrite changes timestamp/checksum.
- bindgen/cbindgen/swig/actool tool-version banners; cargo `--timings`.

### Host / machine / CPU dependence
- `-march=native`/`-Ctarget-cpu=native` — expand or mark host-impure.
- rc default codepage/langid from host locale — force `/c` and `/l`.
- `nightly`/`+toolchain`/xcode-select/`DEVELOPER_DIR`/`SDKROOT` resolve to
  ambient state — fold the *resolved* tool+SDK+version into the key.
- stack/cabal resolver names point at mutable snapshots until lock-pinned.
- reg reads pull live machine state; remote `\\machine\` paths host-dependent.
- landrun `--best-effort`/`--ldd` realize a different sandbox per kernel/host.

### Absolute-path leakage
- DWARF `comp_dir`/`DW_AT_*`, `__FILE__`, panic msgs (cc, as, rustc, ghc, go) —
  inject/verify `-fdebug-prefix-map`/`-ffile-prefix-map`/`--remap-path-prefix`/
  `-trimpath`/`--object-prefix-map`.
- pkg-config emits absolute `-I`/`-L` from `.pc`; cmake `CMakeCache.txt`
  records absolute compiler/source/build paths.
- PDB absolute paths (`/Zi`, `/DEBUG`) — `/PDBALTPATH:%_PDB%`.
- ld `-rpath`/`-soname` bake absolute non-relocatable paths into the binary.
- tsc `.tsbuildinfo`, sourcemaps `sourceRoot`, webpack `webpack://`, esbuild
  `--metafile`, ninja compdb, protoc `--dependency_out`, dsymutil OSO paths.
- sandbox profiles / firejail whitelists embed `$HOME`/`/Users/...`.

### Nondeterministic ordering / parallelism
- `-j`/`-p`/`-parallel`/`GOMAXPROCS` reorder diagnostics + stdout (make `-O`,
  ninja, cargo, go `-json`, xcodebuild) — *logs* nondeterministic, *artifacts*
  deterministic; strip logs from the artifact key, sort/normalize event streams.
- reg query output is enumeration-order-dependent — canonically sort.
- lipo universal member order, lib/ar member insertion order — fix arch order.
- ghc parallel `--make` reorders `-ddump-*`/interface writes.
- ghc `-frandom-seed`, clang `-frandom-seed`, rustc `-Cmetadata`/`-Cextra-filename`
  deliberately perturb symbol hashes — must be **in** the key (they affect output).

### Network / impure-by-construction
- codesign `--timestamp` (network TSA + random nonce) — fundamentally
  non-reproducible; `--timestamp=none` or impure.
- signtool `/tr`; pip `--index-url`/live PyPI; cargo/go/cabal/stack network
  resolution; npm `--before`/`--min-release-age` (time-dependent); yarn/pnpm
  registry; protoc plugin / bindgen libclang out-of-band binaries.
- powershell `-Command` with `Get-Date`/`iwr`/`[guid]::NewGuid()` — undetectable.

### Stateful / cross-run / temp state
- rustc `-Cincremental=DIR`, tsc `.tsbuildinfo`, vite `node_modules/.vite`,
  webpack `.cache`, ninja `.ninja_log`/`.ninja_deps`, cmake `--fresh`, go
  `-work/-x` random WORK dir, xcrun lookup cache — exclude dir from key or impure.
- make is mtime-driven (not content) — a content cache must ignore make's logic.
- bwrap `--tmpfs`/`--proc`/`--dev`, firejail `--private` — fresh random fs state.
- `PYTHONHASHSEED` randomizes dict/set ordering; `__pycache__` `.pyc` timestamps.

### Diagnostic-only flags (strip from artifact key)
- rustc `--error-format`/`--json`/`--color`/`-v`/`--diagnostic-width`; cc/go
  verbosity; anything that affects stderr but not artifacts.

### Secrets (redact, never hash verbatim into a shareable key)
- signtool `/p <password>`, `/f cert.pfx`; any credential-bearing argv token.

### Out-of-band binary identity (fold version+content into key)
- linker (`-Clinker`, `-pgml`), plugins (`protoc-gen-*`), libclang, build
  backends, `--with-compiler`, resolved `+toolchain`/SDK — the binary that runs
  is an input even when only its *name* is on argv.
