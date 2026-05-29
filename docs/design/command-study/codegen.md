# Command Study: Codegen / misc tool group

Tools: **protoc, flatc, bindgen, cbindgen, swig, python, pip**

Studied against the Vixen command model:

```
Grammar { flags, positionals, subcommands: Map<String,Grammar> }
Flag { lead, spelling, policy, kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

Each tool is dissected by the 6 template points. Where a real flag cannot be
represented, it is flagged **MODEL-BREAKER**.

---

## 1. protoc (Protocol Buffers compiler)

### 1.1 Invocation shape
No subcommands. Flat: `protoc [flags] file.proto...`. `Grammar.subcommands`
is unused; dispatch is entirely on flags. Depth 0.

The twist is that the *flag namespace is open-ended*: any flag matching
`--<name>_out` that does not resolve to a built-in generator is dispatched to
an external plugin `protoc-gen-<name>` found on PATH. So the set of legal flags
is not a fixed table — it is "any `--*_out` / `--*_opt` token". This is closer
to a subcommand dispatch hidden inside the flag grammar.

### 1.2 Spelling forms
- Separated: `protoc -I src` , `-o out.pb`
- Equals: `--cpp_out=gen/`, `--proto_path=src`
- Attached short: `-Isrc` (clang-style attached include) — **Attached** with `Include` kind.
- **Compound colon value** `--go_out=plugins=grpc,paths=source_relative:./gen`
  — the value is `<comma-separated opts> : <output dir>`. One token encodes a
  sub-option list **and** a PathOut, glued by `:`. **MODEL-BREAKER** — neither
  Equals nor Passthrough(sep=',') captures the embedded `:`-delimited PathOut.
- Split form: `--go_out=./gen --go_opt=paths=source_relative` — the *same
  generator* configured via two different flags that must be merged. **Cross-flag
  aggregation** the model has no notion of.
- `@argfile` response files: `protoc @args.txt` — file contents are spliced into
  argv (one arg per line). **MODEL-BREAKER** for canonicalization: a path-valued
  arg whose *content* is more argv. Not a Spelling we have; also a path hazard
  (the @file is PathIn but expands recursively).

### 1.3 Multiplicity & order
- `-I/--proto_path`: repeatable, **order is semantic** (first match wins for
  imports) → `DedupOrdered` with `Include` kind. Dedup is safe; reorder is not.
- `--cpp_out` / `--java_out` / each `_out`: at most one per language, `Once`.
- `--<name>_opt`: repeatable, accumulates into that generator's param string;
  values are comma-joined → effectively `DedupOrdered`-ish but really an
  *accumulator scoped to a sibling flag*. **MODEL-BREAKER** (cross-flag scope).
- `Mfoo/bar.proto=pkg` import mappings (inside go_opt) — a map; last-wins per key.
- positional `.proto` files: `Positional`, never reorder/dedup.

### 1.4 Path-bearing slots
- `-I/--proto_path`: `Include` (dir). ✓
- `--cpp_out=` etc.: `PathOut` (a **directory**, not a file) — needs
  `PathOut(dir)` variant; current `PathOut` is file-oriented. Gap.
- `--descriptor_set_out=FILE`: `PathOut` (file). ✓
- `--dependency_out=FILE`: `PathOut` (Makefile fragment listing absolute paths —
  caching hazard, see 1.6).
- `--plugin=protoc-gen-foo=/abs/path`: a `PathIn` to an **executable**, named via
  `NAME=PATH`. New sub-kind: `PathExe`. The plugin binary's content affects output
  but is not normally hashed.
- `@argfile`: `PathIn` whose content is argv. Gap.

### 1.5 Model-breakers
1. Open flag namespace `--*_out` → external plugin dispatch (not in flag table).
2. `--foo_out=opts:dir` compound value (opt-list + colon + PathOut in one token).
3. `--foo_opt` accumulates into the sibling `--foo_out` — **flag whose meaning
   depends on another flag**.
4. `@argfile` recursive argv expansion.

### 1.6 Caching hazards
- `--dependency_out` writes **absolute paths**.
- Plugin binary (`protoc-gen-*`) is an *out-of-band input* not in argv — its
  version/content silently changes output. Must be content-hashed or marked impure.
- `--descriptor_set_out` with `--include_source_info` embeds source byte offsets;
  stable but path-sensitive.

---

## 2. flatc (FlatBuffers compiler)

### 2.1 Invocation shape
Flat, no subcommands. `flatc [GENERATORS] [OPTIONS] file.fbs... [-- json...]`.
Depth 0. Generator selection is by flag (`--cpp`, `--rust`, `--ts`...), multiple
allowed in one run.

### 2.2 Spelling forms
- Bare generator selectors: `--cpp`, `--rust`, `--gen-object-api`, `--gen-mutable`.
- Separated: `-o path`, `-I dir`, `--include-prefix p`, `--cpp-std c++17`.
- `--`: **end-of-flags / mode switch** — `flatc --cpp schema.fbs -- data.json`
  separates schema inputs from data inputs. The `--` here is not "everything after
  is positional-to-clang"; it's "switch the positional *kind*". **MODEL-BREAKER**:
  `--` changes the *meaning of subsequent positionals* (schema → data), not just
  ends flags.
- `--filename-suffix`, `--filename-ext`: Str.

### 2.3 Multiplicity & order
- Generator flags: each `Once`; collectively a set.
- `-I`: repeatable, order semantic → `DedupOrdered`/`Include`.
- `-o`: `Once`, last-wins.
- positionals split into two ordered classes (schemas, then data after `--`).

### 2.4 Path-bearing slots
- `-o`: `PathOut(dir)`. `-I`: `Include`. `--include-prefix`: a Str *path fragment*
  emitted literally into generated `#include` lines — NOT a real filesystem path,
  must stay `Str` even though it looks like a path. Easy to mis-kind.
