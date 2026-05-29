# Go Toolchain CLI Grammar Study

Tools: `go` (build / test / vet / install / mod), `cgo` (`go tool cgo`), `gofmt`.
Reference Go: `go1.26.3 darwin/arm64`. All flag inventories below are pulled from the
local `go help …`, `gofmt -h`, `go tool cgo -h`, and `go doc cmd/cgo`.

Model under test:

```
Grammar { flags, positionals, subcommands }
Flag { lead, Spelling, Policy, Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

The single most important global fact about the Go toolchain that fights this model:
**Go flags are single-dash, and accept BOTH `-flag value` (Separated) and `-flag=value`
(Equals) for the same flag, interchangeably, for every value-bearing flag.** A `bool`
flag is `-x` (Bare) but also `-x=false` (Equals). So Spelling is not a per-flag property
in Go — it's a per-*invocation* choice the user makes, and the canonicalizer must collapse
all forms to one. Our enum treats Spelling as a fixed attribute of a Flag; in Go it is a
*set* of accepted forms. This is the first model-breaker and it is pervasive.

---

## 1. `go` (the dispatcher + build/test/vet/install/mod)

### 1.1 Invocation shape / subcommand tree

```
go
├── build        [build flags] [packages]
├── install      [build flags] [packages]
├── test         [build flags] [test flags] [packages] [-args ...]
├── vet          [build flags] [-vettool prog] [vet flags] [packages]
├── run          [build flags] [-exec xprog] package [arguments...]
├── get / list / clean / generate / fix / work / env / doc / tool / version / bug / fix / telemetry
└── mod
    ├── download / graph / init / tidy / vendor / verify / why
    └── edit   [editing flags] [-fmt|-print|-json] [go.mod]
