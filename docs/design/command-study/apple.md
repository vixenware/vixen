# Apple toolchain command study

Tool group: `xcrun`, `xcodebuild`, `codesign`, `lipo`, `libtool`, `install_name_tool`, `dsymutil`, `actool`.

Measured against the Vixen command model:

```
Grammar { flags: [Flag], positionals: [Positional], subcommands: Map<String,Grammar> }
Flag { lead, spelling: Spelling, policy: Policy, kind: Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough{sep}
Policy   = Once | Group{name} | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn{dir?} | PathOut | Include | Lib | Define
```

All flag inventories below are taken from the actual binaries shipped with Xcode (macOS 26.5, build 25F71; `xcrun version 72`, `actool` bundle 24765). Examples are real invocations.

The single most important cross-cutting finding: **the Vixen model assumes a GNU/Clang-shaped world (single leading char class, `-D`/`-I`/`-l` semantics, `-Wl,` as the only sub-language). The Apple toolchain violates this on almost every axis.** Three tools (`lipo`, `xcodebuild`, `codesign`) are not flag-and-positional grammars at all — they are *mode-dispatched* grammars where a single flag (`-create` vs `-thin`, `-s` vs `-v` vs `-d`) reconfigures which other flags are even legal. That is the headline model-breaker.

---

## 1. xcrun

`xcrun` is a *tool launcher / argv splitter*. It is the one tool whose entire job is the thing the Vixen model calls "passthrough into a nested grammar", but at the top level, not behind a flag.

### 1. Invocation shape
`xcrun [options] <tool name> ... arguments ...`. Depth = 2 but unbounded in spirit: `xcrun`'s own options stop at the first non-option token, which is the *tool name*; everything after is the child tool's argv passed verbatim. `xcrun clang -c foo.c -o foo.o` → run `clang`. `xcrun --sdk iphoneos --toolchain swift swiftc ...`.
- `Grammar.subcommands` *almost* covers it, but the child set is **open** (any tool on disk in the active developer dir), not a closed `Map`. The model needs an "open subcommand → opaque argv tail" escape: a subcommand whose grammar is `Passthrough(rest)`.

### 2. Spelling forms
- Bare: `-v`/`--verbose`, `-l`/`--log`, `-f`/`--find`, `-r`/`--run`, `-n`/`--no-cache`, `-k`/`--kill-cache`, `-h`, `--version`, and the `--show-sdk-*` family (`--show-sdk-path`, `--show-sdk-version`, `--show-sdk-build-version`, `--show-sdk-platform-path`, `--show-toolchain-path`).
- Separated: `--sdk <name>`, `--toolchain <name>`.
- **GNU double-dash long + single-dash short for the same flag** (`-v`/`--verbose`). The `Spelling` enum has no notion of *aliases mapping to one canonical flag* — see §3 last-wins-across-aliases.
- No equals, no attached, no count, no `@argfile`. But the **end-of-own-options boundary is positional, not `--`**: the first bare word is the tool name and silently terminates xcrun option parsing. That's a 6th spelling-context the enum lacks: "implicit end-of-flags at first positional, rest is opaque."

### 3. Multiplicity & order
- `--sdk` / `--toolchain`: `Once` (last-wins).
- The `--show-*` queries are mutually exclusive *actions* — really a `Group`, but the group selects a whole different output, not a flag value.
- `-f`/`-r`/`-l` interact: `-f` (find only) vs `-r` (run, default) is a `Group`.

### 4. Path-bearing slots
- `--sdk` takes a *name OR a path* (`iphoneos` or `/path/to/SDK`). Ambiguous Str-vs-PathIn → a **new Kind `NameOrPath`** (resolved against a search root). `--toolchain` same.
- The tool name resolves to a path via `DEVELOPER_DIR` env + on-disk search. **The path comes from the environment, not argv** → caching hazard (§6).

### 5. Model-breakers
- The child argv is an *entire other grammar*, chosen at runtime. This is recursion the model has but with an **open, filesystem-derived key set**.
- `DEVELOPER_DIR` / `--sdk` change which binary the child name resolves to — the *meaning of a positional* depends on out-of-band state.

### 6. Caching hazards
- `DEVELOPER_DIR`, `SDKROOT`, active `xcode-select` dir all leak into resolution: identical argv, different tool. The canonicalizer must fold the *resolved tool path + version* into the key, not the bare name.
- `-n`/`--no-cache`, `-k`/`--kill-cache` mutate xcrun's own lookup cache — observable side effects, must mark impure / ignore.

