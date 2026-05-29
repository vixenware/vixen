# MSVC tool group — CLI grammar study

Tools covered: `cl.exe`, `link.exe`, `lib.exe`, `clang-cl`, `rc.exe`, `mt.exe`, `dumpbin`.

Model under test:

```
Grammar { flags: [Flag], positionals: [Positional], subcommands: Map<String,Grammar> }
Flag { lead, spelling: Spelling, policy: Policy, kind: Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

The MSVC family is the adversarial case for any Unix-shaped grammar: it has TWO lead
characters (`/` and `-`) that are *interchangeable* per flag, a colon-as-separator
convention that is sometimes-separated/sometimes-attached, case-insensitive option
names, options whose argument is glued with NO delimiter at all (`/Fooutput.obj`),
and several tools that are really one binary multiplexing on a *leading* option rather
than a subcommand token. `dumpbin`/`lib`/`editbin` are all `link.exe` wearing hats.

---

## 0. Cross-cutting facts that stress the model

- **Dual lead, interchangeable.** Every native MSVC option accepts both `/` and `-`:
  `/c` == `-c`, `/Fo` == `-Fo`, `/OUT:x` == `-OUT:x`. The model's `Flag.lead` is a
  single value; here lead is a *free variant* that must canonicalize (pick one, e.g. `/`).
- **Case-insensitive option NAMES, case-sensitive VALUES.** `/NOLOGO` == `/nologo` ==
  `/Nologo`; but the path in `/Fo:Foo.obj` keeps its case. cl's source-file *letters*
  are case-sensitive though (`/Fo` object vs `/Fa` asm vs `/Fe` exe vs `/FA` listing-content):
  the canonical name cannot be naively lowercased.
- **Colon separator that is really "attached, delimited by `:`"**: `/OUT:foo.exe`,
  `/MACHINE:X64`, `/SUBSYSTEM:CONSOLE`. No space allowed around `:`. This is neither
  `Equals` (`=`) nor `Separated` (space) — it's a third delimiter the enum doesn't name.
- **Glued-no-delimiter**: `/Fooutput.obj` (= `/Fo` + `output.obj`), `/Iinclude\dir`,
  `/DNAME=val`, `/link`-tail. This *is* `Attached`, but attached value boundaries are
  defined by the option spelling table, not by a delimiter — the parser needs the table.
- **`@response-file`** on every tool. No space between `@` and filename. Recursive
  (a response file may contain `@other`). cl/link tokenize response files with the
  same quoting rules as the shell-expanded command line.

---

## 1. `cl.exe` (the C/C++ compiler driver)

### 1. Invocation shape
Flat — no subcommands. `cl [options] file... [/link link-options]`. The deepest
"nesting" is the `/link` sentinel: every token AFTER `/link` is handed verbatim to
`link.exe`, i.e. an embedded sub-grammar (link.exe's grammar) reached by a *flag*, not
a bare token. `Grammar.subcommands` (dispatch on a bare token) does **not** model this;
it's a flag-introduced passthrough into another whole Grammar. Depth = 2 (cl → link).

### 2. Spelling forms actually used
- **Bare**: `/c` (compile only), `/nologo`, `/EHsc`, `/MT`, `/MD`, `/LD`, `/Zi`, `/W4`.
- **Attached / glued-no-delim**: `/Fooutput.obj`, `/Femyprog.exe`, `/Faasm.asm`,
  `/Fdfoo.pdb`, `/Iinc\path`, `/DFOO`, `/DFOO=1`, `/UFOO`, `/Yustdafx.h`.
- **Colon-attached** (delimiter `:`): `/Fo:out.obj` (newer cl accepts the colon form),
  `/analyze:...`, `/diagnostics:caret`, `/std:c++20`, `/arch:AVX2`, `/permissive-`.
- **Equals inside attached value**: `/DNAME=value` — `=` is *inside* the define payload,
  not the option/value separator. `/D` is Attached; the `=` belongs to the macro syntax.
- **Trailing `-` as a modifier**: `/permissive-`, `/GR-`, `/Gm-`, `/Zc:wchar_t-`. The `-`
  suffix flips a boolean. This is neither a value nor a lead — it's a per-flag negation
  marker glued to the name.
- **Count-ish**: warning levels `/W0 /W1 /W2 /W3 /W4 /Wall` are a numbered family
  (Group), not a repeat-count; `/wd4996` (disable warning N), `/we4996` (treat-as-error N),
  `/wo4996`, `/w14996` (set warning N to level 1) — these *accumulate*.
- **Response files**: `cl @args.rsp foo.c`.
- **Passthrough**: `/link <link-opts...>` (to EOL — consumes the remainder).
- `--` end-of-flags: **not honored** by native cl (a bare `--` would be treated as a
  malformed option). Only clang-cl honors `--`.

### 3. Multiplicity & order semantics → Policy
- `Group`: optimization `/Od /O1 /O2 /Ox /O0(n.a.)` and the `/O` sub-letters
  (`/Os` size, `/Ot` speed, `/Oy` fp-omit) interact; runtime-lib `/MT /MTd /MD /MDd /LD`
  are mutually exclusive (Group, last-wins). Warning level `/W0../W4/Wall` Group.
- `DedupUnordered`: `/D` defines behave set-like for the *names*, BUT `/DFOO=1 /DFOO=2`
  is **last-wins** (later redefine overrides) — so `/D` is NOT pure unordered-dedup; it's
  *keyed last-wins on the macro name*. The model's `DedupUnordered` (sort+unique whole
  token) would wrongly keep both `FOO=1` and `FOO=2`. **Needs keyed-last-wins.**
- `DedupOrdered`: `/I` include dirs — order is the search order, semantic; dedup of
  identical dirs is safe, reorder is not. Same for `/AI` (assembly search), `/FI`
  (forced include — order matters AND is not a dir).
- `Once` (last-wins+lint): `/Fe`, `/Fo` when given a file (not a dir), `/Fd`, `/std:`,
  `/arch:`, `/EH*`.
- `Positional`: input source files `foo.c bar.cpp` — never reorder (object link order),
  never dedup (a file may legitimately be compiled twice with different `/Tp`/`/Tc`).
- **Accumulate-with-suppression**: `/wdNNNN` (disable) vs `/weNNNN` (error) vs `/woNNNN`
  vs `/wLNNNN` — multiple per build, the *set* matters and they can conflict on the same
  N. This is the GCC `-Wfoo`/`-Wno-foo` shape under a different skin — no Policy expresses
  "accumulate keyed by warning-number with precedence".

### 4. Path-bearing slots
- `PathOut`: `/Fe` (exe), `/Fo` (obj), `/Fd` (pdb), `/Fa`/`/FA` (asm listing),
  `/Fm` (map), `/Fp` (pch), `/Fi` (preprocessed), `/Fr`/`/FR` (browse), `/doc` (xdc).
- **PathOut-or-dir ambiguity**: `/Fo` with a trailing `\` (or naming an existing dir)
  means *output directory*; otherwise it's the output file. Same byte sequence, meaning
  set by a trailing separator / filesystem state. `PathOut` can't carry "or a dir".
- `Include`: `/I dir`, `/external:I dir` (external/system include), `/imsvc dir`
  (clang-cl), `/FI file` is a forced-include FILE not a dir → `PathIn`.
- `PathIn`: source files (positional), `/Yu`/`/Yc` (pch header file), `/FU` (force #using
  managed assembly), `/Tp file`/`/Tc file` (treat file as cpp/c — value IS a path).
- **Env-valued paths**: `INCLUDE`, `LIB`, `LIBPATH`, `_CL_`, `CL` env vars inject
  search dirs and *whole extra options* into the command. `_CL_` is appended, `CL` is
  prepended to argv. These are invisible in argv but change the cache key. **No Kind/Policy
  models env-injected argv.**
- `Define`: `/D`, `/U` (undef — anti-define, same namespace).

### 5. Model-breakers
- `/link` passthrough = flag opens a foreign Grammar to EOL (not subcommand-on-bare-token).
- `/Tc`/`/Tp` *retag the next positional's language* — a flag that changes the kind/meaning
  of a positional. Conditional grammar.
- `CL` / `_CL_` env vars prepend/append tokens: the *effective* argv ≠ literal argv.
- `/Fo` file-vs-dir disambiguation by trailing slash / FS probe.
- `/D` keyed-last-wins on macro name, not whole-token dedup.

### 6. Caching hazards
- `/Zi`/`/ZI` embed **absolute PDB paths** and build timestamps into objects; `/ZI`
  (edit-continue) is inherently nondeterministic. Canonicalizer must mark impure or pin
  `/Brepro` (deterministic builds, zeroes timestamps) — newer cl/link support `/Brepro`.
- `__DATE__`/`__TIME__` macros → embedded build time (source-level, not flag-level, but a
  cache-soundness hazard the flag layer can't see).
- `/FS` (force-serialized PDB writes) interacts with `/MP` parallel compile → **output
  order nondeterminism** in the shared PDB.
- Temp paths via `TMP`/`TEMP`; `/Fd` default `vcXXX.pdb` name carries a version hash.
- Absolute paths leak through `/I`, `/Fo`, `__FILE__`.

---

## 2. `link.exe` (the linker)

### 1. Invocation shape
Flat, no subcommands. `link [options] file...`. BUT the same binary is also invoked as
`link /lib ...` (→ lib.exe behavior), `link /dump ...` (→ dumpbin), `link /edit ...`
(→ editbin). These are **leading-option subcommands**: dispatch on a *flag*, not a bare
token, and they swap the entire grammar. `Grammar.subcommands` keys on a bare token, so
it misses this; you'd need a flag-keyed subcommand dispatch. Depth = 2.

### 2. Spelling forms
- **Bare**: `/DLL`, `/DEBUG`, `/NOLOGO`, `/INCREMENTAL`, `/RELEASE`, `/PROFILE`, `/VERBOSE`.
- **Colon-attached, single value**: `/OUT:foo.exe`, `/MACHINE:X64`, `/SUBSYSTEM:WINDOWS`,
  `/ENTRY:main`, `/BASE:0x10000000`, `/PDB:foo.pdb`, `/MAP:foo.map`, `/IMPLIB:foo.lib`.
- **Colon-attached, ENUM value**: `/DEBUG:FULL|FASTLINK|NONE`, `/OPT:REF,ICF,NOICF`,
  `/INCREMENTAL:NO`, `/LTCG:INCREMENTAL`, `/GUARD:CF`.
- **Colon-attached, COMMA-LIST value (nested mini-grammar)**: `/OPT:REF,NOICF`,
  `/DEFAULTLIB` can repeat, `/MANIFESTDEPENDENCY:"type='win32'..."`. The comma list is a
  Passthrough-like sub-language inside a single token.
- **Glued path**: `/LIBPATH:dir` (colon), repeatable.
- **Repeatable, value-bearing**: `/DEFAULTLIB:libcmt`, `/NODEFAULTLIB:libc`,
  `/WHOLEARCHIVE:foo.lib` (each repeat adds one), `/DELAYLOAD:foo.dll`, `/INCLUDE:symbol`,
  `/EXPORT:name`, `/LIBPATH:dir`, `/MANIFESTINPUT:file`.
- **Bare positionals**: `*.obj`, `*.lib`, `*.res`, `*.exp` files — mixed obj/lib in one
  list, order is link order (semantic).
- **Response files**: `link @link.rsp` — the *primary* mechanism (cl auto-generates one to
  beat the Windows command-length limit).
- `--`: not honored.

### 3. Multiplicity & Policy
- `Once`/Group: `/MACHINE:`, `/SUBSYSTEM:`, `/OUT:`, `/ENTRY:`, `/DEBUG:` (last-wins).
- `DedupOrdered`: `/LIBPATH:` (search order semantic). `/DEFAULTLIB:` accumulates but
  ordering vs explicit libs matters for symbol resolution. `/INCLUDE:` accumulates.
- `DedupUnordered`-ish: `/NODEFAULTLIB:` set semantics. But `/DEFAULTLIB` + `/NODEFAULTLIB`
  of the same lib is an **interacting pair** (suppress) — accumulate-with-negation again.
- `Positional`: `.obj`/`.lib` inputs — **never reorder** (classic link-order symbol
  resolution; reordering changes which definition wins) and never dedup (same `.lib`
  listed twice is a legit way to resolve circular deps).
- `/OPT:REF,ICF` vs `/OPT:NOREF,NOICF`: comma-list of sub-flags, each last-wins within the
  list, across multiple `/OPT:` tokens too. Sub-grammar with its own last-wins.

### 4. Path-bearing slots
- `PathOut`: `/OUT:`, `/PDB:`, `/MAP:`, `/IMPLIB:`, `/MANIFESTFILE:`, `/PGD:`, `/ILK`.
- `Lib`: positional `.lib`, `/DEFAULTLIB:`, `/WHOLEARCHIVE:` (value is a lib or empty=all),
  `/NODEFAULTLIB:` (anti-lib).
- `Include`(search-dir analog): `/LIBPATH:` is the Lib search dir → needs a "LibPath/dir"
  kind distinct from C `Include`.
- `PathIn`: `.obj`/`.res`/`.exp` positionals, `/DEF:foo.def`, `/MANIFESTINPUT:`,
  `/ORDER:@file` (order file), `/MIDL:@file`.
- **`@file` as a VALUE** (not the response-file lead): `/ORDER:@layout.txt` embeds a
  response-file-style `@` path *inside* a colon value. A second, nested `@file` slot.
- Env: `LIB`, `LIBPATH` env vars feed search dirs.

### 5. Model-breakers
- `/lib`, `/dump`, `/edit` flag-as-subcommand swapping the whole grammar.
- `/DEFAULTLIB` ↔ `/NODEFAULTLIB` mutual suppression keyed by lib name.
- Link-order positionals: order is *resolution-semantic*, and dedup is forbidden even for
  identical tokens — `Positional` covers "don't reorder/dedup" but the model has no way to
  express "this duplicate is meaningful, keep it" as distinct from a lint-worthy dup.
- `/WHOLEARCHIVE` with empty value = "all libs" — a flag whose value-presence changes scope.
- Nested comma sub-grammar in `/OPT:`.

### 6. Caching hazards
- `/DEBUG` embeds a **PDB GUID + age + absolute PDB path** + timestamps into the image.
  Mark impure unless `/Brepro` + `/PDBALTPATH:%_PDB%` (relative PDB path) are set.
- PE header **TimeDateStamp** is the build clock by default → `/Brepro` zeroes it.
- `/INCREMENTAL` writes a `.ilk` and produces **non-reproducible** layouts (padding/thunks);
  must be `/INCREMENTAL:NO` for a stable cache key.
- `/MANIFEST` may shell out to mt.exe; embedded manifest ordering.
- Absolute path leakage in `/PDB:`, `/MAP:`, import-lib paths.

---

## 3. `lib.exe` (the librarian)

### 1. Invocation shape
Flat, no subcommands. `lib [options] [files...]`. Mode is selected by which command-letter
option appears: default = build/append archive; `/EXTRACT:` = extract; `/REMOVE:` = remove;
`/LIST` = list; `/DEF:` = build import lib from a `.def`. These are **mode flags**, not
subcommands. Also reachable as `link /lib`. Depth = 1 (2 via link).

### 2. Spelling forms
- **Bare/mode**: `/LIST`, `/VERBOSE`, `/NOLOGO`, `/WX` (warnings as errors).
- **Colon-attached**: `/OUT:archive.lib`, `/MACHINE:ARM64`, `/DEF:exports.def`,
  `/NAME:foo.dll`, `/EXPORT:sym`, `/EXTRACT:member.obj`, `/REMOVE:member.obj`,
  `/LIBPATH:dir`, `/SUBSYSTEM:`.
- **Positionals**: input `.obj` and `.lib` files to archive.
- **Response files**: `lib @lib.rsp`.

### 3. Multiplicity & Policy
- `Once`: `/OUT:`, `/MACHINE:`, `/NAME:`, `/DEF:` (last-wins).
- `DedupOrdered`: `/LIBPATH:`.
- Repeatable: `/EXTRACT:` / `/REMOVE:` (one member each, accumulate), `/EXPORT:`.
- `Positional`: member `.obj`/`.lib` — **order defines archive member order**; do not
  reorder. Dedup forbidden (an `.obj` can be added twice; later wins inside the archive).
- `/LIST` to a redirected stdout — output, not a flag value.

### 4. Path-bearing slots
- `PathOut`: `/OUT:`. `PathIn`: `/DEF:`, member files. `/EXTRACT:member` value is an
  *archive-internal member name* (a path-LIKE string that is NOT a filesystem path) →
  must NOT be mounted/content-hashed. The model's path Kinds would wrongly try to mount it.
- `Lib`/dir: `/LIBPATH:`.

### 5. Model-breakers
- `/EXTRACT:`/`/REMOVE:` operate on archive-internal names — a "logical path" that looks
  like `PathIn` but resolves inside the archive, not the FS.
- Mode-by-flag (default vs extract vs remove vs def-import) — no bare subcommand token.

### 6. Caching hazards
- Archive member **timestamps and the archive symbol-table order**: lib writes a header
  per member with a date stamp → non-reproducible unless `/Brepro`-equivalent (lib honors
  `link`'s deterministic mode via the LIB tool's `/Brepro` in recent toolsets).
- Member insertion order vs symbol index → must canonicalize input order but CANNOT
  (order is semantic) — pure only if inputs are already canonical.

---

## 4. `clang-cl` (Clang's MSVC-compatible driver)

### 1. Invocation shape
Flat. `clang-cl [cl-style + clang-style opts] file... [/link link-opts]`. Two embedded
sub-grammars: `/link ...` (→ link/lld-link) and `/clang:<arg>` / `-Xclang <arg>`
(→ the clang -cc1 frontend). Depth = 2, with *two different* foreign grammars reachable
by different flags. `subcommands` (bare-token) models neither.

### 2. Spelling forms — the richest, and where the enum cracks
clang-cl accepts **BOTH** the MSVC `/`-style AND the GCC `-`/`--` style *simultaneously*:
- MSVC-style: `/c`, `/O2`, `/Fooout.obj`, `/I dir`, `/DFOO=1`, `/std:c++20`, `/EHsc`.
- GCC-style attached: `-O2`, `-Wall`, `-Wno-unused`, `-fcolor-diagnostics`,
  `-fsanitize=address`, `-std=c++20`, `-Iinc`, `-DFOO=1`, `-isystem dir`, `-lm`.
- **Equals**: `-std=c++20`, `-fsanitize=address,undefined` (comma-list value).
- **Separated**: `-o out.o`, `-isystem dir`, `-include hdr.h`, `-Xclang -ast-dump`.
- **`/clang:` glued passthrough**: `/clang:-fno-rtti` — colon-glued, value is a single
  GCC flag handed to the driver-as-clang. Repeatable; ORDER preserved.
- **`-Xclang <next-token>` passthrough**: each `-Xclang` forwards exactly ONE following
  token to cc1. To pass an option WITH an argument you write `-Xclang -foo -Xclang bar`.
  This is a Separated passthrough that forwards a single token into a foreign grammar.
- **`-Wl,a,b,c`**: comma-split, forwarded to linker (GCC convention) — this IS the model's
  `Passthrough(sep=',')`. Also `-Xlinker arg`, `-Wp,` (preprocessor), `-Wa,` (assembler).
- **`--`**: clang-cl DOES honor `--` end-of-options; everything after is a filename even if
  it starts with `/` or `-`.
- **Count flags**: `-v` verbosity; `-g`, `-ggdb` levels.
- **Negation pairs**: `-Wfoo`/`-Wno-foo`, `-f<x>`/`-fno-<x>` — accumulate-with-negation,
  last-wins per logical knob.
- Response files: `@file`, recursive, and clang resolves `@file` in config files relative
  to the config file.

### 3. Multiplicity & Policy
- **Accumulate-with-negation** (`-Wfoo`/`-Wno-foo`, `-fX`/`-fno-X`): NOT expressible by any
  Policy. It's "last token wins among a name + its `no-` antonym, keyed by the base name."
  This is THE canonical model-breaker for the warning/feature flag class.
- `Group`: `-O0/-O1/-O2/-O3/-Os/-Oz` and `/O*` last-wins, *across the two styles*
  (`-O2` then `/Od` → `/Od` wins): last-wins-across-aliases spanning syntaxes.
- `DedupOrdered`: `-I`/`/I`/`-isystem`/`-iquote`/`-idirafter` — multiple include-path
  *flavors* whose interleaved order is semantic. One logical "include search list" built
  from several distinct flag spellings → Policy can't say "these N flags share one ordered
  accumulator."
- `Positional`: inputs. `/clang:` and `-Xclang` accumulate, order preserved.
- `-Xclang` is "Separated but the value is opaque to me" — forwarded uninterpreted.

### 4. Path-bearing slots
- `PathOut`: `/Fo`, `/Fe`, `-o`. `Include`: `-I`,`/I`,`-isystem`,`-iquote`,`-idirafter`,
  `-imsvc`, `-isysroot`, `/external:I`. `Lib`: `-l`, `-L` (search dir), positional.
- `PathIn`: `-include file`, `-Xclang -include-pch -Xclang foo.pch`, `--config file`.
- **Config files** `--config=foo.cfg` pull in *more flags* from disk → like response files,
  invisible argv expansion that affects the cache key.
- `-fprofile-use=path`, `-fprofile-instr-use=path` — path inside an `=` value.

### 5. Model-breakers
- Two interchangeable syntaxes in ONE argv, with last-wins precedence spanning both.
- `-Wfoo`/`-Wno-foo` and `-f`/`-fno-` negation accumulation.
- Multiple distinct flag spellings feeding ONE ordered accumulator (`-I`/`-isystem`/...).
- `-Xclang` one-token-forward, requiring pairs to express option+arg.
- `--config` / `@file` argv expansion from the filesystem.
- `/clang:` colon-glued single-flag forward into a different grammar.

### 6. Caching hazards
- `-frandom-seed=` (must be PINNED, not left to clang's default which derives from the
  output filename/PID) — the textbook impurity.
- `-g` embeds absolute source paths + `.` cwd into DWARF/CodeView → needs
  `-fdebug-prefix-map=/abs=.` / `-ffile-prefix-map=` to be reproducible.
- `__DATE__`/`__TIME__`; `-ftime-trace` writes a JSON with wall-clock + emits a path.
- ASLR/`-fsanitize` runtime nondeterminism is runtime, not build.
- `/Brepro` (clang-cl honors it) zeroes the PE timestamp.

---

## 5. `rc.exe` (Resource Compiler)

### 1. Invocation shape
Flat, no subcommands. `rc [options] script.rc`. One positional (the `.rc` script).

### 2. Spelling forms
- **Glued single-letter (case-sensitive!)**: `/r`, `/v` (verbose), `/x` (no std include),
  `/c<codepage>` (e.g. `/c1252`), `/l<langid>` (e.g. `/l409` hex), `/ln<lang>`.
- **Glued path**: `/foOUT.res` (`/fo` + output, NO delimiter), `/Iinc\dir` (include path).
- **Separated-ish define**: `/dNAME` or `/dNAME=val` (preprocessor define, like cl `/D`),
  `/uNAME` (undefine).
- **Bare**: `/n` (null-terminate strings), `/y` (no warn on dup), `/nologo`.
- Both `/` and `-` accepted. Response files: `rc @args`.

### 3. Multiplicity & Policy
- `Once`: `/fo` (output), `/c` codepage, `/l` langid (last-wins).
- `DedupOrdered`: `/I` include search dirs.
- `Define`: `/d` / `/u` (define/undef, keyed-last-wins on name like cl `/D`).
- `Positional`: exactly one `.rc` input.

### 4. Path-bearing slots
- `PathOut`: `/fo` (the `.res`). `Include`: `/I`. `PathIn`: the `.rc` script and, transitively,
  every `#include`d header / `ICON`/`BITMAP`/`RCDATA`/`MANIFEST` file the script names.