```

- Dispatch is on the next bare token: `go` → `mod` → `edit`. **Deepest nesting = 3 levels**
  (`go mod edit`, `go tool dist test`, `go work edit`). The recursive `Grammar.subcommands`
  map covers this cleanly *as a tree*.
- **But dispatch is not uniformly "next bare token".** `go tool <name>` dispatches to an
  *arbitrary external/embedded tool* whose grammar is not known to `go` — e.g.
  `go tool cgo …`, `go tool vet …`, `go tool pprof …`, `go tool compile …`. The set of
  subcommands here is **open/dynamic** (it includes user tools declared in `go.mod` via
  `tool` directives, plus everything in `$GOROOT/pkg/tool`). Our `subcommands` is a static
  `Map<String,Grammar>`; `go tool` needs a *fallback / dynamic-resolution* arm.
- `go run` and `go test` have a **hard positional boundary that flips parsing mode**: after
  the package spec, `-args` (test) or the program arguments (run) are handed verbatim to a
  *different* program. See §5.

### 1.2 Spelling forms actually used

| Form | In Go? | Example |
|------|--------|---------|
| Bare (bool) | yes | `-a`, `-race`, `-n`, `-x`, `-v`, `-work`, `-short`, `-failfast` |
| Separated | yes | `-o output`, `-p 4`, `-timeout 30s`, `-run TestFoo`, `-C dir` |
| Equals | yes (same flags) | `-o=output`, `-p=4`, `-buildvcs=false`, `-x=true` |
| Attached | **no** | Go never uses `-O2`-style glued short clusters; `-pn` is not `-p n`. |
| Passthrough(sep) | **partial** | `-gcflags='[pattern=]arg list'`, `-ldflags=…`, `-asmflags=…`, `-gccgoflags=…`, `-tags=a,b,c`. The *value* is a sub-argv (space-split, shell-quoting honored) destined for `go tool compile/link/asm`. |
| Response files (`@file`) | **no** | Go has no `@argfile` mechanism. |
| Repeated `--` | n/a | Go flags are single-dash; `--flag` is *accepted as an alias* for `-flag` (Go's flag pkg treats `-` and `--` identically) but `--` alone is **not** an end-of-flags marker. |
| `--` end-of-flags | **no** | There is no `--` separator. Boundaries are positional (the package list) and the explicit `-args` token (test) / first non-flag (run). |
| Count flags (`-vvv`) | **no** | `-v` is boolean, not a counter. `go tool dist` etc. use `-v` repeated occasionally but the public surface is bool. |
| Windows `/FLAG` | **no** | Go is uniformly POSIX-ish single-dash on all platforms incl. Windows. |

**Forms the Spelling enum lacks / mishandles:**
- **Dual Separated|Equals acceptance** (already noted) — not representable as one `Spelling` value.
- **`-flag=false` to negate a bool** — a boolean flag is `Bare`, yet `-buildvcs=false`,
  `-cgo=false`, `-x=false` are legal. So a `Bare` flag *also* accepts an `Equals` boolean
  value. Our `Kind` has no `Bool`, and `Bare` has no "may also take `=true/=false`" notion.
- **Passthrough whose value is a space-split sub-argv with an optional `pattern=` prefix**:
  `-gcflags='fmt=-m -l'` means "apply `-m -l` only to package `fmt`". That is a *map from
  package-pattern to sub-argv*, not a flat nested argv. Our `Passthrough(sep)` assumes a
  single nested grammar with one separator; this is `pattern=<sub-argv>`, repeatable, and
  last-one-doesn't-win — multiple `-gcflags` accumulate and are matched per package.

### 1.3 Multiplicity & order — required Policies and what's missing

Real `go build` / `go test` flag classes:

- **`Once` (last-wins)** — almost all scalar build flags: `-o`, `-p`, `-C`, `-buildmode`,
  `-compiler`, `-installsuffix`, `-mod`, `-modfile`, `-overlay`, `-pgo`, `-timeout`,
  `-parallel`, `-count`, `-run`, `-bench`, `-skip`, `-coverprofile`. Repeats: last wins.
- **`Group(name)` (mutually-exclusive last-wins)** — `-covermode set|count|atomic` is a
  *closed enum on one flag* (not separate flags), so it's `Once` over an enum, not `Group`.
  Go has **no** `-O0/-O1` style sibling-flag groups. Closest real Group: `-race` / `-msan`
  / `-asan` are mutually exclusive sanitizers but expressed as *independent bool flags* whose
  combination `go` rejects at validation time — our `Group` can't model "three separate bools,
  at most one true."
- **`DedupUnordered` (set, sorted)** — none cleanly. `-tags=a,b,c` is comma-joined and is
  effectively an unordered set (build tags), but it's carried as a single comma value, and
  **repeated `-tags` flags accumulate / are space-or-comma merged** (historical: space-sep
  was allowed). Canonical form: sort + dedupe the tag set.
- **`DedupOrdered`** — `-coverpkg pat1,pat2`, `-cpu 1,2,4` are ordered comma lists where
  order is *not* semantic for coverpkg but **is** semantic for `-cpu` (it sets the sequence
  of GOMAXPROCS values the tests run under). So one comma-list flag is dedup-unordered and a
  structurally-identical one is order-significant. Kind/Policy must be per-flag, fine, but
  note `-cpu 2,2` is **legal and meaningful** (run twice at GOMAXPROCS=2) → it must **not**
  dedupe. That violates the premise that any "list" flag is dedupable.
- **`Positional`** — the trailing `[packages]` list: import paths / `./...` patterns /
  `*.go` file lists. Never reorder (file-list order names the output binary: `go build ed.go
  rx.go` → binary `ed`), never dedupe.

**Missing policy needs:**
1. **`AccumulatePassthrough`** — `-gcflags`/`-ldflags`/`-asmflags` accumulate across repeats
   AND across `pattern=` scopes; canonicalization must group by pattern then order-preserve.
2. **`EnumOnce`** — value constrained to a closed set on a single flag (`-covermode`,
   `-buildvcs ∈ {true,false,auto}`, `-mod ∈ {readonly,vendor,mod}`).
3. **`OrderedNoDedup` list** (`-cpu`) vs **`UnorderedDedup` list** (`-tags`) — both are comma
   lists; the dedupe/sort decision is *value-semantic*, not spelling-derived.
4. **Cross-flag mutual exclusion** (`-race`+`-msan` rejected) — a *constraint between flags*,
   not within one. No policy expresses it.

### 1.4 Path-bearing slots

| Flag / slot | Kind needed | Notes |
|---|---|---|
| `-o output` (build/install) | **`PathOut` OR dir** | Ambiguous: if value names an existing dir or ends in `/`, output goes *into* it (a `PathOut` that is a directory of N files); else it's a single output file. One flag, two kinds. |
| `-C dir` | **`PathIn(dir)`** | chdir *before* anything; **must be first flag**. Changes the meaning of every subsequent relative path → affects how every other PathIn resolves. |
| `-overlay file` | `PathIn` | JSON file remapping source paths → a *virtual filesystem*; the paths inside it are themselves PathIns the cache must hash. |
| `-modfile file` / `go.mod` arg | `PathIn` | alternate go.mod. |
| `-pgo file` / `auto`/`off` | `PathIn` or enum | profile-guided-opt profile; `auto` means "find default.pgo". Mixed enum+path. |
| `-coverprofile`, `-cpuprofile`, `-memprofile`, `-blockprofile`, `-mutexprofile`, `-trace`, `-c -o`, `-dynout` | `PathOut` | profiling outputs. |
| `-outputdir directory` | `PathOut(dir)` | directory for the above profiles + test artifacts. |
| `-toolexec 'cmd arg'` / `-exec xprog` | `PathIn` + sub-argv | a *program to run* whose argv is a passthrough. |
| `[packages]` | `PathIn` *or* import path | `./foo` is a path; `example.com/foo` is an import path (resolved via module cache, NOT a filesystem path) — same slot, two namespaces. |
| `*.go` file list | `PathIn` | when args are `.go` files of one package. |
| `-trimpath` (build) | (no value) | strips absolute paths from binary → see caching §1.6. |
| `cgo -L${SRCDIR}/libs` | `Lib` + dir | via `#cgo LDFLAGS` (cgo section). |