---

## 2. xcodebuild

The deepest, most model-hostile grammar in the group. It is a **mode-dispatched multigrammar** keyed by a *build action* (positional) AND by which "verb flag" is present.

### 1. Invocation shape
No subcommand tree in the bare-token sense. Instead:
- **Build actions are trailing bare positionals**, repeatable & ordered: `clean`, `build`, `test`, `archive`, `analyze`, `install`, `installsrc`, `build-for-testing`, `test-without-building`, `docbuild`. e.g. `xcodebuild clean build`, `xcodebuild build test`.
- **Verb flags select entirely different usages** (depth-1 "subcommands" spelled as flags): `-list`, `-showsdks`, `-showBuildSettings`, `-version`, `-exportArchive`, `-exportLocalizations`, `-importLocalizations`, `-resolvePackageDependencies`, `-create-xcframework`, `-downloadPlatform`, `-importComponent`, etc. Each enables/requires a different flag subset (`-exportArchive` requires `-archivePath` + `-exportOptionsPlist`; `-create-xcframework` takes repeated `-framework`/`-library`).
- Deepest nesting: `-create-xcframework -library <path> -headers <path> ... -output <path>` — the `-headers` flag is only legal *immediately after* a `-library` (it modifies the preceding `-library`), a positional-adjacency dependency.

`Grammar.subcommands` does **not** cover flag-spelled mode selection, and definitely not adjacency-scoped sub-flags.

### 2. Spelling forms
- **All long flags use a single leading dash** (`-project`, `-scheme`, `-configuration`, `-destination`) — this is the classic NeXT/`getopt_long_only` style. The Vixen `lead` field must allow `-longword`; many parsers special-case `--`.
- Separated: `-project NAME`, `-scheme NAME`, `-sdk SDK`, `-arch ARCH`, `-destination SPEC`, `-derivedDataPath PATH`, `-resultBundlePath PATH`, `-xcconfig PATH`, `-jobs NUMBER`.
- **Colon-suffixed flags** (a sub-language!): `-only-testing:TEST-IDENTIFIER`, `-skip-testing:TEST-IDENTIFIER`. The identifier rides on the flag name after a `:`. This is neither `Equals` (`=`) nor `Attached` — it's a **`Colon`-separated value**. New spelling.
- **Free-form `KEY=value` build-setting assignments as positionals**: `xcodebuild SYMROOT=/tmp/build ONLY_ACTIVE_ARCH=YES build`. These look like `Equals` flags but have *no declared flag* — arbitrary key. New concept: **open key=value positional namespace**.
- `YES|NO` enum flags: `-parallel-testing-enabled YES|NO`, `-enableCodeCoverage YES|NO`, `-enableAddressSanitizer YES|NO`.
- `-json` as an optional modifier appearing after several different verbs (`-list -json`, `-showsdks -json`, `-version -sdk ... -json`).
- No `@argfile`, no count flags, no `/FLAG`.

### 3. Multiplicity & order
- `-arch ARCH`: repeatable, **order significant for fat output** → `DedupOrdered`.
- `-target`, `-only-testing:`, `-skip-testing:`, `-destination`: repeatable accumulate.
- `-only-testing:` vs `-skip-testing:` are **accumulate-with-mutual-negation** (an allow-list and a deny-list applied together) — the model's `DedupUnordered` cannot express two linked sets with opposite polarity. Similar to `-Wfoo`/`-Wno-foo`. **Missing policy: `AccumPolarized{positive, negative}`.**
- Build-action positionals: ordered, *not* deduped (`clean build clean` is meaningful) → `Positional`.
- `-configuration`, `-scheme`, `-project`, `-workspace`: `Once`.
- `KEY=value` overrides: last-wins per key (`DedupUnordered` over keys, but value matters) → really a *map*, not a set.

### 4. Path-bearing slots
- PathIn: `-project`, `-workspace`, `-xcconfig`, `-archivePath` (in export mode), `-exportOptionsPlist`, `-xctestrun`, `-importPath`, `-localizationPath`.
- **PathOut DIRS** (not files): `-derivedDataPath`, `-resultBundlePath`, `-exportPath`, `-clonedSourcePackagesDirPath`, `-testProductsPath`, `SYMROOT=`/`OBJROOT=`/`DSTROOT=` build settings. `Kind::PathOut` exists but **the dir-vs-file distinction on output is unmodeled** (`PathIn` has `dir?` but `PathOut` does not). **Missing: `PathOut{dir?}`.**
- `-resultStreamPath PATH` — output path that **must already exist** (pre-created file written into). A PathOut with a precondition the cache can't satisfy.
- `-sdk` is NameOrPath again.
- `-framework`/`-library`/`-headers` (xcframework) are PathIn.