- **Implicit input paths inside the .rc**: the script references icons, bitmaps, type
  libraries, manifests by RELATIVE path. None appear in argv; all are real PathIn for the
  cache key. **The model only sees argv-level paths; it cannot discover script-referenced
  inputs.** This is a genuine "content-addressing needs to parse the input" hazard.

### 5. Model-breakers
- Argv-invisible transitive inputs (icons/bitmaps/manifests referenced in the script).
- `/c`/`/l` numeric values are hex/decimal codepage & LCID literals (Str, must not be
  path-mounted) glued to a single-letter case-sensitive option.

### 6. Caching hazards
- `.res` is generally deterministic, BUT version-info resources can embed build dates;
  language/codepage defaults pull from the **host locale** if `/c`/`/l` omitted →
  host-dependent output. Canonicalizer should force `/c`/`/l` to be explicit or mark impure.

---

## 6. `mt.exe` (Manifest Tool)

### 1. Invocation shape
Flat, no subcommands — but it's a multi-mode tool (generate / merge / embed / extract /
hash / validate) selected by combinations of options, not a token.

### 2. Spelling forms — the odd one out
- **`-`-led, NO colon, SPACE-SEPARATED multi-value**: `-manifest file1.manifest
  file2.manifest -out:merged.manifest`. `-manifest` takes **one or more** following
  space-separated filenames until the next `-option`. This is a **variadic Separated** the
  enum lacks — `Separated` implies exactly one value token.