- positional `.fbs`: `PathIn`. positional `.json` (after `--`): `PathIn`.

### 2.5 Model-breakers
- Dual positional classes selected by `--` (schema vs data).
- `--include-prefix` is path-shaped but literal (`Str`).

### 2.6 Caching hazards
- Generated code embeds file basenames; reproducible. `--gen-onefile` ordering of
  outputs depends on input order. Low hazard overall.

---

## 3. bindgen (rust-bindgen)

### 3.1 Invocation shape
Flat: `bindgen [bindgen-flags] header.h -o out.rs -- [clang args]`. Depth 0,
but with a **nested foreign grammar** after `--`.

### 3.2 Spelling forms
- Separated/Equals interchangeable: `--allowlist-function add.*` or
  `--allowlist-function=add.*`.
- `-o out.rs`, `-o` separated.
- `--` then a **full clang argv**: `-- -I/usr/include -DFOO=1 -std=c++17 -x c++`.
  This is a Passthrough, but the separator is a *single `--` introducing an entire
  nested argv*, not `-Wl,`-style comma list. The Passthrough(sep) variant assumes
  one token with an internal separator; here it's "rest of argv is grammar G2".
  **The model needs `Passthrough(EndOfArgs → nested Grammar)`** — partly covered
  if we treat `--` as a passthrough whose "value" is all remaining tokens.

### 3.3 Multiplicity & order
- `--allowlist-function/-type/-var`, `--blocklist-*`, `--opaque-type`: each
  repeatable, **set semantics, order irrelevant** → `DedupUnordered`. But: allow
  vs block *interact* — adding any allowlist flag flips the default from
  "everything" to "nothing". **MODEL-BREAKER**: presence of one flag changes the
  default policy of a whole flag family (conditional grammar).
- clang `-I` (after `--`): `DedupOrdered` (order semantic). `-D`: `DedupUnordered`.
  `-include foo.h`: order semantic, force-include.