### 5. Model-breakers
- **Verb-flag mode dispatch**: `-exportArchive` vs `-list` vs `-create-xcframework` make completely different flag sets legal. No single flat `[Flag]` list is valid; you need *conditional grammars selected by a flag*.
- **Adjacency-scoped flags**: `-headers` binds to the preceding `-library`. Meaning depends on a prior flag's presence/position.
- **Open `KEY=value` positional namespace** — arbitrary build settings, no closed schema.
- `-destination` value is itself a **comma-separated key=value mini-language** (`-destination 'platform=iOS Simulator,name=iPhone 15,OS=17.0'`) — a sub-language beyond `-Wl,`.

### 6. Caching hazards
- `-jobs`, `-parallelizeTargets`, `-parallel-testing-worker-count` → parallelism changes output *ordering* and log interleaving; build product should be order-independent but logs are not.
- `-derivedDataPath` default is `~/Library/Developer/Xcode/DerivedData/<hash>` — absolute, user/time/uuid-derived → must be normalized.
- `-resultBundlePath` / xcresult embeds **timestamps, durations, absolute paths, device UDIDs**.
- Build settings inject `$(SRCROOT)`, `BUILD_DIR`, timestamps into products; `xcodebuild` is essentially impure end-to-end. Honest answer: mark as impure unless run in a fully hermetic sandbox with `-derivedDataPath` pinned and clock frozen.

---

## 3. codesign

A **mode-dispatched grammar by leading verb flag**: `-s`/`--sign` (sign), `-v`/`--verify`, `-d`/`--display`, `-h` (hosting), `--validate-constraint`. Each mode legalizes a different flag set.

### 1. Invocation shape
Flat: `codesign <mode-flag> [options] path...`. No bare subcommands. Mode is chosen by which of `-s/-v/-d/-h` appears. Trailing positionals are `path...` (or `[+]pid...` in verify/hosting mode — a positional whose *type* depends on mode).

### 2. Spelling forms
- Bare: `-f`/`--force`, `--deep`, `--continue`, `--all-architectures`, `--generate-entitlement-der`, `--force-library-entitlements`, `--detached-database`.
- Separated: `-s identity`, `-i`/`--identifier ident`, `-r`/`--requirements reqs`, `-D`/`--detached file`, `--entitlements path`, `--keychain file`, `--prefix string`, `-R`/`--test-requirement req`, `--runtime-version version`, `-a`/`--architecture arch`.
- **Equals-optional flags**: `--timestamp[=URL]` (bare = use default TSA; `--timestamp=http://...` = explicit; or `--timestamp=none`). The value is *optional*; `Equals` in the model assumes a value is always present. **Missing: `Equals{optional}`** / a "bare-or-equals" spelling.
- `--strict` / `--strict=all` — same optional-equals shape.
- **Comma-list value**: `-o`/`--options flag,flag,...` (e.g. `-o runtime,library,kill,hard`). And `--preserve-metadata=identifier,entitlements,flags,...`. This is the `-Wl,a,b` *value shape* but **not** a passthrough into another argv — it's an unordered **set of named option flags**. The model's `Passthrough{sep}` implies a nested *grammar*; here it's a flat dedup-set. **A comma-set value kind is missing** (closest: `DedupUnordered`, but applied to a single flag's value, not repeated flags).
- `-v` is **count-like** in verify/display mode: more `-v` = more verbosity (`-vvv`), but spelled as repeats not a single attached run. The man page describes "increasing levels of verbosity." → count semantics.
- `-R=<req>` OR `-R <reqfile>`: `-R=` inline string vs `-R` separated *path* — same flag, two kinds depending on `=`.

### 3. Multiplicity & order
- `-o`/`--options`: set of named flags, unordered, dedup → `DedupUnordered` *over the comma-list*.
- `-r` can be specified per-requirement; multiple `--strict` accumulate.
- `-s`, `-i`, `--keychain`, `--timestamp`: `Once`.
- `-v` repeated: **count** policy — not in model.
- The verb flags `-s/-v/-d/-h`: `Group` (mutually exclusive mode).