- **Colon-attached**: `-out:final.manifest`, `-inputresource:dll;#1`,
  `-outputresource:exe;#1`, `-updateresource:exe;#1`, `-hashupdate:dir`, `-tlb:foo.tlb`.
- **Resource-id sub-syntax in the value**: `dll;#1` (file `;` `#` resource-id) — a
  micro-grammar inside a colon value: `-inputresource:foo.dll;#1`.
- **Bare**: `-nologo`, `-validate_manifest`, `-canonicalize`, `-makecdfs`.
- mt.exe historically accepts `/` too but docs use `-`.

### 3. Multiplicity & Policy
- **Variadic accumulate**: `-manifest a b c` (merge inputs, order may matter for merge
  precedence) — Positional-like but introduced by a flag, and the boundary is "until next
  `-`". No Policy/Spelling expresses "flag introduces a variadic run of values."
- `Once`: `-out:` (last-wins).
- mode interactions: `-inputresource`/`-outputresource`/`-updateresource` combine to mean
  extract-then-embed; meaning is the *combination*.

### 4. Path-bearing slots
- `PathIn`: `-manifest <files...>`, `-tlb:`, `-managedassemblyname:`. `PathOut`: `-out:`,
  `-outputresource:` (writes into an existing binary — both in & out!).
- **`-updateresource:exe;#1`** reads AND writes the same file → a PathInOut, plus a
  resource-id sub-path (`;#1`) that is not a filesystem path.