### 3.4 Path-bearing slots
- `header.h` positional: `PathIn`. `-o`: `PathOut`.
- After `--`: `-I` Include, `-D` Define, `-isystem` Include(system), `-include`
  PathIn (force-include file). All live in the nested grammar.
- `--clang-args-from-file`? No — but `@file` not supported; clang itself supports
  `@file` response files in the passthrough region. Recursive.

### 3.5 Model-breakers
- Two-grammar split at `--` (bindgen-grammar | clang-grammar).
- allowlist presence inverts default visibility (conditional default).
- regex-valued flags: `Str` but with semantic equivalence (`.*` vs `.+`) the
  canonicalizer can't normalize.

### 3.6 Caching hazards
- Embeds clang/libclang **version-dependent** layout (struct sizes, `--target`).
  The libclang `.so` is an out-of-band input. Must hash target triple + clang version.
- `--with-derive-default` etc. deterministic. Output order of items is stable.

---

## 4. cbindgen (Rust → C/C++ headers)

### 4.1 Invocation shape
Flat: `cbindgen [flags] [INPUT crate dir] -o out.h`. Depth 0. Heavily driven by a
**config file** `cbindgen.toml` (`--config cbindgen.toml`) — most behavior is in
the file, not argv. The argv grammar is thin; the real grammar is the TOML.

### 4.2 Spelling forms
- Separated: `--config cbindgen.toml`, `--crate name`, `-o out.h`, `--lang c++`.
- Bare: `--cpp-compat`, `--clean`, `-v` (and `-vvv` count — verbosity).
- **Count flag `-v/-vv/-vvv`**: repeated bare flag whose multiplicity *is* the
  value. `Spelling` enum lacks a **Count** form. **MODEL-BREAKER**.
- `--lang [c|c++|cython]`: enumerated Str (a `Group`-like choice but single flag).

### 4.3 Multiplicity & order
- `-v`: count (see above).
- `--config`, `-o`, `--crate`, `--lang`: `Once`.
- `--profile [debug|release]`: `Group`.

### 4.4 Path-bearing slots
- `--config`: `PathIn` (TOML — content drives output, **must be hashed**).
- INPUT (positional): `PathIn(dir)` — a crate directory.
- `-o`: `PathOut`. `--depfile path`: `PathOut` (writes absolute paths → hazard).

### 4.5 Model-breakers
- Count flag `-vvv`.
- Dominant behavior lives in `cbindgen.toml`, not argv — canonical argv is
  *insufficient* as a cache key without folding in config-file content.

### 4.6 Caching hazards
- `--depfile` absolute paths.
- cbindgen embeds an `Autogenerated by cbindgen` header; some versions embed the
  cbindgen **version string** → cache key must include tool version.

---

## 5. swig

### 5.1 Invocation shape
Flat but **target-language flag acts as a mode switch that unlocks a different
flag set**: `swig -python -c++ -I incl -outdir out -o wrap.cxx iface.i`.
`swig -python -help` prints *python-specific* options. So the grammar after
`-<lang>` is conditional. Depth 0 in subcommands, but **conditional grammar**.

### 5.2 Spelling forms
- Bare language selectors: `-python`, `-java`, `-csharp`, `-go`, `-lua`, `-tcl`...
  (note: **single-dash long words**, not `--`). Our `lead` must allow single-dash
  multi-char tokens distinct from clusters.
- Bare mode: `-c++`, `-c` — note `-c++` contains `+`, a single token.
- Attached include: `-Idir` and separated `-I dir`.
- Separated: `-o wrap.cxx`, `-outdir dir`, `-module name`, `-oh header.h`.
- Equals/define: `-Dmacro` and `-Dmacro=value` (attached, define).
- Single-dash long options that look like GNU long opts but aren't: `-outdir`,
  `-includeall`, `-importall`, `-nofastdispatch`. **Spelling fine (Separated/Bare),
  but `lead` taxonomy must not assume single-dash ⇒ short cluster.**

