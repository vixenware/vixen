# Command study: JS/TS tool group

Tools: **node, npm, pnpm, yarn, bun, vite, esbuild, tsc, rollup, webpack**.

Evaluated against the Vixen command model:

```
Grammar { flags: [Flag], positionals: [Positional], subcommands: Map<String,Grammar> }
Flag { lead, spelling: Spelling, policy: Policy, kind: Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

Each tool is dissected along the 6 template points. The structured digest at the end
keeps only the model-stressing distillation; everything below is the reference inventory.

> Versions probed locally: node v26.0.0, npm 11.12.1, pnpm 11.4.0, yarn 1.22.22.
> esbuild/vite/tsc/rollup/webpack/bun from official docs + source.

---

## Cross-cutting forms the `Spelling` enum does NOT have (preview)

The C/C++ group taught us `Attached`/`Equals`/`Passthrough`. The JS group adds several
that have no slot in the current enum:

- **`--no-FLAG` negation** (every Commander/yargs/npm bool flag): `--no-save`, `--no-fund`,
  `--no-audit`, `--no-bin-links`, node `--no-warnings`. The *negation prefix* is structural,
  not a separate flag name. The model has no notion of "this flag is the negation of that flag."
- **Compound-key colon spelling** (esbuild): `--define:K=V`, `--loader:.js=jsx`,
  `--out-extension:.js=.mjs`, `--external:react`, `--pure:console.log`, `--banner:js=...`.
  This is `Separated`-key + `Equals`-value fused with a `:` — a *two-level* key/value, not
  expressible as `Equals`.
- **Windows `/FLAG`** does NOT appear in this group (that's MSVC/cl). Absent here.
- **`@argfile` / response files**: tsc (`tsc @args.txt`), and node via `NODE_OPTIONS`-ish
  patterns. Webpack/rollup take config *files* rather than response files.
- **count flags `-vvv`**: pnpm/npm use `--loglevel`/`-d -dd -ddd` historically; yarn `--verbose`.
  Mostly absent in modern tools, but esbuild/webpack `--verbose` and node `-v` exist as Once bools.
- **kv-list flags** (`--env foo=bar`, npm `--omit dev --omit optional`): repeatable key=value
  whose canonical order may or may not matter.

---

## 1. node

### 1.1 Invocation shape
`node [v8-opts] [node-opts] [script.js] [script-args...]`. Two real sub-modes:
- `node inspect <script|host:port>` — a genuine **subcommand** (the inspector REPL/CLI).
- `node --test [paths...]` — flips node into the **test-runner** dispatch, a different
  positional grammar (test file globs) layered on the same flags.
- `node --watch`, `node --eval`, `node --print` — modal but flag-driven, not subcommands.

Deepest nesting: `node inspect <script> <args>` is depth 1. `subcommands` map covers
`inspect`. The *modal* flags (`--test`, `-e`, `-p`) are NOT subcommands — they mutate how
positionals are interpreted, which the recursive Grammar cannot model (point 5).

### 1.2 Spelling forms used
- Bare bool: `-c/--check`, `--watch`, `--test`, `--enable-source-maps`, `-i`.
- Separated **and** Equals interchangeably — node's own help states *both* `--flag value`
  and `--flag=value` are accepted, and `--no-flag` for bools. So every value flag is
  dual-spelled: `--require ./x.js` == `--require=./x.js`.
- Attached short: `-r ./pre.js`, `-e "code"`, `-p "1+1"`, `-C cond`.
- Optional-attached: `--inspect[=[host:]port]`, `--inspect-brk[=...]`, `--inspect-wait[=...]`
  — the value is *optional* and only Equals-attached (space form means "use default port").
  **This is an `Equals`-only-with-optional-value form** the enum doesn't distinguish.
- `--` end-of-flags: node uses `--` to mean "captures all remaining args in JavaScript"
  (i.e. into `process.argv`), per `--v8-options`. Real `--` semantics.
- Comma-list values: `--inspect-publish-uid=stderr,http`, `--disable-warning=a,b`,
  `--conditions` accumulates.
- Env-channel: **`NODE_OPTIONS`** env var is a *second argv source* spliced in front.
  `--env-file=.env` / `--env-file-if-exists=.env` load files that set env (path-bearing).

### 1.3 Multiplicity & order
- `-r/--require` and `--import`: **repeatable, order is semantic** (preload order matters) →
  `DedupOrdered` at best, but dedup may be *wrong* (preloading twice can matter) → really
  needs `Positional`-like "keep all, in order."
- `-C/--conditions`: accumulate, set-like → `DedupUnordered`.
- `--inspect` vs `--inspect-brk` vs `--inspect-wait`: mutually exclusive last-wins →
  `Group("inspect-mode")`.
- V8 flags (`--max-old-space-size=N`, `--stack-size=N`): last-wins `Once`. Node explicitly
  has `--abort-on-contradictory-flags` showing it *knows* flags can contradict.
- `--no-warnings` vs `--warnings`: negation pair → Once with negation awareness.

### 1.4 Path-bearing slots
- `script.js` positional → `PathIn`.
- `-r/--require`, `--import`, `--loader/--experimental-loader` → `PathIn` (module specifiers,
  may be bare package names too — *not always a path*).
- `--env-file=` → `PathIn`. `--cpu-prof-dir=`, `--diagnostic-dir=`, `--report-directory=` →
  **`PathOut` that is a DIRECTORY** (output dir). `--report-filename=`, `--cpu-prof-name=` → `PathOut`.
- `--snapshot-blob=`, `--build-snapshot-config=` → mixed in/out paths.
- `--allow-fs-read=<glob>` / `--allow-fs-write=<glob>` → path *globs* for the permission model.

### 1.5 Model-breakers
- **Modal positional reinterpretation**: `-e "code"` makes the next positional NOT a path;
  `--test` makes positionals globs; default makes positional[0] a script and the rest opaque
  script args. The grammar of positionals is *conditioned on a prior flag*. The model's
  static `positionals: [Positional]` cannot express "positional kind depends on flags seen."
- **NODE_OPTIONS env splice**: argv isn't the only input. Canonical argv must fold env in.
- **`--` hands everything after it to the *script's* argv**, an opaque nested namespace whose
  grammar we don't own (like passthrough, but unbounded and undelimited per-flag).
- **V8 flag passthrough**: `--v8-options` reveals hundreds of `--harmony-*` flags forwarded to
  V8 — a separate sub-grammar with its own `--no-` negations and `--flag value` rules.

### 1.6 Caching hazards
- `--cpu-prof`, `--prof`, `--heap-prof`, `--report-on-fatalerror` write timestamped,
  PID-named files (`CPU.20260529.*.cpuprofile`, `isolate-0x...-v8.log`) → impure, must mark.
- `--inspect[=port]` with default port binds a random-ish port → nondeterministic side effect.
- `--diagnostic-dir`/`--report-directory` default to CWD → **absolute-path leakage** into
  output. Reports embed timestamps, hostnames, PIDs.
- `--env-file` content affects behavior but isn't on argv → must content-hash the file.
- `process.env` generally → must decide which env keys are part of the cache key.

---

## 2. npm

### 2.1 Invocation shape
Flat but wide: `npm <command> [args] [--flags]`. ~60 commands (`install`, `ci`, `run`,
`exec`, `publish`, `pack`, `config`, `pkg`, `dist-tag`, `cache`, `audit`, `org`, `team`...).
Two-level dispatch on several: `npm config get|set|list|delete`, `npm cache add|clean|verify`,
`npm dist-tag add|rm|ls`, `npm pkg get|set|delete`, `npm team create|destroy|add|rm|ls`,
`npm org set|rm|ls`, `npm access grant|revoke|list|get|set`. **Deepest: depth 2**
(`npm access list packages <scope>` arguably depth 3). `subcommands` map covers this cleanly.
`npm run <script>` dispatches on *user-defined* script names — an **open/dynamic subcommand set**
read from package.json, not a fixed map (point 5).

### 2.2 Spelling forms used
- Long Equals/Separated: `--save-dev`, `--install-strategy hoisted`, `--prefix=./x`.
- Short aliases: `-S/--save`, `-E/--save-exact`, `-g/--global`, `-D/--save-dev`,
  `-w/--workspace`, `-f/--force`.
- **`--no-` negation everywhere**: `--no-save`, `--no-audit`, `--no-fund`, `--no-bin-links`,
  `--no-package-lock`, `--no-package-lock-only`.
- **Repeatable kv/enum flags**: `--omit dev --omit optional`, `--include prod --include dev`,
  `-w a -w b`.
- `--` passthrough: `npm run build -- --flag` forwards `--flag` to the script. Real `--`.
- `--key=value` generic config: ANY `.npmrc` key is settable as `--<key> <val>` (open flag set).
- Enum-valued: `--install-strategy <hoisted|nested|shallow|linked>`, `--allow-git <all|none|root>`.

### 2.3 Multiplicity & order
- `--omit`/`--include`: accumulate into a SET; order irrelevant → `DedupUnordered`. BUT
  `--include` can *cancel* a prior `--omit` of the same value → **negation-by-accumulation**
  across two different flag names. Model can't express "these two flags edit one set."
- `-w/--workspace`: accumulate, order irrelevant → `DedupUnordered`.
- Scalar config (`--prefix`, `--registry`, `--install-strategy`): last-wins `Once`.
- `--save` vs `--save-dev` vs `--save-optional` vs `--save-peer` vs `--no-save`: mutually
  exclusive last-wins → `Group("save-target")`.

### 2.4 Path-bearing slots
- positional `<package-spec>` → can be a path/tarball (`./local-pkg`, `file:../x`, `*.tgz`),
  a git url, or a registry name → **PathIn-or-not, context dependent**.
- `--prefix`, `--cache <dir>`, `--globalconfig <file>`, `--userconfig <file>`,
  `--logs-dir <dir>` → PathIn/PathOut dirs+files. `npm pack --pack-destination <dir>` → PathOut dir.
- `--before <date>` → a Str that is a *date*, not a path. `--min-release-age <days>` → number.

### 2.5 Model-breakers
- **Open subcommand set**: `npm run <user-script>` and `npm <command>` where commands include
  arbitrary script names; `npm exec`/`npx <any-bin>` runs arbitrary packages whose flags we
  cannot know. Grammar is *data-defined* (package.json), not static.
- **Generic `--<npmrc-key>` flags**: any config key is a valid flag. The flag set is open.
- **Cross-flag set editing** (`--omit`/`--include` as described).
- `npm exec -- <cmd>` re-enters an arbitrary tool's grammar (delegation).

### 2.6 Caching hazards
- `npm install` mutates `package-lock.json` and `node_modules` (massive impurity) — registry
  network, `--before`/`--min-release-age` make resolution *time-dependent*.
- Lockfile ordering historically nondeterministic; `npm-shrinkwrap` timestamps.
- `--cache` absolute path leaks. `--logs-dir` writes timestamped `*-debug-*.log`.
- `npm pack` tarball embeds mtimes/uid/gid unless normalized → impure tarball hash.

---

## 3. pnpm

### 3.1 Invocation shape
`pnpm [command] [flags]`. Commands: `add/i/install/up/update/rm/remove/link/unlink`,
`audit/ls/list/outdated/why`, `create/dlx/exec/run`, `config`, `init`, `publish`, `stage`,
plus `pnpm config get|set|delete|list` (depth 2). **`pnpm run <script>`** dynamic dispatch like
npm. `pnpm --filter <pat> <cmd>` is a workspace selector that *prefixes* a command — a global
flag that changes which projects the subcommand runs in. Deepest fixed: depth 2 (`config get`).

### 3.2 Spelling forms used
- `-r/--recursive` (run across workspace), `-C <dir>/--dir`, `-w/--workspace-root`,
  `-g/--global`, `-D/--save-dev`, `-P/--save-prod`, `-O/--save-optional`, `-E/--save-exact`.
- `--no-` negation: `--no-frozen-lockfile`, `--no-optional`, `--no-strict-peer-dependencies`.
- **`--filter <selector>` is a mini-language**: `--filter ./packages/*`, `--filter "...^pkg"`,
  `--filter "{packages/foo}[origin/main]"`, `--filter "pkg..."`. A *sub-grammar* (glob +
  dependency-graph operators `...`/`^`/`[since]`) inside one flag value — beyond `Passthrough`.
- `--` passthrough to scripts. `--config.<key>=<val>` dotted nested config keys.

### 3.3 Multiplicity & order
- `--filter`: **repeatable, results UNIONed**; order irrelevant but each value is itself a
  graph query → `DedupUnordered` of *opaque sub-queries*.
- `--save-dev`/`--save-prod`/`--save-optional`: `Group("save-target")`.
- `--reporter <default|append-only|ndjson|silent>`: last-wins `Once`.

### 3.4 Path-bearing slots
- `-C/--dir <dir>`, `--workspace-root`, `--store-dir <dir>`, `--virtual-store-dir`,
  `--modules-dir`, `--prefix` → dir paths. `--filter ./pkg` embeds a path inside a selector.
- positional package specs same as npm (path/git/registry tri-state).

### 3.5 Model-breakers
- **`--filter` dependency-graph sublanguage** (`...`, `^`, `[<git-ref>]`, brace globs) — a real
  query language with its own operators; not key=value, not `-Wl,`-style comma list.
- **`--filter` interacts with `-r`** and with the command: `pnpm -r --filter x run build` —
  the *meaning of positionals* (script name) depends on selector + recursive flags.
- Dotted config flags `--config.foo.bar=baz` → arbitrary nested namespace.

### 3.6 Caching hazards
- Store is content-addressed (good) but `--store-dir` absolute path leaks into output/lockfile.
- `pnpm-lock.yaml` regenerated; `--filter "[since=ref]"` makes results **git-state dependent**.
- Network/registry + `--prefer-offline` make outcome depend on local cache state.

---

## 4. yarn (classic 1.x; Berry differs)

### 4.1 Invocation shape
`yarn [command] [flags]`; bare `yarn` == `yarn install`. Commands include `add/remove/upgrade`,
`run`, `workspace`, `workspaces`, `global`, `config`, `cache`, `policies`, `team`, `owner`,
`tag`, `unplug`, `licenses`, `node`. **Genuine depth 2–3**: `yarn workspace <name> <cmd> [args]`
(run a command in a named workspace — *recursive re-dispatch* into another yarn command),
`yarn workspaces run <cmd>`, `yarn global add <pkg>`, `yarn config set <k> <v>`,
`yarn cache clean|dir|list`, `yarn policies set-version`, `yarn team add|remove|list`.
**Deepest: `yarn workspace pkg-a run build -- --flag` ≈ depth 3 + passthrough.**
`yarn workspace <name> <cmd>` is the cleanest case where `Grammar.subcommands` recursion
*genuinely applies* — but the second token (`<name>`) is **dynamic** (workspace name from
package.json), so the subcommand key is data-defined, not a static map entry.

### 4.2 Spelling forms used
- Separated value: `--cache-folder <path>`, `--cwd <cwd>`, `--mutex <type>[:specifier]`,
  `--network-concurrency <number>`, `--modules-folder <path>`.
- **Optional-bool with `[bool]`**: `--emoji [bool]`, `--scripts-prepend-node-path [bool]`,
  `--production [prod]` — flag takes an *optional* boolean-ish value. The enum has no
  "optional bool value" spelling.
- Alias pairs: `--enable-pnp, --pnp`; `--prod, --production`; `-s/--silent`; `-v/--version`.
- `--no-` negation: `--no-bin-links`, `--no-lockfile`, `--no-progress`, `--no-default-rc`,
  `--no-node-version-check`.
- `--mutex network:30330` → **value with embedded `:specifier`** sub-form.
- `--` passthrough (esp. `yarn workspace x run y -- ...`).

### 4.3 Multiplicity & order
- Mostly `Once` scalars (`--cache-folder`, `--registry`, `--network-timeout`).
- `--frozen-lockfile` / `--pure-lockfile` / `--no-lockfile`: overlapping lockfile modes →
  `Group("lockfile-mode")`.
- `--prefer-offline` / `--offline`: `Group("network-mode")`.

### 4.4 Path-bearing slots
- `--cache-folder`, `--global-folder`, `--link-folder`, `--modules-folder`,
  `--preferred-cache-folder`, `--cwd`, `--use-yarnrc <path>` → all PathIn/PathOut dirs+files.
- `--har` writes a HAR file to CWD (PathOut, implicit name).

### 4.5 Model-breakers
- **`yarn workspace <dynamic-name> <subcommand>`**: the subcommand key is a runtime value;
  then the *inner* command has its own full grammar — true recursive dispatch but with a
  dynamic key space.
- `--mutex <type>:<specifier>` and `--production [prod]` optional-value forms.
- Berry (yarn 2+) adds `yarn dlx`, plugin-defined commands → fully open subcommand set.

### 4.6 Caching hazards
- `yarn.lock` regenerated; `--update-checksums` rewrites integrity.
- `--har` output embeds wall-clock timings and absolute URLs.
- `--cache-folder`/`--cwd` absolute leakage. `--mutex file:/tmp/...` embeds tmp path.

---

## 5. bun

### 5.1 Invocation shape
`bun <command|file> [flags]`. Runtime + PM + bundler + test runner in one. Subcommands:
`run`, `install/add/remove/update/link/unlink`, `pm` (with `pm ls|cache|hash|bin|trust|...`
→ depth 2), `build`, `test`, `x`/`bunx`, `init`, `create`, `upgrade`, `repl`, `outdated`,
`patch`, `publish`. Also bare `bun ./file.ts` runs a file (like node). **Deepest: `bun pm cache rm`**
≈ depth 2. `bun run <script>` dynamic dispatch. `bun --filter <glob> <cmd>` workspace selector.

### 5.2 Spelling forms used
- Bun mirrors esbuild for `bun build`: `--define:K=V`, `--loader:.ts=tsx`, `--external react`,
  `--target <browser|bun|node>`, `--format <esm|cjs|iife>`, `--minify`, `--sourcemap[=linked|inline|external]`.
- `--no-` negation: `--no-save`, `--no-install`, `--no-cache`.
- Optional-attached value: `--sourcemap` (bare) vs `--sourcemap=external`.
- `--filter '<glob>'` workspace selector (simpler than pnpm: glob only, no graph ops).
- `--` passthrough; `bunx <pkg> -- args`. `--hot`, `--watch` modal runtime flags.

### 5.3 Multiplicity & order
- `bun build --external a --external b`: accumulate set → `DedupUnordered`.
- `--define:` repeatable; later wins per key → effectively a map (`DedupUnordered` keyed).
- `--target`/`--format`: `Once`/`Group`.

### 5.4 Path-bearing slots
- entry positionals → `PathIn`. `--outdir <dir>` → PathOut dir. `--outfile <file>` → PathOut.
- `--root <dir>`, `--tsconfig-override <file>`, `--config <file>` → PathIn.
- `--public-path <url-or-path>` → path-ish but may be a URL prefix.

### 5.5 Model-breakers
- Same esbuild compound-colon two-level key/value (`--define:K=V`).
- `bun run <dynamic-script>` / `bunx <arbitrary-pkg>` open dispatch + delegation.
- Modal: bare `bun <file>` vs `bun <subcommand>` — first token is *either* a path *or* a
  command keyword; ambiguity resolved by existence-on-disk. Grammar can't express
  "subcommand key only if not a file."

### 5.6 Caching hazards
- `bun build --sourcemap` embeds absolute source paths unless `--root` normalizes.
- `bun.lockb` is a binary lockfile (mtime-sensitive tooling). Install is network/time dependent.
- `--watch`/`--hot` are stateful daemons (impure by nature).

---

## 6. vite

### 6.1 Invocation shape
`vite [command] [root] [flags]`. Commands: `dev`/`serve` (default), `build`, `preview`,
`optimize`. Bare `vite` == `vite dev`. Positional `[root]` is the project dir. **Depth 1.**
`subcommands` map covers `{dev, build, preview, optimize}`.

### 6.2 Spelling forms used
- Separated/Equals: `--config vite.config.ts` (`-c`), `--base /path/`, `--mode production`
  (`-m`), `--outDir dist`, `--logLevel info` (`-l`), `--port 5173`, `--host [host]`.
- Optional-value bool-ish: `--host` (bare → 0.0.0.0) vs `--host 1.2.3.4`; `--open [path]`;
  `--sourcemap [output]` (bare/true/inline/hidden).
- `--no-` negation: `--no-clearScreen`, `--no-color`.
- `-d/--debug [feat]` count-ish/optional filter; `-f/--filter <filter>`.
- camelCase flags: `--clearScreen`, `--emptyOutDir`, `--minify [minifier]`, `--assetsInlineLimit`.
- `--force` (re-optimize deps). `--strictPort`. `--cors`.

### 6.3 Multiplicity & order
- Almost all `Once` (last-wins). `--mode`/`--base`/`--port` scalar.
- `--minify [minifier]` enum optional-value `Once`.

### 6.4 Path-bearing slots
- `[root]` positional → PathIn dir. `--config <file>` → PathIn. `--outDir <dir>` → PathOut dir.
- `--base <path>` is a *URL base path*, NOT a filesystem path (Str, not Path).
- `--logLevel`, `--mode` → Str. `--ssrManifest [name]` → PathOut.

### 6.5 Model-breakers
- `--base` looks like a path but is a URL prefix — the **Path-vs-Str distinction is value-
  semantic, not lexical**. Misclassifying it would wrongly mount/hash it.
- Behavior driven mostly by `vite.config.ts` (a JS module that can read env / be a function) —
  the *real* grammar lives in a Turing-complete config, invisible to argv canonicalization.

### 6.6 Caching hazards
- `vite build` emits `dist/assets/<name>-<hash>.js` — hashes derive from content (ok) but
  build also embeds `__vite__mapDeps`, timestamps in some plugins.
- Dev server picks a free port if `--port` taken (unless `--strictPort`) → nondeterminism.
- `node_modules/.vite` dep-optimize cache keyed on lockfile + config → cross-run state.

---

## 7. esbuild

### 7.1 Invocation shape
`esbuild [entry points...] [flags]`. Effectively flat — no subcommands; `--serve`, `--watch`,
`--analyze` are modal flags. Positionals are entry points. **Depth 0.**

### 7.2 Spelling forms used (the richest in this group)
- **Compound-colon two-level key/value** (unique): `--define:process.env.NODE_ENV=\"production\"`,
  `--loader:.png=dataurl`, `--out-extension:.js=.mjs`, `--external:react`, `--pure:console.log`,
  `--banner:js=//x`, `--footer:css=...`, `--alias:foo=bar`, `--inject:./shim.js`,
  `--mainFields:module,main`, `--resolve-extensions:.ts,.js`, `--supported:bigint=false`,
  `--log-override:foo=silent`. The `:` selects a *sub-key*; the `=` sets its value.
- Plain Equals: `--bundle`, `--minify`, `--format=esm`, `--platform=node`, `--target=es2020`,
  `--sourcemap=external`, `--tree-shaking=true`.
- Optional-value: `--sourcemap` (bare) / `--sourcemap=inline|external|both|linked`;
  `--minify` (bare) / `--minify-whitespace`/`--minify-identifiers`/`--minify-syntax` (split).
- Comma-list values: `--target=es2020,chrome58,node14`, `--main-fields:module,main`.
- `--out-extension:.js=.mjs` — the value itself is `oldext=newext`.

### 7.3 Multiplicity & order
- `--external:` / `--pure:` / `--inject:` : accumulate **set** → `DedupUnordered`.
- `--define:K=V` / `--loader:.ext=l` / `--alias:` : accumulate **map keyed by sub-key**;
  later same-key wins → keyed `DedupUnordered`. Model has no keyed-map policy.
- `--inject:` order can matter (injection order) → arguably `DedupOrdered`.
- `--target` comma-list: order irrelevant, dedupe ok.
- `--format`/`--platform`: `Once`/`Group`.

### 7.4 Path-bearing slots
- entry points → `PathIn`. `--outfile=` → PathOut. `--outdir=` → PathOut **dir**.
- `--inject:./shim.js` → PathIn nested in colon-flag. `--tsconfig=` → PathIn.
- `--outbase=` → PathIn dir (controls output layout). `--public-path=` → URL-ish.
- `--loader:.ext` keys are extensions (Str), values are loader names (Str) — *not paths*.
- `--external:` values can be globs/package names — not paths.

### 7.5 Model-breakers
- **Two-level `--key:subkey=value`** is the headline: needs Spelling that parses a sub-key
  AND a value, and a keyed-map Policy.
- `--sourcemap=both` produces *two* outputs from one flag (1→N output relationship).
- `--out-extension:.js=.mjs` rewrites output naming — affects PathOut *names* derived from rule.

### 7.6 Caching hazards
- `--sourcemap` embeds `sourceRoot`/absolute source paths unless `--sourceRoot=`/`--outbase` set.
- Banner/footer may include user timestamps. `--define:BUILD_TIME=...` invites timestamp injection.
- `--metafile=` writes a JSON with absolute paths. `--serve` picks a port (nondeterministic).

---

## 8. tsc

### 8.1 Invocation shape
`tsc [files...] [options]` OR `tsc -b/--build [project-dirs...] [build-opts]`. The `--build`
flag switches to a **different grammar** (project-reference orchestrator) where many normal
flags (`--outDir`) are *rejected* with "Unknown build option". No nested subcommands;
`--init`, `--watch`, `--listFiles` are modal. **Depth 0, but two mutually-incompatible flag
universes selected by `-b`.**

### 8.2 Spelling forms used
- Long Separated/Equals: `--outDir dist`, `--target ES2020`, `--module esnext`,
  `--moduleResolution bundler`, `--lib ES2020,DOM`, `--jsx react-jsx`.
- Short: `-p/--project tsconfig.json`, `-b/--build`, `-w/--watch`, `-d/--declaration`.
- `--no-`-ish via explicit false: `--noEmit`, `--strict false` (bool flags take `true|false`
  as a *separate token* too: `tsc --strict false`).
- **`@argfile` response file**: `tsc @args.txt` reads newline-separated args from a file.
  *The only true response-file user in this group.*
- comma-list: `--lib ES2020,DOM,DOM.Iterable`, `--types node,jest`.
- camelCase everywhere (`--allowJs`, `--esModuleInterop`, `--skipLibCheck`).

### 8.3 Multiplicity & order
- `--lib`/`--types`: comma-list, set-like → `DedupUnordered` (but `--lib` order documents intent).
- Scalars (`--target`, `--module`, `--outDir`): `Once`.
- `--build` modes: `--build --force`, `--build --clean`, `--build --watch`, `--build --dry`.
- Many bools last-wins; CLI overrides tsconfig.

### 8.4 Path-bearing slots
- positional `files...` → PathIn. `-p/--project <file|dir>` → PathIn (file OR dir!).
- `--outDir <dir>` → PathOut dir. `--outFile <file>` → PathOut. `--rootDir <dir>` → PathIn dir.
- `--declarationDir <dir>` → PathOut dir. `--tsBuildInfoFile <file>` → PathOut.
- `--typeRoots <dir,dir>` → PathIn dirs (comma-list of dirs). `--baseUrl <dir>` → PathIn dir.
- `@argfile` → PathIn whose *contents are more argv* (response file).

### 8.5 Model-breakers
- **`-b/--build` selects an entirely different flag universe**: a *conditional grammar* where
  the validity of every other flag depends on whether `-b` is present. The model has no
  "flag presence gates the rest of the grammar."
- `@argfile` is a path whose content is recursively argv → needs response-file Spelling.
- `--project <x>` accepts file OR dir, changing meaning (PathIn file vs dir) by inspection.
- bool-as-separate-token (`--strict false`) blurs Bare-bool vs Separated-value.

### 8.6 Caching hazards
- `--incremental`/`composite` write `.tsbuildinfo` containing **absolute paths + version +
  timestamps + file hashes** → must be excluded from / normalized in the cache key.
- `--tsBuildInfoFile` default lands in outDir (absolute-path leak).
- Emit ordering generally deterministic, but declaration maps embed source absolute paths
  unless `--rootDir`/`--declarationDir` normalize.

---

## 9. rollup

### 9.1 Invocation shape
`rollup [input] [options]` or `rollup -c [config]`. No subcommands; `-w/--watch` modal.
**Depth 0.** Config-file driven (`rollup.config.js`).

### 9.2 Spelling forms used
- `-i/--input <file>`, `-o/--file <file>` (alias `--output.file`), `-f/--format <es|cjs|umd|iife>`,
  `-c/--config [file]` (optional value), `-n/--name <name>`, `-m/--sourcemap`,
  `-e/--external <a,b>`, `-g/--globals <k:v,k:v>`, `-p/--plugin <name>`, `-d/--dir <dir>`.
- **Dotted output namespace**: `--output.file`, `--output.dir`, `--output.format`,
  `--output.sourcemap`, `--output.banner`, `--output.globals`, `--output.entryFileNames`.
  A *nested object path* in the flag name (`--a.b.c=v`). Beyond `Equals`/`Passthrough`.
- `--globals foo:bar,baz:qux` → comma-list of `key:value` pairs (yet another sub-form).
- `--no-` negation: `--no-treeshake`, `--silent`. `-w/--watch`.

### 9.3 Multiplicity & order
- `--plugin`: repeatable, **order semantic** (plugin pipeline) → `DedupOrdered`/keep-all.
- `--external`/`--globals`: set/map comma-lists → `DedupUnordered` keyed.
- `--output.*`: each scalar `Once`, but multiple outputs possible via config (array).

### 9.4 Path-bearing slots
- `-i/--input` → PathIn (can be comma-list of entries, or `name=path` keyed). `-o/--file` → PathOut.
- `-d/--dir`/`--output.dir` → PathOut dir. `-c/--config [file]` → PathIn.
- `--input app=src/main.js` → keyed PathIn (named entry).

### 9.5 Model-breakers
- **Dotted nested-object flag names** (`--output.entryFileNames`) — arbitrary nesting depth.
- `--input name=path` keyed entries (PathIn behind a name).
- Config-file is the real source of truth; argv is a thin override layer.

### 9.6 Caching hazards
- `entryFileNames`/`chunkFileNames` patterns include `[hash]` (content-derived, ok) but also
  `[date]`-ish via plugins; banners often embed build timestamps.
- sourcemap `sourceRoot` absolute leakage; `--watch` daemon impurity.

---

## 10. webpack

### 10.1 Invocation shape
`webpack [entries...] [options]` via webpack-cli. **Real subcommands**: `webpack build`
(default), `webpack serve` (`webpack-dev-server`), `webpack watch`, `webpack info`,
`webpack init`, `webpack configtest`, `webpack help`, `webpack version`, `webpack loader`,
`webpack plugin`, `webpack migrate`. **Depth 1**, dispatched by webpack-cli's command registry.
`subcommands` map covers it; some are plugin-contributed (open-ish).

### 10.2 Spelling forms used
- `--config <file>` (repeatable!), `--config-name <name>` (repeatable), `--merge`,
  `--mode <development|production|none>`, `-o/--output-path <dir>`, `--entry <file>`,
  `-w/--watch`, `-d/--devtool <type>`, `--target <web|node|...>`, `--stats <preset>`.
- **`--env <key=value>`** repeatable, builds an env object passed to a *function config*:
  `--env production --env platform=web` → `{production:true, platform:'web'}`.
- Dotted/bracket nested: `--env foo.bar=baz` builds nested object; `--module-rules`,
  `--optimization-minimize`, `--output-library-name` — long flattened config-path flags.
- `--no-` negation: `--no-color`, `--no-stats`, `--no-watch`.
- `--analyze` (loads bundle-analyzer). `--progress [profile]`.

### 10.3 Multiplicity & order
- `--config` repeatable + `--merge`: multiple configs **combined** (order matters for merge)
  → `DedupOrdered`/keep-all.
- `--config-name` repeatable: selects subset of an array-config → `DedupUnordered` set.
- `--env` repeatable: **accumulates into a structured object**, dotted keys nest → keyed map
  with nesting; later same-key wins → no existing Policy fits.
- `--mode`/`--target`/`--devtool`: `Once`.

### 10.4 Path-bearing slots
- entries / `--entry` → PathIn (can be named: `--entry-reset` + `--entry main=./a.js`).
- `-o/--output-path <dir>` → PathOut dir. `--config <file>` → PathIn. `--output-filename` → Str pattern.
- `NODE_OPTIONS` env channel for node-level flags (path-bearing preloads).

### 10.5 Model-breakers
- **`--env key=value` building a nested config object** (dotted keys → nesting, bool when
  bare) — a structured-object accumulator, not flat key=value.
- **`--config` + `--merge`** composes multiple grammars/config modules (1→N config inputs
  merged with semantics).
- Most config lives in `webpack.config.js` which **can export a function** receiving `--env`
  — argv canonicalization can't see the real config graph.
- Plugin-contributed subcommands → open command set.

### 10.6 Caching hazards
- `output.filename: '[name].[contenthash].js'` (content-ok) vs `[hash]`/`[fullhash]` (build-
  global, changes with unrelated input) and `[chunkhash]`; some setups embed build `Date.now()`.
- `--devtool` sourcemaps embed absolute `webpack://` + source paths.
- `records-path`/persistent cache (`cache.type:'filesystem'`) under `node_modules/.cache`
  is cross-run state with absolute-path keys; parallelism can reorder chunk ids.
- `--watch`/`serve` daemons; dev-server picks free port.

---

## Summary: what the model must grow

- **`Spelling`** needs: `Negatable` (`--no-x`), `OptionalValue` (`--inspect[=port]`,
  `--sourcemap`), `ColonKeyEquals` (esbuild `--define:K=V`), `DottedPath` flag names
  (rollup `--output.dir`, webpack `--env a.b=c`), `ResponseFile` (`@argfile`), and
  value sub-forms like `type:specifier` (yarn `--mutex network:port`) and `k:v,k:v` lists.
- **`Policy`** needs: `KeyedMap` (later-same-key-wins, for `--define`/`--env`/`--alias`),
  `CrossFlagSet` (`--omit`/`--include` editing one set), and order-significant-keep-all for
  preload/plugin pipelines (`-r`, `--plugin`, `--inject`).
- **`Kind`** needs: `PathOutDir` (distinct from file), `Url`/`UrlBase` (vite `--base`,
  `--public-path` — looks like path, must NOT be mounted/hashed), `ResponseFile`,
  `ModuleSpecifier` (bare-name-or-path), and `PackageSpec` (registry|git|path tri-state).
- **Conditional grammars**: tsc `-b`, node modal positionals, bun file-vs-command — the model
  needs flags that gate the validity of the rest of the grammar.
- **Dynamic subcommand keys**: `npm/pnpm/yarn/bun run <script>`, `yarn workspace <name>`,
  plugin commands — subcommands map must support data-defined keys + delegation.