**Kind coverage gaps:**
- **`ImportPath`** — a *non-filesystem* package reference resolved through the module
  cache + GOPATH + `./...` glob expansion. It is content-addressed but not via a local path;
  the cache key needs the resolved module version + checksum, not a mounted dir. `PathIn` is
  wrong for it.
- **`OutDir`** — `-o`/`-outputdir` can name a directory receiving an unknown set of files.
  `PathOut` (single file) is insufficient.
- **`Overlay`** — a path whose *contents are a map of further paths*. Recursive path hashing.
- **`Glob/Pattern`** — `./...`, `all`, `cmd/...` expand at runtime to a package set; the
  canonical key should be the *expansion*, not the literal pattern, or builds with `./...`
  cache-collide across trees.

### 1.5 Model-breakers (genuinely inexpressible)

1. **Separated/Equals are both legal for every value flag** (and bool flags also take
   `=true/=false`). Spelling is a per-occurrence user choice, not a Flag attribute.
2. **`-gcflags=pattern=args`** — a flag value that is itself `<package-glob>=<sub-argv>`,
   repeatable and accumulating per pattern. Two-level nested grammar with a keyed dispatch
   the `Passthrough(sep)` single-separator design cannot hold.
3. **`go tool <name>`** — dynamic, open subcommand set (includes user-declared module tools).
   `subcommands: Map<String,Grammar>` is static.
4. **`go test` mode-flip at the package boundary** — flags *after* the package list that are
   unknown to `go test` are an error UNLESS preceded by `-args`; after `-args` everything is
   passed verbatim to the test binary. The grammar in effect *changes* mid-argv. Worse: the
   same flag (`-cpuprofile`) is **rewritten** by `go test` into `-test.cpuprofile` for the
   child binary, and `-x` is **dropped** entirely. Canonicalization here is a *translation*,
   not a normalization.