### 5.3 Multiplicity & order
- `-I`: repeatable, order semantic → `DedupOrdered`/`Include`.
- `-D`: repeatable define set → `DedupUnordered`/`Define`. `-U` undefs interact
  with `-D` order → actually order-sensitive. **MODEL-BREAKER** (`-D X` then `-U X`).
- `-l file.i`: SWIG library files, repeatable, order semantic.
- `-outdir`, `-o`, `-module`: `Once`.

### 5.4 Path-bearing slots
- `-I` Include, `-o` PathOut, `-oh` PathOut, `-outdir` PathOut(dir),
  `-l` PathIn (library .i), iface `.i` positional PathIn.
- `-l` searches a **library path** (SWIG_LIB) — env-valued search dir. Out-of-band.

### 5.5 Model-breakers
- `-<lang>` conditionally unlocks a whole flag family (per-language options like
  `-py3`, `-noproxy`, `-namespace`). Conditional grammar keyed on a prior flag.
- `-D`/`-U` interleave order matters.
- single-dash-long lead taxonomy.

### 5.6 Caching hazards
- `SWIG_LIB` env var changes which library files resolve. Generated wrappers may
  embed the swig version banner. Include search depends on `-I` order + env.

---

## 6. python (the interpreter as a CLI)

### 6.1 Invocation shape
`python [interp-flags] (-m module | -c cmd | script.py | -) [script args...]`.
Depth ~1 but the "subcommand" is selected by **mutually exclusive mode flags**
(`-m`, `-c`, `-`, or a positional script), each of which **terminates interpreter
flag parsing** and hands the rest of argv verbatim to the program. This is a
classic **`--`-less end-of-flags**: the first non-option (or `-c`/`-m`) ends
Python's flags; everything after is the *program's* argv (a foreign grammar).

`python -m` dispatch is itself open: `python -m pip ...`, `-m venv`, `-m http.server`
— arbitrary nested tools. So real depth is unbounded via `-m`.

### 6.2 Spelling forms
- Bare: `-B`, `-E`, `-s`, `-S`, `-u`, `-I`, `-O`, `-OO`, `-q`, `-d`.
- **Count**: `-v` / `-vv` (verbosity). **MODEL-BREAKER** (Count again).
- **Repeated-with-meaning**: `-O` vs `-OO` — `-OO` ≠ two `-O`. Looks like count but
  is a **Group with escalating levels** expressed by char repetition in one token.
- Cluster: `-Es` = `-E -s`.
- `-X key=value`: an open-ended `-X` implementation option namespace
  (`-X faulthandler`, `-X importtime`, `-X dev`, `-X utf8=1`). Equals-ish but the
  key space is undefined. Repeatable, set-like. Like protoc's `_opt` but on one flag.
- `-W action:msg:cat:mod:lineno`: warning filter — a `:`-delimited 5-field
  sub-language in one token, repeatable, **order semantic** (later filters take
  precedence). **MODEL-BREAKER** sub-language (richer than `-Wl,` comma list).
- `-c cmd`: Separated, but value is a *program*, ends flag parsing.
- `-m mod`: Separated; value selects nested grammar, ends flag parsing.
- `--check-hash-based-pycs always|default|never`: enumerated.

### 6.3 Multiplicity & order
- `-W`: accumulate, ordered, precedence by position → `DedupOrdered`-ish but really
  a precedence stack.
- `-X`: set of key=val, last-wins per key → map.
- `-O`/`-OO`: `Group(optimize)` last/max-wins.
- mode flags `-c`/`-m`/script: mutually exclusive → `Group(mode)` but each ENDS parsing.

### 6.4 Path-bearing slots
- `script.py` positional: `PathIn`. `-` means **stdin** (a non-path "path").
- `PYTHONPATH` env → search dirs (out-of-band Include-equivalent).
- `-X importtime` etc. no paths. `-m` resolves a module via sys.path (env-valued).