### 4. Path-bearing slots
- PathIn: `--entitlements path`, `--keychain file`, `-R <reqfile>`, `-r` can reference a file (`-r @file`).
- PathOut: `-D`/`--detached filename`.
- `--requirements` / `-r` value may be an **inline requirement-language string OR `@/path`** — a sub-language *or* a path. Another NameOrPath/Str-or-Path ambiguity.
- Positional `path...` are PathIn that get *mutated in place* (signature embedded) → both PathIn and PathOut simultaneously. The model has no "in-out path."

### 5. Model-breakers
- **Mode dispatch by `-s` vs `-v` vs `-d`** — conditional grammar, same problem as xcodebuild.
- Positional kind depends on mode (`path` vs `[+]pid`). A `+pid` prefix selects "guest" — sub-syntax on a positional.
- `-r`/`-R` carry the **Code Signing Requirement Language** (`anchor apple and identifier "com.x"`) — a full embedded DSL, far beyond `-Wl,`.
- `--preserve-metadata` makes the *output a function of the existing signature on disk*, not just argv — meaning depends on prior file state.

### 6. Caching hazards
- `--timestamp` contacts a **network TSA**; embeds the *current wall-clock time* into the signature → nondeterministic by construction. Must canonicalize to `--timestamp=none` for reproducibility or mark impure.
- The CMS signature embeds a random nonce / cert chain; the signed binary's bytes differ run-to-run even with identical inputs. Code signing is **fundamentally non-reproducible**; the cache key can cover *inputs* but not assert output-byte equality.
- `--keychain` / default keychain search pulls identity material from out-of-band state.

---

## 4. lipo

The cleanest example of **command-dispatch-by-flag** with an unusual single-token positional rule.

### 1. Invocation shape
`lipo <input_file> <command> [<options>...]`. Exactly one *command* required from: `-create`, `-thin <arch>`, `-extract <arch>`, `-extract_family <arch>`, `-remove <arch>`, `-replace <arch> <file>`, `-archs`, `-info`, `-detailed_info`, `-verify_arch <arch>...`. Mode-dispatched (one command per invocation), no nesting.

### 2. Spelling forms
- All flags single-dash long (`-create`, `-output`, `-segalign`).
- Bare command: `-create`, `-archs`, `-info`, `-detailed_info`, `-hideARM64`, `-fat64`.
- **Single-token separated** `-thin arm64`, `-remove i386`.
- **Two-token separated** `-replace arm64 lib_arm64.o`, `-segalign arm64 0x4000`, `-arch arm64 input.o` — a flag that consumes **two** following tokens. The `Separated` spelling implies *one* value token. **Missing: `SeparatedN{n}` / multi-arity separated.**
- `-verify_arch arch...` consumes a **variadic trailing list** until end of args. Variadic separated — also unmodeled.
- No equals, no attached, no `@file`.

### 3. Multiplicity & order
- `-extract`, `-remove`, `-replace` repeatable, each adds an arch operation. Order between different commands isn't allowed (one command), but repeats accumulate.
- `-arch arch input` repeatable — declares per-input architecture.
- `-output`: `Once`.
- `-create` inputs: ordered positionals; **output arch order follows input order** → `Positional` (never reorder).
- `-s`/`-c`/`-a` (in libtool sense) — n/a here. `-segalign` repeatable per arch → keyed map.

### 4. Path-bearing slots
- Positional inputs: PathIn (multiple). `-replace arch <file_name>`: the second token is PathIn. `-output <file>`: PathOut. `-arch <arch> <input_file>`: second token PathIn.
- `Kind` covers PathIn/PathOut; **but the path is the *second* of a two-token flag** — coupling Kind to spelling-arity is unmodeled.

### 5. Model-breakers
- Two-token and variadic flag values (`-replace`, `-segalign`, `-verify_arch`).
- The required-command-out-of-a-set is `Group` but with the twist that the group members have *different arities and different following grammars*.
- `arch_type` values (`arm64`, `arm64e`, `x86_64`, `armv7`, `i386`, `arm64_32`) are a closed enum the canonicalizer should validate/normalize (e.g. `-archs` output order).

### 6. Caching hazards
- Largely deterministic. `-segalign` defaults and `-hideARM64` change output bytes deterministically (fine).
- Universal-file member *order* and padding can vary if inputs given in different order; canonical form should fix arch order (e.g. ascending CPU type) for the cache key even though `lipo` itself preserves input order.

---

## 5. libtool