- `-hashupdate:dir` is a DIR input whose *contents* (the files being hashed) are transitive
  inputs not in argv.

### 5. Model-breakers
- Variadic `-manifest a b c` value run.
- `-updateresource` = in-place read/write (no Kind for "mutated in place").
- `file;#id` resource-locator micro-grammar inside a colon value.

### 6. Caching hazards
- `-hashupdate` writes **file hashes computed at run time** into the manifest → output
  depends on referenced file contents (transitive), fine if those are pinned.
- Embedding a manifest with `-outputresource` rewrites a binary in place → the binary's
  timestamp/checksum changes; non-idempotent if run twice. Mark the in-place mode impure or
  model it as produce-new-file.

---

## 7. `dumpbin` (PE/COFF dumper)

### 1. Invocation shape
Flat. `dumpbin [options] file...`. Reachable as `link /dump`. Read-only inspector; mode
selected by which display option(s) given. Depth = 1 (2 via link).

### 2. Spelling forms
- **Bare display modes**: `/ALL`, `/HEADERS`, `/SYMBOLS`, `/RELOCATIONS`, `/EXPORTS`,
  `/IMPORTS`, `/DEPENDENTS`, `/DIRECTIVES`, `/LINENUMBERS`, `/LOADCONFIG`, `/SUMMARY`.