5. **`-C dir` reinterprets all later relative paths** — a flag whose presence changes the
   resolution semantics (and thus the Kind behavior) of every other path arg. Flag meaning
   depends on a prior flag's effect on the filesystem cwd.
6. **`-cpu 2,2`** legal duplicate with meaning → "list ⇒ dedupable" is false for some lists.
7. **`-race` auto-mutates other flags**: setting `-race` forces `-covermode=atomic` and
   appends `_race` to `-installsuffix`. A flag whose value silently rewrites *other* flags'
   canonical values. No place in the model for flag-to-flag value propagation.

### 1.6 Caching hazards

- **Absolute path leakage**: object files and the test/cover binary embed the build cwd and
  GOPATH/GOROOT absolute paths unless **`-trimpath`** is set. Canonicalizer should treat
  `-trimpath` presence as part of the key (or refuse to cache trimpath-less builds as
  reproducible). `-C dir` and `${SRCDIR}` expansion (cgo) bake absolute paths too.
- **`-buildvcs`**: stamps VCS revision + **uncommitted/dirty flag + commit time** into the
  binary → nondeterministic across commits/time. `=false` neutralizes; otherwise impure.
- **`-ldflags '-X pkg.Var=...'`** is commonly used to inject `-X main.buildTime=$(date)` /
  git SHA → embeds timestamps. The literal value is in the passthrough; canonicalizer cannot
  tell a date-injection from a stable value → mark passthrough values that look like dates/
  `$(…)` impure, or hash them as-is and accept they bust the cache.
- **`-shuffle on`** seeds test order from the **system clock**; `-shuffle N` is reproducible.
  `on` is impure; the *reported* seed differs every run.
- **Fuzzing (`-fuzz`)** writes new crashers to `testdata/fuzz/` and runs unboundedly
  (`-fuzztime` default = forever) — output is nondeterministic by construction.
- **`-p` / `-parallel` / `GOMAXPROCS`** change parallelism → interleaving of test stdout/JSON
  event order is nondeterministic. Canonical output must sort/normalize event order.
- **`-work`** prints a random `/tmp/go-buildNNNN` dir name to stderr → tmp-path noise.
- **`-x` / `-n`** print the actual compiler invocations including absolute temp paths and a
  fresh `WORK=` dir each run → never cache-stable.
- **`-overlay`** content can point anywhere; its referenced files must be hashed, not just
  the overlay JSON path.

---

## 2. `go vet`

### 2.1 Invocation shape
`go vet [build flags] [-vettool prog] [vet flags] [packages]`. Single level under `go`, but
`-vettool=prog` swaps in an **arbitrary external analyzer** whose `[vet flags]` are unknown to
`go` — the same open-grammar problem as `go tool`. `go tool vet help printf` is a 3-deep
help path into per-analyzer flags (`-printf.funcs`, etc.).

### 2.2 Spelling
Same dual Separated/Equals. Own flags: `-c int` (context lines, Separated/Equals, **default
-1**), `-json` (bool), `-fix` (bool), `-diff` (bool). Plus all build flags it inherits
(`-C`, `-n`, `-x`, `-v`, `-tags`, `-toolexec`). Per-analyzer flags are **dotted-namespace**:
`-printf.funcs=…`, `-buildtag`, `-unreachable=false`. The `analyzer.flagname` dotted form is
a spelling our enum doesn't name (it's just Equals with a dotted key, but the *namespace
dispatch* is implicit).

### 2.3 Policy
- `-fix` / `-diff` mutually exclusive (cross-flag constraint, same gap as §1.3).
- Each analyzer is independently enable/disable: `-printf=false -shadow` → a **set of boolean
  toggles in a dotted namespace**. Closest model: many `Once` bools, but the namespace is
  open (depends on the loaded vettool).

### 2.4 Paths
`-vettool prog` = `PathIn` (executable). Output is diagnostics to stderr or a unified diff
(`-diff`) / patch applied in place (`-fix`) → `-fix` makes vet a **source mutator**, an effect
no Kind captures (it edits PathIn files).