(Apple's `libtool` — static-archive builder, *not* GNU libtool.) Mode-dispatched: `-static` is required and selects "create static library."

### 1. Invocation shape
`libtool -static [options]* <mach-o file>+ -o <file>`. Flat, no nesting. `-static` is the (currently sole) mode selector — but the design clearly anticipates other modes (it is `Group`-shaped).

### 2. Spelling forms
- Bare: `-static`, `-toc64`, `-c`, `-s`, `-a`, `-D`, `-V`, `-no_warning_for_no_symbols`, `-encode_sdk_libraries_as_references`.
- Separated: `-arch_only <arch>`, `-o <file>`, `-filelist <file>`, `-dependency_info <file>`, `-ref-framework <Foo>`.
- **Attached**: `-ref-l<foo>` (e.g. `-ref-lcrypto`) — the lib name is glued to the flag, exactly like clang `-l`. This is `Attached` and maps to `Kind::Lib`-ish but as an *auto-link hint*, not a link input.
- `-D` here means **"deterministic / zero archive mtime"** — *collides in spelling with clang's `-D<macro>` define*. A cross-tool reminder that `-D` is not universally `Define`. The Vixen `Kind::Define` must not be inferred from the `-D` lead alone.

### 3. Multiplicity & order
- Input `.o`/`.a` files: ordered positionals, **archive member order is preserved** unless `-s` (sorted TOC) → `Positional`.
- `-s` (sorted TOC, default) vs `-a` (unsorted) vs `-c`: a `Group`.
- `-ref-l<foo>` / `-ref-framework`: accumulate, dedup-ordered (auto-link hints) → `DedupOrdered`.
- `-o`, `-arch_only`: `Once`.

### 4. Path-bearing slots
- PathIn: positional `.o`/`.a` files, `-filelist <file>` (**a response-file: a file whose contents are *more input paths***).
- PathOut: `-o <file>`, `-dependency_info <file>`.
- `-filelist` is the key one: **a `@argfile`-equivalent path whose *contents* are inputs that must be hashed transitively.** `Kind` has nothing for "path that expands to more paths." **Missing: `Kind::FileList` (response file) — must be read and its referents content-hashed.**

### 5. Model-breakers
- `-D` semantic collision (deterministic vs define) — the model can't infer `Kind` from lead char.
- `-filelist` indirection — argv content lives in a file.

### 6. Caching hazards
- **`-D` exists precisely to *fix* the nondeterminism**: without it, archives embed member **mtimes**. Canonicalizer should force/assume `-D` semantics (zeroed mtime) or the archive bytes vary by checkout time. Strong example of "built-in nondeterminism the canonicalizer must neutralize."
- TOC ordering (`-s` vs `-a`) affects bytes deterministically.

---

## 6. install_name_tool

A pure **mach-o rewriter** with repeated paired-argument flags. No modes, no positionals beyond the single trailing input (which it mutates in place).

### 1. Invocation shape
`install_name_tool [-change old new]... [-rpath old new]... [-add_rpath new]... [-delete_rpath old]... [-id name] input`. Flat. Single trailing positional `input` (in-out path). No nesting.

### 2. Spelling forms
- **Two-token separated, repeatable**: `-change old new`, `-rpath old new`. Same two-token problem as `lipo -replace`.
- Single-token separated: `-add_rpath new`, `-delete_rpath old`, `-id name`.
- No equals/attached/argfile.

### 3. Multiplicity & order
- `-change`, `-rpath`, `-add_rpath`, `-delete_rpath`: **all repeatable and ORDER-SENSITIVE** — `-add_rpath A -add_rpath B` produces LC_RPATH in that order; a later `-change` can target the result of an earlier one. Cannot dedup, cannot reorder → `Positional`-like policy applied to *flags* (not just positional inputs). The model's `Positional` policy is conceptually right but it's named/scoped to inputs only. **Need: `OrderedAccum` (repeatable flag, order is semantic, no dedup).**
- `-id`: `Once`.

### 4. Path-bearing slots
- The *values* are install-names / rpaths which are **path-like strings embedded in the binary** (`@rpath/Foo.framework/Foo`, `@loader_path/../lib`, absolute paths). They are simultaneously `Str` (literal, must be preserved verbatim — `@rpath` is not a filesystem path to hash) AND path-shaped. **A path that must NOT be mounted/hashed** — opposite of `PathIn`. The model would wrongly try to content-hash `@rpath/...`. **Missing: a "logical path / opaque string that looks like a path but is literal" distinction.**
- `input`: in-out PathIn+PathOut (mutated in place).

### 5. Model-breakers
- Order-sensitive repeatable flags (above).
- `@rpath`/`@loader_path`/`@executable_path` runtime placeholders — values that resemble paths but are runtime tokens, must stay literal.

### 6. Caching hazards
- In-place mutation: the same `input` path is read and rewritten. A content-addressed cache must treat input-content + ops → output-content, not key on the path.
- Otherwise deterministic.

---

## 7. dsymutil

The most GNU/LLVM-conventional tool here (it's an LLVM tool), and the best fit for the model — yet still exposes alias and reproducer issues.

### 1. Invocation shape
`dsymutil [options] <input files>`. Flat, no subcommands. Multiple trailing positional inputs.

### 2. Spelling forms
- **GNU double-dash long + single-dash short, heavily aliased**: `--flat`/`-f`, `--help`/`-h`, `--num-threads`/`-j`, `--update`/`-u`, `--quiet`/`-q`, `--symtab`/`-s`, `--version`/`-v`, `--out`/`-o`, `--verify-dwarf=output`/`--verify`. Many short forms are documented as "Alias for --long." The model has **no first-class alias→canonical mapping**; canonicalization-to-one-spelling requires it.
- Separated: `--arch <arch>`, `-o <file>`, `-D <path>`, `--oso-prepend-path <path>`, `--num-threads <n>`, `--toolchain <name>`, `--accelerator <type>`, `--reproducer <mode>`, `--use-reproducer <path>`, `--verify-dwarf <mode>`.
- **Equals on long forms**: `--verify-dwarf=output`, `--reproducer=GenerateOnExit`.
- **`key=value` value-attached** flags: `--object-prefix-map <prefix=remapped>`, `--build-variant-suffix <suffix=buildvariant>`, `-cas-plugin-option <Option>=<Value>`. The *value itself* is a `k=v` pair (a remap). Sub-structure inside the value, like xcodebuild `-destination`.
- Enum-valued: `--accelerator Apple|Dwarf|Pub|Default|None`, `--verify-dwarf none|input|output|all|auto`, `--reproducer GenerateOnExit|GenerateOnCrash|Off`.

### 3. Multiplicity & order
- `--arch`: repeatable, "once for each desired architecture" → set, order not strictly semantic → `DedupUnordered` (or `DedupOrdered` to be safe).
- `-D <path>` (dSYM search dir): repeatable, search-path → `DedupOrdered` (search order matters), like `-I`. **This `-D` means a search directory, not a define** — second tool where `-D` ≠ Define (cf. libtool).
- `--object-prefix-map`: repeatable, **order can matter** (first match wins in remap) → `DedupOrdered`.
- `-o`, `--num-threads`, `--accelerator`, `--linker`: `Once`/`Group` (`--accelerator` is last-wins enum).
- `--verify-dwarf` vs `--verify` alias both write the same slot → last-wins-across-aliases.

### 4. Path-bearing slots
- PathIn (search dir): `-D <path>` → `Include`-like (dir search). `--oso-prepend-path`, `--remarks-prepend-path`, `--object-prefix-map` (the `remapped` half is a path prefix).
- PathOut: `-o`/`--out <file>`.
- `--use-reproducer <path>`: PathIn dir (a recorded bundle of object files).
- `-cas <path>`, `-cas-plugin-path <path>`: PathIn (content-addressed store dir + plugin lib).
- Implicit inputs: dsymutil reads the **`.o` files referenced by the executable's debug map** — paths *not on the command line*. Cache key must hash the referenced object files (found via OSO entries), an entire indirect dependency set. `Kind` cannot express "input is a graph rooted at the positional."

### 5. Model-breakers
- Implicit object-file fan-out (debug map → many `.o`). Same class as libtool `-filelist` but *discovered from the binary*, not a flag.
- `key=value`-valued flags (`--object-prefix-map`) — sub-structure in the value.

### 6. Caching hazards
- **`--no-object-timestamp`, `--no-swiftmodule-timestamp`, `--no-output`, `--no-odr`** exist because the *default* behavior checks object **timestamps** — timestamp-sensitivity is built in. For caching, prefer/force `--no-object-timestamp` so mtimes don't perturb the result.
- `--reproducer GenerateOnExit` / `--gen-reproducer` write a reproducer bundle (side effect + absolute paths embedded).
- `--num-threads`/`-j` parallelism — output should be deterministic but verifier/stat output ordering is not.
- dSYM bundle can embed `--toolchain` info and absolute OSO paths → absolute-path leakage; `--oso-prepend-path`/`--object-prefix-map` are the normalization knobs the canonicalizer should drive.

---

## 8. actool

The asset-catalog compiler. Long `--` flags, **plist-on-stdout** I/O, and the heaviest "value semantics depend on other flags" coupling.

### 1. Invocation shape
`actool [options] document`. Flat (one trailing positional: the `.xcassets` document, or multiple). The *action* is selected by presence of `--compile <dir>` (compile mode) vs nothing (verify/print mode) — soft mode dispatch. No nesting.

### 2. Spelling forms
- **GNU `--` long, separated**: `--compile <dir>`, `--output-format <fmt>`, `--platform <name>`, `--minimum-deployment-target <ver>`, `--app-icon <name>`, `--output-partial-info-plist <path>`, `--development-region <code>`, `--sticker-pack-strings-file <file>`, `--include-language <lang>`, `--asset-pack-output-directory <dir>`.
- Bare: `--warnings`, `--errors`, `--notices`, `--compress-pngs`, `--include-all-app-icons`, `--enable-on-demand-resources`, `--skip-app-store-deployment`.
- Enum-valued: `--output-format binary1|xml1|human-readable-text`, `--include-partial-info-plist-localizations yes|no`.
- **Errors reported as a `plist` on stdout, not exit-code/stderr** (`--help` itself returns a plist error!). Not a spelling issue but means the *grammar's own diagnostics are structured data* — unusual.
- No equals, no attached, no argfile, no count.

### 3. Multiplicity & order
- `--target-device <name>`: repeatable ("may be passed multiple times": `iphone`, `ipad`, `mac`, `watch`, `tv`) → `DedupUnordered`.
- `--include-language <lang>`: repeatable accumulate → `DedupUnordered`.
- `--alternate-app-icon <name>`: repeatable accumulate.
- `--app-icon`, `--accent-color`, `--widget-background-color`: `Once`.
- `--compile`, `--output-format`, `--platform`, `--minimum-deployment-target`: `Once`.

### 4. Path-bearing slots
- PathIn: positional document(s) (`.xcassets` **directory** — a dir input!), `--sticker-pack-strings-file <file>`.
- PathOut DIR: `--compile <dir>` (writes `Assets.car` *into* the dir — output is a dir, filename is implicit/fixed), `--asset-pack-output-directory <dir>`. Again the **`PathOut{dir?}` gap**: output is a directory, the real artifact name (`Assets.car`) is chosen by the tool.
- PathOut file: `--output-partial-info-plist <path>`.
- The `.xcassets` positional is a **directory tree** that must be hashed recursively (every image/json inside) → `Kind::PathIn{dir: true}` covers it *if* "dir" means "hash the whole subtree", which the model should make explicit.

### 5. Model-breakers
- Soft mode dispatch by `--compile`.
- **Cross-flag value coupling**: `--app-icon`'s behavior (CAR-embedded vs copied) depends on `--minimum-deployment-target`; `--include-all-app-icons` vs `--alternate-app-icon` are alternatives; `--skip-app-store-deployment` toggles which validations/assets `--platform` triggers. The *effect* of one flag is a function of several others — beyond per-flag policy.
- Output filename is fixed by the tool (`Assets.car`), not given on argv — the model assumes outputs are named in argv.

### 6. Caching hazards
- `--compress-pngs` runs `pngcrush` — should be deterministic but is an external tool version dependency.
- CAR files can embed build-environment metadata; generally reproducible if `actool` version + inputs pinned (fold bundle version 24765 into the key).
- The partial Info.plist merges discovered values — deterministic given inputs.

---

## Cross-tool synthesis (what the model must grow)

### New Spellings needed
- `SeparatedN{n}` / variadic-separated — `lipo -replace arch file`, `-segalign arch val`, `-verify_arch a b c`; `install_name_tool -change old new`, `-rpath old new`. The `Separated` enum assumes exactly one value token.
- `Colon` — `xcodebuild -only-testing:ID`, `-skip-testing:ID`. Value rides after `:` on the flag name.
- `EqualsOptional` (bare-or-`=value`) — `codesign --timestamp[=URL]`, `--strict[=all]`; dsymutil `--verify`(=output).
- `SingleDashLong` lead class — xcodebuild/lipo/libtool/install_name_tool all use `-longword` (getopt_long_only). The `lead` field must not assume `--` for multi-char.
- Implicit-end-of-flags-at-first-positional with opaque tail — `xcrun <tool> <rest...>`.
- Open `KEY=value` positional namespace — xcodebuild build settings.

### New Policies needed
- `OrderedAccum` (repeatable, order semantic, no dedup) — `install_name_tool` rpath ops, `dsymutil --object-prefix-map`. (`Positional` is the right idea but is scoped to inputs.)
- `AccumPolarized{positive, negative}` — `xcodebuild -only-testing:`/`-skip-testing:`; the `-Wfoo`/`-Wno-foo` shape the prompt flags; linked allow/deny sets.
- `Map{key}` (last-wins per key) — xcodebuild `KEY=value`, comma-option sets.
- `CommaSet` value policy — `codesign -o runtime,library,kill`, `--preserve-metadata=a,b,c` (dedup-set *inside one flag value*, not repeated flags).
- `Count` — `codesign -v -v -v`, xcrun/xcodebuild verbosity.
- First-class `alias → canonical` so dsymutil `-f`==`--flat`, `--verify`==`--verify-dwarf=output` canonicalize to one spelling.

### New Kinds needed
- `NameOrPath` (resolved against a search root) — `--sdk iphoneos`-or-path, `--toolchain`, codesign `-r @file`-or-string.
- `PathOut{dir?}` — output is a *directory* with a tool-chosen filename: `actool --compile`, xcodebuild `-derivedDataPath`/`-resultBundlePath`/SYMROOT, lipo n/a.
- `FileList`/`ResponseFile` — `libtool -filelist`; contents are more input paths, hash transitively.
- `DebugMapFanout` / `IndirectInputs` — `dsymutil` reads `.o` files discovered from the binary, not argv.
- `LogicalPath` (literal, do NOT mount/hash) — `install_name_tool` `@rpath/...`, `@loader_path/...`; looks like a path, is a runtime token.
- `InOutPath` — `codesign path...`, `install_name_tool input` mutate the file in place (PathIn + PathOut on one slot).
- `Requirement`/embedded-DSL — codesign `-r`/`-R` requirement language; `-destination` k=v mini-language; `--object-prefix-map` k=v.

### Genuine model-breakers (cannot be a flat `[Flag]` + per-flag policy)
1. **Mode dispatch by flag, not bare token** — `lipo` (`-create`/`-thin`/...), `codesign` (`-s`/`-v`/`-d`/`-h`), `xcodebuild` (`-list`/`-exportArchive`/...), `libtool` (`-static`), `actool` (`--compile`). Each mode legalizes a *different flag set*. `Grammar.subcommands` only dispatches on bare tokens.
2. **Adjacency-scoped sub-flags** — `xcodebuild -create-xcframework -library X -headers H`: `-headers` modifies the preceding `-library`.
3. **Conditional/coupled flag semantics** — `actool --app-icon` effect depends on `--minimum-deployment-target`; `codesign --preserve-metadata` makes output depend on prior on-disk signature; `--runtime-version` only meaningful when `runtime` is in `-o`.
4. **Positional kind depends on mode** — `codesign path...` vs `[+]pid...`.
5. **Value sub-languages beyond `-Wl,`** — codesign requirement DSL, `-destination` k=v list, `--object-prefix-map` remaps.

### Caching hazards (canonicalizer must neutralize or mark impure)
- **Wall-clock / TSA**: `codesign --timestamp` (network + current time embedded); CMS nonce makes signed output non-reproducible by design.
- **Archive mtimes**: `libtool` embeds member mtimes unless `-D`; force `-D`.
- **Object timestamps**: `dsymutil` checks `.o` mtimes by default; force `--no-object-timestamp`.
- **Absolute path leakage**: OSO paths in dSYM (`--oso-prepend-path`/`--object-prefix-map` to normalize); `-derivedDataPath`/DerivedData hashes; xcresult bundles embed paths/UDIDs/timestamps.
- **Env-derived tool resolution**: `DEVELOPER_DIR`/`SDKROOT`/`xcode-select` change which binary `xcrun` runs — fold resolved tool path+version into key.
- **Parallelism ordering**: `xcodebuild -jobs`, `dsymutil --num-threads` — output bytes should be order-stable but logs/stats/verifier output are not.
- **Reproducer/cache side effects**: `xcrun -n/-k`, `dsymutil --gen-reproducer/--reproducer`.
- **Tool version**: fold `xcrun 72` / `actool 24765` / Xcode build `25F71` into keys; output format and embedded metadata vary by version.