- **Colon-attached value**: `/DISASM:BYTES|NOBYTES`, `/SECTION:.text`, `/IMPORTS:foo.dll`,
  `/PDBPATH:VERBOSE`, `/RAWDATA:BYTES,8`, `/OUT:dump.txt`.
- Multiple display modes combine (`/HEADERS /SYMBOLS` dumps both).
- Both `/` and `-`. Response files supported.

### 3. Multiplicity & Policy
- Display modes accumulate (set semantics) → `DedupUnordered`-ish but ORDER affects the
  output text ordering (sections printed in flag order) → arguably `DedupOrdered`.
- `Once`: `/OUT:` (last-wins). `/SECTION:` can repeat to filter several sections.

### 4. Path-bearing slots
- `PathIn`: the binary positional(s). `PathOut`: `/OUT:` (redirect dump to file).
- `/IMPORTS:foo.dll` value is a FILTER name, not a path to mount.

### 5. Model-breakers
- Pure-read tool: there is no build output, only a text report — but `/OUT:` makes it a
  cacheable transform. The "kind" of a read-only inspector (no PathOut at all in the
  default case) isn't represented; it's a degenerate transform whose output is stdout.

### 6. Caching hazards
- `/DISASM` and `/PDBPATH` resolve and read the matching **PDB by absolute path** found in
  the image → output depends on a file NOT named in argv (the embedded PDB path).