### 6.5 Model-breakers
- Mode flag ends interpreter grammar and starts the *program's* grammar (two-grammar,
  like bindgen's `--` but triggered by `-c`/`-m`/positional, no delimiter token).
- `-m` opens unbounded nested tool grammar (recursion is real here).
- `-W` 5-field `:` sub-language with positional precedence.
- `-OO` repetition-as-level (Group encoded in token spelling).
- `-X` open key namespace.
- `-` = stdin pseudo-path.

### 6.6 Caching hazards
- `PYTHONHASHSEED` randomizes dict/set ordering & `hash()` — **nondeterminism**;
  must pin to fixed seed or mark impure. (`-R` / env.)
- `__pycache__` writes timestamp/hash-based `.pyc` — build-time side effects.
- `-X importtime`, profiling flags emit timing → impure output.
- Absolute paths leak into tracebacks / `__file__`.

---

## 7. pip

### 7.1 Invocation shape
**Genuine subcommand tree, depth 1–2**: `pip <cmd> [opts] [args]` where cmd ∈
{install, download, uninstall, freeze, list, show, check, config, search, wheel,
hash, debug, cache, index, inspect}. Some have **second-level subcommands**:
- `pip config get|set|unset|edit|list|debug` (depth 2)
- `pip cache dir|info|list|remove|purge` (depth 2)
- `pip index versions` (depth 2)
`Grammar.subcommands` covers this cleanly. **Deepest nesting: `pip config set k v`
(depth 2 + 2 positionals).**

### 7.2 Spelling forms
- Separated/Equals: `-i URL` / `--index-url=URL`, `-r req.txt`, `-c con.txt`,
  `-e ./pkg`, `-f links`, `--find-links links`.
- Bare: `--no-deps`, `--no-index`, `--user`, `--pre`, `-U/--upgrade`, `-q`, `-v`.
- **Count**: `-v/-vv/-vvv`, `-q/-qq` (verbosity/quietness, symmetric counts).
  **MODEL-BREAKER** (Count, and *paired* counts that net out: `-v -q`).
- Repeatable accumulate: `-r` (multiple requirements files), `-e`, `-f`,
  `--extra-index-url`, `--global-option`, `--config-settings KEY=VAL`.
- `--config-settings KEY=VALUE` / `-C KEY=VALUE`: key=val map, repeatable.
- requirement specifiers as positionals: `pip install 'foo[extra]>=1,<2'` — a
  **mini-language** (PEP 508) in a positional. `Str`/`Positional` holds it but
  can't canonicalize `>=1,<2` vs `<2,>=1`.

### 7.3 Multiplicity & order
- `-r/-e/-f/--extra-index-url`: repeatable, **order matters** (install/resolution
  order, link precedence) → `DedupOrdered`.
- `-i/--index-url`: `Once`, last-wins.
- positionals (package specs): `Positional`, order can affect resolution.
- `-v`/`-q`: count, and they **cancel** (`-v -q -v`) — net verbosity. The model has
  no "signed count / canceling counts".

### 7.4 Path-bearing slots
- `-r req.txt`: `PathIn` whose **content is more requirements** (recursive: a
  requirements file can `-r other.txt`, `-c con.txt`, `--find-links`, even URLs).
  **MODEL-BREAKER**: PathIn whose content is itself argv-grammar.
- `-c con.txt`: PathIn (constraints). `-e ./pkg`: PathIn(dir) editable.
- `--find-links DIR|URL|file`: Include-like search location, may be a path OR URL.
  **New Kind: `PathOrUrl` / `SearchLocation`.**
- `--target dir`, `--prefix dir`, `-d/--dest dir`: `PathOut(dir)`.
- `--cache-dir`, `--log file`: PathOut. `--cert`, `--client-cert`: PathIn.
- `--index-url`/`--extra-index-url`: **URL, not a path** — affects output but is a
  network input. New Kind `Url` (impure / network).