### 2.5 Model-breakers
Open per-analyzer flag namespace behind `-vettool`; `-fix` mutates inputs.

### 2.6 Caching
`-fix` is non-idempotent w.r.t. the working tree; diagnostics include absolute file paths
unless `-C`/trimpath context normalizes them.

---

## 3. `go test` (treated as its own surface)

Covered structurally in §1, but its flag space is the densest. Highlights for the model:

- **`-test.` prefix duality**: every flag is recognized as `-foo` *and* `-test.foo`. When
  running a *compiled* test binary directly, the `-test.` prefix is **mandatory**; under
  `go test` it's optional. So the canonical name of a flag depends on *who consumes the
  argv* — `go` strips/rewrites `-cpuprofile`→`-test.cpuprofile`. The grammar's notion of a
  single canonical spelling is consumer-relative.
- **`-benchtime 100x` / `-fuzztime 1000x` / `-fuzzminimizetime`**: a value that is *either* a
  `time.Duration` (`1h30s`) *or* an `Nx` iteration count. One slot, two value languages.
- **`-shuffle off|on|N`**: enum ∪ int. (`EnumOrInt`.)
- **`-vet list`**: comma list of vet checks, or `off`, or empty=curated-default. Enum ∪ list.
- **`-args`**: literal end-of-go-flags token; everything after is the test binary's argv
  (a passthrough with **no separator char** — it's a positional sentinel).
- Profiling flags (`-cpuprofile`, `-memprofile`, `-blockprofile`, `-mutexprofile`, `-trace`,
  `-coverprofile`) all `PathOut`, and each implicitly **leaves `pkg.test` behind** (a side
  output) and sets `-c`-like behavior.

---

## 4. `go mod` (esp. `go mod edit`)

### 4.1 Invocation shape
`go mod {download,graph,init,tidy,vendor,verify,why,edit}`. `go mod edit` is the model-stressor.

### 4.2 / 4.3 Spelling + multiplicity — `go mod edit` is a **sequence of editing operations**

```
go mod edit [-fmt|-print|-json] \
  -module=path \
  -go=1.22 -toolchain=go1.22.1 \
  -godebug=key=value  -dropgodebug=key \
  -require=path@v      -droprequire=path \
  -exclude=path@v      -dropexclude=path@v \
  -replace=old[@v]=new[@v]  -dropreplace=old[@v] \
  -retract=version     -dropretract=version \
  [go.mod]
```

- The `-require/-droprequire`, `-replace/-dropreplace`, `-exclude/-dropexclude`,
  `-retract/-dropretract`, `-godebug/-dropgodebug` flags are **repeatable AND applied in
  command-line order as an edit script** ("The editing flags specify a *sequence* of editing
  operations"). This is **order-significant accumulation with paired add/drop** — exactly the
  `-Wfoo`/`-Wno-foo` accumulate-with-negation shape the prompt flags as missing.
- **`-replace` overrides redundant prior replacements**, and omitting `@v` *drops* version-
  specific ones — so a later `-replace` has side effects on earlier ones. Last-wins-with-
  scoped-clobber, not plain last-wins.
- `-fmt | -print | -json` are **mutually exclusive output modes** (a real `Group`).
- Values like `-replace=old@v=new@v` are **structured tuples** (`module@version` pairs with an
  optional RHS that is *either a module path or a local directory* = a `PathIn`!). One flag
  value contains both an import-path namespace token and a possible filesystem path.

### 4.4 Paths
`-replace`'s RHS may be a local dir (`PathIn(dir)`). The trailing `[go.mod]` positional is a
`PathIn`. `go mod vendor` writes a `vendor/` **OutDir**; `go mod download` populates the
module cache (a global `OutDir` keyed by checksum). Module versions resolve through
`go.sum` checksums — a `Checksummed` namespace, not a path.

### 4.5 Model-breakers
- **Edit-script ordering** of repeatable paired add/drop flags. The canonical form is the
  *resulting go.mod*, not a reordered argv — you cannot canonicalize by sorting flags; you must
  *replay the script*. This is the cleanest example of "order secretly matters."
- `-replace` RHS = path | import-path (overloaded namespace in one value).

### 4.6 Caching
`go mod tidy`/`download` reach the **network** (proxy.golang.org) and depend on remote module
availability + go.sum → impure unless `GOFLAGS=-mod=mod` pinned and cache warm. `GONOSUMCHECK`/
`GONOSUMDB`/`GOPROXY=off` change determinism. Embeds resolved versions that can move (`@latest`).

---

## 5. `cgo` (`go tool cgo`)

### 5.1 Invocation shape
`cgo -- [compiler options] file.go ...` — **single level**, but note the literal `--`:
here `--` is a **mandatory separator** between cgo's own flags and the *C compiler options*
that follow (a Passthrough to gcc/clang). This is a real `--`-as-passthrough-start, distinct
from POSIX end-of-flags.

### 5.2 Spelling
cgo's own flags are single-dash Separated/Equals: `-objdir string`, `-srcdir string`,
`-importpath string`, `-exportheader string`, `-dynimport`, `-dynout`, `-dynpackage`,
`-ldflags string`, `-gccgo`, `-gccgoprefix`, `-gccgopkgpath`, `-trimpath`, `-V`,
`-debug-gcc`, `-debug-define`. After `--`, the **C compiler argv** appears with full
GCC/Clang spelling: `-I/usr/include` (**Attached** — finally a real Attached form!),
`-DPNG_DEBUG=1` (**Attached Define**), `-lpng` (**Attached Lib**), `-L${SRCDIR}/libs`
(Attached lib-search-dir with **variable expansion**), `-O2`, `-Wl,-rpath,/x` (the canonical
**nested Passthrough(sep=,)**), `-Wno-unused` (accumulate-with-negation).

So cgo is where **every C-toolchain spelling the C study covers re-appears**, behind a `--`.

### 5.3 Policy (the post-`--` zone)
- `-I` / `-l`: `DedupOrdered` (order is semantic for search). Matches the model.
- `-D`: `DedupUnordered` define-set, but cgo concatenates `#cgo CFLAGS` across directives and
  files in source order, so it's actually accumulate-ordered.
- `-Wfoo` / `-Wno-foo`: **accumulate-with-negation, last-wins-per-warning** — model gap.
- Security filter: cgo only allows `-D -U -I -l` by default; `CGO_CFLAGS_ALLOW` is a **regex**
  that widens the allowed set. So *which flags are even legal* is governed by an env-valued
  regex — the grammar itself is environment-dependent.

### 5.4 Paths
`-objdir`/`-srcdir` (`PathIn(dir)`), `-dynout`/`-exportheader` (`PathOut`), trailing `file.go`
(`PathIn` positional). Post-`--`: `-I` Include, `-l` Lib, `-L` lib-search-**dir**,
`${SRCDIR}` token expands to **absolute source dir** inside a flag value.

### 5.5 Model-breakers
- **`--` as passthrough-start then a full C sub-grammar** behind it (two grammars in one argv,
  the second being the entire C-compiler study).
- **`${SRCDIR}` interpolation** inside flag values (and `#cgo` directives in *source comments*,
  not on the CLI at all — flags arriving from the build via `// #cgo LDFLAGS:` lines).
- **Env-regex-governed legality** (`CGO_CFLAGS_ALLOW`/`_DISALLOW`).
- `#cgo amd64 386 CFLAGS: …` — flags **conditioned on a build-constraint expression**
  (GOARCH/GOOS). Conditional grammar selection by target platform.

### 5.6 Caching
- `${SRCDIR}` and `-objdir`/`-srcdir` bake **absolute paths** → `-trimpath` exists on cgo to
  rewrite them.
- `CGO_CFLAGS`/`CGO_LDFLAGS`/`PKG_CONFIG` **env vars** feed flags that are *not* on the argv at
  all → cache key must include the cgo env, and `pkg-config` output is host-dependent +
  nondeterministic (orders, version-specific).
- `-debug-gcc` prints absolute gcc invocations with temp dirs.

---

## 6. `gofmt`

### 6.1 Invocation shape
`gofmt [flags] [path ...]` — **flat, no subcommands**. (`go fmt` is just a wrapper that runs
gofmt over packages.) Trivially covered by a leaf Grammar.

### 6.2 Spelling
Standard Go single-dash, Separated/Equals: `-r string` (Separated), `-cpuprofile string`
(PathOut). Bool: `-d`, `-e`, `-l`, `-s`, `-w`. No Attached/passthrough/response files.

### 6.3 Policy
- `-d` (diff) / `-w` (write) / `-l` (list) / default(stdout) are **output modes** — a `Group`
  in spirit, but gofmt allows some combinations (`-l -w`) and silently makes others useless,
  so not a strict mutual exclusion.
- `-r 'a[b:len(a)] -> a[b:]'` is a **rewrite-rule mini-language** as a flag value (its own
  pattern grammar with `->`). One occurrence only (last-wins). A Str whose content is a DSL.

### 6.4 Paths
`[path ...]` positional = files or dirs (recurses dirs) → `PathIn(dir?)`. `-cpuprofile` =
`PathOut`. **`-w` rewrites the input files in place** → again the "flag turns a PathIn into a
mutated output" effect with no Kind.

### 6.5 Model-breakers
- `-w` in-place mutation of `PathIn` positionals (input == output).
- `-r` rewrite-rule DSL embedded as a flag value.

### 6.6 Caching
`-cpuprofile` is the only impurity; otherwise gofmt is deterministic (formatting is a pure
function of input + Go version). Good cache-key candidate: hash inputs + `gofmt` version +
`-s`/`-r` flags.

---

## Cross-cutting summary

**Spellings the enum lacks/mishandles:** dual `Separated|Equals` per flag (per-occurrence,
not per-flag); bool flags also taking `=true/=false`; dotted analyzer namespace
(`-printf.funcs`); `Nx`-or-duration value language; enum∪int (`-shuffle`); `--` as mandatory
passthrough-*start* (cgo) vs absent end-of-flags; structured tuple values (`-replace=a@v=b@v`);
`pattern=<sub-argv>` keyed passthrough (`-gcflags`).

**Policies missing:** edit-script ordered paired add/drop (`go mod edit -require/-droprequire`);
accumulate-with-negation (`-Wfoo/-Wno-foo` in cgo passthrough); cross-flag mutual exclusion
(`-race`/`-msan`, `-fmt`/`-print`/`-json`, `-fix`/`-diff`); flag-to-flag value propagation
(`-race` ⇒ `-covermode=atomic`); ordered-no-dedup list with meaningful duplicates (`-cpu 2,2`)
vs unordered-dedup list (`-tags`); env-regex-governed legality (`CGO_CFLAGS_ALLOW`).

**Kinds missing:** `ImportPath`/`Checksummed` (module ref, not a local path); `OutDir`
(`-o dir`, `-outputdir`, vendor/); `Overlay` (path → map of paths); `Glob/Pattern` (`./...`
expansion); `InPlace` (input mutated as output: `gofmt -w`, `go vet -fix`); env-valued paths
(`CGO_*`, `GOPROXY`, `PKG_CONFIG`); `${SRCDIR}`-interpolated path values.

**Caching hazards:** absolute-path leakage (mitigated only by `-trimpath`); `-buildvcs` VCS
stamp incl. dirty flag + commit time; `-ldflags -X` date/SHA injection; `-shuffle on`
(clock-seeded); fuzzing writes corpora + runs forever; parallelism-order in `-json`/stdout
event streams; `-work`/`-x`/`-n` temp-dir noise; cgo `${SRCDIR}` + `CGO_*`/`pkg-config` env
inputs absent from argv; `go mod tidy`/`download` network + `@latest` version drift.