- `/DEPENDENTS`/`/IMPORTS` output is stable, but `/DISASM` symbolization depends on whatever
  PDB is locatable on the host → host-dependent. Mark `/DISASM` impure unless the PDB is a
  pinned input.

---

## Consolidated digest (what the model must grow)

**New Spellings:**
- `ColonAttached` — `/OUT:x`, `/MACHINE:X64`: attached value delimited by `:`, no spaces.
- `DualLead` — `/` and `-` interchangeable per flag; canonicalize to one.
- `GluedNoDelim` — `/Fooout.obj`, `/Iinc`, `/dNAME`: value glued with zero delimiter,
  boundary defined by the option table (already roughly `Attached`, but boundary is
  table-driven not delimiter-driven).
- `NegationSuffix` — `/permissive-`, `/GR-`: trailing `-` flips a boolean.
- `VariadicSeparated` — `-manifest a b c`: one flag opens a run of space-separated values
  until the next option.
- `ConfigInclude` — `--config=f` / `@file`: argv expanded from a file on disk (recursive).
- `CommaSubGrammar` — `/OPT:REF,ICF`, `-Wl,a,b`, `-fsanitize=a,b`: comma list as a nested
  mini-grammar in one token (model has `Passthrough(sep)`; needs it for non-passthrough too).