### 7.5 Model-breakers
- Requirements file is recursive argv-grammar inside a PathIn.
- PEP 508 specifier mini-language in positionals (un-canonicalizable by us).
- `--find-links` = path-or-url union kind.
- Canceling count flags (`-v`/`-q`).
- `--index-url` network input (no kind for "remote, impure").

### 7.6 Caching hazards
- Network indexes: `--index-url` resolves to *whatever PyPI serves now* — wheel
  selection depends on platform tags, time, yanked releases. Fundamentally impure
  unless pinned by hash (`--require-hashes`, `pip hash`).
- Installs run **arbitrary `setup.py`** (build backend) → side effects, timestamps.
- `pip freeze` output order is sorted but env-dependent.
- `__pycache__`, `RECORD` files embed absolute paths.

---

## Cross-tool synthesis

### Spelling forms the enum LACKS
| Form | Seen in | Note |
|---|---|---|
| **Count** (`-vvv`, `-q`, `-OO`) | cbindgen, python, pip | multiplicity *is* the value; sometimes canceling (pip `-v`/`-q`), sometimes escalating-Group (python `-O`/`-OO`) |
| **EndOfFlags→nested Grammar** | bindgen (`--`), python (`-c`/`-m`/script) | rest of argv is a *foreign* grammar; not a single-token Passthrough |
| **CompoundColon** `opts:dir` | protoc `--go_out=a,b:dir` | opt-list + `:` + PathOut in one token |
| **Multi-field sub-language** | python `-W a:b:c:d:e` | richer than `-Wl,` comma list; positional fields + precedence |
| **Response file `@file`** | protoc, clang-via-bindgen, pip `-r` | PathIn whose *content expands to argv* (recursive) |
| **Single-dash-long** lead | swig (`-python`, `-outdir`, `-c++`) | breaks "single dash ⇒ short cluster" assumption |

### Policies the enum LACKS
- **CrossFlagAccumulate**: protoc `--foo_opt` feeds sibling `--foo_out`;
  python/pip `--config-settings`. A flag's value scoped to another flag.
- **ConditionalDefault**: bindgen allowlist presence flips default visibility;
  swig `-<lang>` unlocks a different flag set. The grammar/defaults depend on which
  flags appeared.
- **OrderSensitiveOverride**: `-D X` then `-U X` (swig/clang); `-W` precedence stack
  (python). `DedupOrdered` dedups, but these need "keep all, order is the value".
- **CancelingCount**: pip `-v`/`-q` net out.
- **EscalatingGroup-in-token**: python `-O`/`-OO`.

### Kinds the enum LACKS
- **PathOut(dir)** — protoc/flatc/pip output *directories*, not files.
- **PathExe** — protoc `--plugin=NAME=/abs/exe`; out-of-band binary input.
- **Url / SearchLocation** — pip `--index-url`, `--find-links` (path-or-url union).
- **PathRecursive (argfile)** — `@file`, `-r req.txt`: content is more grammar.
- **StrPathLike** — flatc `--include-prefix`: path-shaped but emitted literally; must
  NOT be mounted/hashed as a path.
- **Stdin pseudo-path** — python `-` / pip `-`.
- **EnvSearchPath** — `SWIG_LIB`, `PYTHONPATH`, `-L` dirs: out-of-band search roots.

### Universal caching hazards
- **Out-of-band binary inputs**: protoc plugins, libclang (bindgen), build backends
  (pip). Not in argv; silently change output. Must hash tool/plugin version.
- **Network inputs**: pip indexes. Impure unless hash-pinned.
- **Absolute path leakage**: depfiles (protoc/cbindgen), tracebacks/`__file__`
  (python), RECORD (pip).
- **Embedded version banners**: cbindgen, swig, flatc headers.
- **PYTHONHASHSEED nondeterminism** (python).
- **`@file`/`-r` recursion** must be expanded *and* the referenced file content
  folded into the cache key, not just its path.