**New Policies:**
- `KeyedLastWins` — `/D FOO=1` then `/D FOO=2` → keep last by macro name; warning-number
  flags `/wd /we /wo /wLN` keyed by N. (`DedupUnordered` on whole token is wrong here.)
- `AccumulateWithNegation` — `-Wfoo`/`-Wno-foo`, `-f`/`-fno-`, `/DEFAULTLIB`/`/NODEFAULTLIB`:
  last-wins among a base name and its negated antonym.
- `LastWinsAcrossAliases` — `-O2` vs `/Od` (different syntaxes, same Group) must collapse.
- `SharedOrderedAccumulator` — `-I`/`-isystem`/`-iquote`/`-idirafter` all feed ONE ordered
  include list; `Policy` is per-flag and can't bind several flags to one accumulator.
- `MeaningfulPositionalDup` — link `.lib` listed twice on purpose (circular deps): must
  distinguish "lint-worthy dup" from "semantically required dup."

**New Kinds:**
- `PathOutOrDir` — cl `/Fo` is a file or, with trailing `\`, a directory (FS-state-dependent).
- `LibSearchDir` — `/LIBPATH:` (lib search dir, distinct from C `Include`).
- `LogicalMember` — lib `/EXTRACT:member.obj`, dumpbin `/IMPORTS:foo.dll`: path-LIKE name
  resolved inside an archive/import-table, NOT mounted/hashed.
- `ResourceLocator` — mt `file.dll;#1`: filesystem-path + `;#`-resource-id micro-grammar.
- `PathInOut` — mt `-updateresource:exe;#1`: same file read and written in place.
- `EnvInjectedArgv` — cl `CL`/`_CL_`, `INCLUDE`/`LIB`/`LIBPATH`: tokens/paths injected from
  the environment, invisible in literal argv but part of the effective command & cache key.
- `TransitiveInputSet` — rc `.rc` script-referenced assets, mt `-hashupdate` dir contents,
  dumpbin/`/DISASM` embedded-PDB: real inputs the canonicalizer can't see without parsing.

**Genuine model-breakers:**
- Flag-keyed subcommand dispatch (`link /lib|/dump|/edit`, cl `/link`) — `subcommands`
  dispatches on a *bare token*, but MSVC swaps grammars on a *flag*.
- `/Tc`/`/Tp` retag the language of the FOLLOWING positional (flag mutates a positional).
- Env vars (`CL`, `_CL_`) prepend/append to argv → effective argv ≠ literal argv.
- Argv expansion from `@file` / `--config` (recursive) blurs argv vs filesystem.
- Two interchangeable flag syntaxes (clang-cl) with precedence spanning both.

**Caching hazards (the canonicalizer must neutralize or mark impure):**
- PE `TimeDateStamp`, PDB GUID+age, embedded absolute PDB path → require `/Brepro` +
  `/PDBALTPATH:%_PDB%` or mark impure.
- `/ZI` edit-and-continue, `/INCREMENTAL` `.ilk` layouts → forbid for cache keys.
- `-frandom-seed` default derives from output name/PID → must pin.
- `-g`/`/Zi` absolute source paths in debug info → `-ffile-prefix-map` / `/Brepro`.
- `__DATE__`/`__TIME__`, version-info resource dates → source-level impurity.
- `/MP` + `/FS` shared-PDB write order; lib member timestamps & symbol-index order.
- mt `-outputresource`/`-updateresource` in-place binary rewrite → non-idempotent.
- rc/`/DISASM` host-locale and host-PDB dependence → outputs depend on un-named host state.
