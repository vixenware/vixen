# Command Study — Windows System / Packaging Tool Group

Tools: **msiexec**, **signtool**, **WiX candle / light**, **powershell**, **reg**.

Studied against the Vixen tool grammar:

```
struct Grammar { flags: [Flag], positionals: [Positional], subcommands: Map<String,Grammar> }
Flag { lead, spelling: Spelling, policy: Policy, kind: Kind }
enum Spelling { Bare, Attached, Separated, Equals, Passthrough(sep) }
enum Policy   { Once, Group(name), DedupUnordered, DedupOrdered, Positional }
enum Kind     { Str, PathIn(dir?), PathOut, Include, Lib, Define }
```

Two non-negotiable facts dominate this group and break the model harder than the GCC/clang family:

1. **The lead character is `/` (and `-`), and the *value-attach* convention is `:` not `=`.** `signtool sign /fd sha256`, `reg add ... /t REG_SZ`, `candle -dFoo=Bar`, `light -cultures:en-us`. The Spelling enum has `Equals` (`=`) but no `Colon` form and no `/`-lead awareness. This is pervasive, not occasional.
2. **`msiexec` and `reg` take `NAME=VALUE` *property/data assignments* as bare positionals**, not flags. `msiexec /i x.msi REINSTALL=ALL REINSTALLMODE=ecmus`. These are order-insensitive, case-folding, last-wins-keyed assignments — a `Define`-like Kind but living in *positional* slots, and they are the actual cache-relevant payload.

---

## 1. msiexec

The Windows Installer engine. Not a subcommand tool — it is a **mode-flag tool**: the first action flag selects the verb.

### 1. Invocation shape
No subcommand tree. Dispatch is on a **mutually-exclusive action flag**, not a bare token:
- `/i package` install
- `/x {ProductCode|package}` uninstall
- `/f[pecmsavdou] package` repair (the repair *type letters are attached*, see below)
- `/a package` administrative install
- `/j[um] package` advertise
- `/p patch.msp` apply patch (newer; classically `/update`)
- `/y module`, `/z module` (un)register a DLL

`Grammar.subcommands` (dispatch-on-bare-token) does **not** model this — dispatch is on a flag, and the verbs are mutually exclusive. This is a `Group(name="action")` of flags that *also* changes which positional is expected. Deepest nesting: **none** — flat.

### 2. Spelling forms
- Windows `/FLAG` lead — every switch.
- **Attached letter-bag with semantics**: `/qn`, `/qb`, `/qb!`, `/qr`, `/qf` — `/q` + UI-level letters; `/fpecms` — repair mode letters. These are *attached single chars whose set/order partly matters*. Not `Attached` (that's one value glued on) — this is a **bitfield/letter-set suffix**.
- **Logging mega-flag**: `/l*v file.log`, `/lvoicewarmupx file.log` — `/l` + a *set of log-level letters* + `*` (= all) + then a **separated path**. So one flag = letter-set suffix **plus** a `PathOut` operand.
- Property assignments as **bare positionals**: `PROP=VALUE`, `PROP="quoted value"`, and `MyVar="a;b;c"`. Case-insensitive property names; **public properties must be UPPERCASE** to survive to a server install.
- `/qn` no equals; no response-file form (msiexec has no `@argfile`).

Forms the enum lacks: **letter-set/bitfield suffix** (`/qb!`, `/l*v`), and **`/flag` + separated path in one token group**.

### 3. Multiplicity & order
- Action flags (`/i /x /f ...`): `Group("action")`, last-wins but really *exclusive* — supplying two is an error, not last-wins.
- `/q` UI level: `Once` (last-wins).
- `/l` logging: `Once` for the path; the letter-set is a `DedupUnordered` set (`/livecwarmupx` order is irrelevant, MS docs say case-insensitive & order-independent).
- **Property assignments**: order-insensitive, **last assignment of a given property wins**, names case-fold for matching. This is exactly a `DedupUnordered`-keyed map — but on *positionals*. `Policy::Positional` is defined as "never reorder/dedup," which is the **opposite** of what property assignments need.

Missing: a **keyed-assignment positional policy** (dedup by LHS, last-wins, canonical-sort by key). Also `REINSTALLMODE=ecmus` is itself an order-independent letter-set *value* — a sub-language inside a positional.

### 4. Path-bearing slots
- The package operand after `/i`/`/a`/`/x` (can be a path **or** a `{GUID}` ProductCode — same slot, two meanings → not always a path).
- `/p patch.msp` — PathIn.
- `/l*v <log>` — **PathOut** (sneaky: glued to the logging flag).
- Property *values* are frequently paths: `INSTALLDIR=C:\Foo`, `TRANSFORMS=a.mst;b.mst` (**`;`-list of PathIn**), `PATCH=x.msp`. None are syntactically marked — you must know the property semantics.
- `/t transform.mst` for admin installs — `;`-list of PathIn.

`Kind` does not cover: **GUID-or-path union operand**, and **`;`-separated path lists hidden in a positional value**.

### 5. Model-breakers
- Property values that are path-lists (`TRANSFORMS=a.mst;b.mst`) — a path-list sub-language inside a `Str` positional.
- The package operand is a **tagged union** (path | ProductCode GUID); canonical form differs (hash the file vs. literal GUID).
- `/f` repair-type letters change *which files get touched* — flag meaning is a flag-internal sub-grammar.

### 6. Caching hazards
- `/l*v <log>` writes a log whose **content includes timestamps, machine name, absolute temp paths** — pure side-channel; must be excluded from any output hashing and ideally normalized to a fixed path for determinism.
- ProductCode GUIDs are install-instance identity — cache key must treat GUID operands as opaque literals, never as content.
- Absolute `INSTALLDIR=C:\...` leaks machine layout into the key.

---

## 2. signtool

Authenticode signing. **Two-level: a verb subcommand, then flags.** This is the closest fit to the recursive grammar in the group.

### 1. Invocation shape
`signtool <verb> [flags] <file>...`. Verbs: `sign`, `timestamp`, `verify`, `catdb`, `remove`, `sign /dg` (digest staging). Depth = **2** (verb → flags + file positionals). `Grammar.subcommands` covers the verb dispatch cleanly; the only nesting beyond 2 is the staged `/dg → /ds → /di` three-call signing dance, which is a *workflow* across processes, not nested argv.

### 2. Spelling forms
- Windows `/FLAG` lead.
- **Separated value, colon-free**: `/fd sha256`, `/f cert.pfx`, `/p <password>`, `/n "Subject Name"`, `/sha1 <thumbprint>`, `/tr <url>`, `/td sha256`, `/d "desc"`, `/du <url>`.
- **Bare boolean flags**: `/a` (auto-select cert), `/as` (append signature), `/debug`, `/q`, `/v`, `/ph` (page hashes), `/fd certHash`.
- `/dlib <dll>` + `/dmdf <file>` — separated paths.
- No `=`, no `@argfile`, no `--`. Counts: none (`/v` verbose is boolean, not `-vvv`).

Forms the enum lacks: nothing new structurally (these are all `Separated`/`Bare`), but note **case-insensitive flag names** and **`/` lead** again.

### 3. Multiplicity & order
- Most flags: `Once`.
- **`/as` (append signature)**: enables *multiple signatures on one file*. Run signtool repeatedly with `/as` — each invocation adds a sig. Within one argv `/as` is `Once` (boolean), but the *semantics* are accumulate-across-invocations — invisible to single-argv canonicalization.
- Cert-selection flags are a **mutually-exclusive group**: `/f <pfx>` vs `/sha1 <hash>` vs `/n <name>` vs `/a` — exactly `Group("certselect")`, last-wins/exclusive.
- File positionals: `Positional`, order is irrelevant to signing but should not be reordered for lint clarity.

Missing: **append-across-invocations** is genuinely outside a per-argv model.

### 4. Path-bearing slots
- `/f cert.pfx` — PathIn (and **content-bearing secret** — see hazards).
- `/dlib *.dll`, `/dmdf metadata` — PathIn.
- `/ac <crosscert>` — PathIn.
- trailing `<file>...` — **PathIn AND PathOut**: signtool *mutates the file in place*. A single operand is both consumed and produced. `Kind` has `PathIn` xor `PathOut`; here we need **PathInOut (mutated-in-place)**.

### 5. Model-breakers
- **In-place mutation** of the target file: input == output. Neither `PathIn` nor `PathOut` captures "this path is read, then overwritten."
- `/tr <url>` / `/t <url>` — a **network URL** in a path-shaped slot (timestamp server). Not a filesystem path, not literal `Str` for cache purposes (contacting it is impure). New Kind: **Url(impure-network)**.
- `/dlib` selects a DLL that *implements the signing primitive* — the flag's value changes the meaning of the rest of the command (cloud-HSM signing). Conditional grammar.

### 6. Caching hazards
- **Timestamping (`/tr`,`/t`) injects a server-supplied timestamp** into the output — nondeterministic, network-dependent. Output bytes change run-to-run. The canonicalizer cannot make a timestamped signature reproducible; must **mark impure**.
- The signature itself embeds a fresh value each run even without timestamp (PKCS#7 signing is deterministic only for the digest, but the structure includes signing-time unless suppressed).
- `/p <password>` is a **secret in argv** — must be redacted from cache keys and logs, never hashed verbatim into a shareable key.
- `/f cert.pfx` content (private key) must not leak into a distributable cache key.

---

## 3. WiX candle.exe (compiler) and light.exe (linker)

WiX v3 toolchain. `.wxs` → (candle) → `.wixobj` → (light) → `.msi`. Mirrors a C compile/link split, so it stresses the `Define`/`Include`/`Lib` kinds the most. (WiX v4+ collapses both into `wix build`; v3 candle/light is studied here.)

### 1. Invocation shape
Both flat: `candle [opts] file.wxs ...` and `light [opts] file.wixobj ...`. No subcommands. Depth = **1**. `Grammar.subcommands` unused.

### 2. Spelling forms
- **`-` lead but `/` accepted** (Windows tools accept both).
- **Attached define, value via `=`**: `-dName=Value` (candle preprocessor var), `-dWixVar=Value` (light). This is `Attached` lead + `Equals` value glued in **one token** — a combination of two Spelling variants.
- **Colon-separated lists**: `-cultures:en-us;fr-fr` (light), `-sice:ICE61`. The value attaches with **`:`** and is itself a `;`-list. Spelling enum has no `:`-attach.
- Separated: `-out output.msi`, `-ext WixUtilExtension`, `-b basepath`, `-loc file.wxl`, `-arch x86`.
- Bare toggles: `-nologo`, `-sval` (skip validation), `-ss`, `-sw`, `-sw1234` (suppress specific warning N — **attached numeric**), `-wx` (warnings as errors), `-pedantic`, `-v`.
- `-sw<N>` / `-sice:<id>` — suppress-by-id, an attached/colon id.
- `@responsefile` — **WiX DOES support response files** (`candle @args.rsp`). The enum has no response-file concept at all.

Forms the enum lacks: **`:`-attached value**, **response-file `@file`**, **attached-numeric suffix** (`-sw1011`).

### 3. Multiplicity & order
- `-d Name=Value`: **DedupUnordered keyed by Name, last-wins** (define semantics). Maps to `Define` kind + a keyed dedup — same need as msiexec properties.
- `-ext <assembly>`: **DedupOrdered** — extensions are loaded in order, duplicates pointless but order can affect symbol resolution. Accumulates.
- `-b <basepath>` (light): **accumulates, order is search-order-significant** — exactly `DedupOrdered`, like `-I`/`-L`. Multiple `-b` allowed.
- `-loc <wxl>`: accumulates, order can matter for string override precedence — `DedupOrdered`.
- `-cultures:a;b;c`: ordered list (precedence) in a single token — order semantic.
- `-sw<N>`, `-sice:<id>`: `DedupUnordered` set.
- `-out`, `-arch`, `-nologo`: `Once`.

Missing: none catastrophic, but `-ext`/`-b`/`-loc` confirm `DedupOrdered` is mandatory, and the keyed-define dedup again wants a map policy.

### 4. Path-bearing slots
- `file.wxs` / `file.wixobj` positionals — **PathIn**.
- `-out <file>` — **PathOut** (and for light, the `.wixpdb` sibling is an *implicit* PathOut written next to it).
- `-b <basepath>` — **PathIn(dir)** search root → exactly `Include`/`Lib` semantics (search dir, content-affecting).
- `-ext <assembly>` — PathIn (a DLL) OR a bare assembly name resolved from a well-known dir — **path-or-name union** like signtool/msiexec.
- `-loc <wxl>` — PathIn.
- `-pdbout <file>`, `-contentsfile`, `-outputsfile`, `-builtoutputsfile` (light tracking files) — **PathOut**.
- `-d` define *values* are frequently paths (`-dMyDir=src\images`) — path hidden in a Define value. `Kind::Define` is treated as literal; here the value is path-bearing and **content should be hashed**.

`Kind` gap: **Define whose value is a path** (content-affecting, not literal). And `-b` is a search dir that fits `Include`/`Lib` but isn't C-flavored — argues for a generic `SearchDir` kind.

### 5. Model-breakers
- `-dFoo=path` — a define whose value is a path: the cache key must hash file content, but the model classifies it as literal `Define`. Conditional kind (depends on the *named* variable's role, which only the WiX author knows).
- `-ext` path-or-assembly-name union.
- light reads **all** the `.wixobj` inputs plus files referenced *inside* them (via `-b` resolution) — the true input set is not visible in argv; it's discovered by resolving `File/@Source` against base paths. Cache key must include transitively-resolved files, which argv alone cannot enumerate.

### 6. Caching hazards
- **Package/Product GUIDs**: `<Product Id="*">` auto-generates a fresh GUID each build → nondeterministic MSI. Must be pinned to make builds reproducible, else mark impure.
- **MSI summary stream embeds a build timestamp** and a per-build PackageCode GUID → output never byte-identical. Canonicalizer cannot fix; impure unless WiX `-reusecab`/fixed-GUID flags used.
- `.wixpdb` and CAB cabinet ordering: cabinet compression can embed timestamps and order-dependent layout.
- `-b` absolute base paths leak machine layout into resolution.

---

## 4. powershell (Windows PowerShell / pwsh)

The shell host as a CLI. The hard case: **the value of `-Command` / `-File` is itself an entire other language.**

### 1. Invocation shape
Flat host flags, then a **terminal flag that swallows the rest of the line**:
- `-File <path> [args...]` — run a script; everything after is *script args*.
- `-Command { ... }` / `-Command "<string>"` — run inline PS; **everything after `-Command` is the command** (in `-Command` mode the parser stops treating tokens as host flags).
- `-EncodedCommand <base64>` — base64-UTF16LE of a script.

Depth: **1** at the host level, but `-Command`/`-File`/`-EncodedCommand` open a **nested sub-language of unbounded depth** (a whole PowerShell program, which itself can invoke more powershell). `Grammar.subcommands` cannot model "rest of argv is a foreign language."

### 2. Spelling forms
- **`-Flag` lead, but separated value with a space, and flags are PREFIX-ABBREVIATED and case-insensitive**: `-ExecutionPolicy Bypass` == `-ep Bypass` == `-exec Bypass`. Any unambiguous prefix is accepted. This is **prefix-abbreviated aliasing** — a single canonical flag has *infinitely many spellings*. The Spelling/Policy model has no notion of "this token is an abbreviation of that flag."
- `-NoProfile` / `-nop`, `-NonInteractive` / `-noni`, `-WindowStyle Hidden`, `-NoLogo`, `-Sta`/`-Mta`.
- `-EncodedCommand <b64>` — opaque encoded payload.
- `--%` (the **stop-parsing token**) — passes everything after verbatim to a native command. Like `--` end-of-flags but stronger (no further PS parsing at all).
- `-File -` reads script from stdin.

Forms the enum lacks: **prefix-abbreviated flag matching**, **`--%` stop-parsing**, **base64-encoded command payload**.

### 3. Multiplicity & order
- Host flags: `Once` mostly. `-ExecutionPolicy`: `Once`.
- **Order matters absolutely**: `-File` / `-Command` / `-EncodedCommand` are terminal — the *position* where they appear ends host-flag parsing. A flag after `-Command` is not a flag. `Policy` has no "terminal flag that ends parsing" concept.
- Script args after `-File` are pure `Positional` (passed through untouched).

Missing: **terminal/parsing-mode-switching flag**.

### 4. Path-bearing slots
- `-File <script.ps1>` — PathIn.
- `-File -` — stdin sentinel (not a path).
- `-ConfigurationName`, `-WorkingDirectory <dir>` — PathIn(dir).
- `-OutputFormat`, `-InputFormat` — not paths.
- Paths inside `-Command` strings are **invisible** to argv-level analysis.

`Kind` gap: **`-` stdin sentinel in a path slot**; paths buried in an opaque command string are unrecoverable.

### 5. Model-breakers
- `-Command "<arbitrary PowerShell>"` / `-EncodedCommand <b64>` — the payload is a **complete foreign program**. No grammar over argv can canonicalize its meaning; the model would have to recurse into a PowerShell parser. This is the strongest model-breaker in the group: a flag value that is a Turing-complete sub-language.
- **Prefix abbreviation**: `-Exec`, `-Execu`, `-ExecutionP` all bind to `-ExecutionPolicy`. Canonicalization to one spelling requires a *full flag table + longest-prefix disambiguation*, which the static Spelling enum cannot do.
- `--%` switches the *entire remaining parse* to verbatim mode.

### 6. Caching hazards
- `-EncodedCommand` base64 of UTF-16LE: two byte-different base64 strings can decode to the *same* script (padding/whitespace) → canonicalizer must decode-and-renormalize to dedupe cache keys.
- A `-Command` payload reading `$env:`, `Get-Date`, `$PID`, `[guid]::NewGuid()`, network, or `$RANDOM`-equivalent is intrinsically impure — undetectable from argv.
- `-WorkingDirectory` makes otherwise-identical commands resolve different relative paths → key must capture cwd.

---

## 5. reg.exe

Registry console tool. Verb subcommand + key path + colon-and-space flags.

### 1. Invocation shape
`reg <verb> <KeyName> [flags]`. Verbs: `add`, `query`, `delete`, `copy`, `save`, `restore`, `load`, `unload`, `compare`, `export`, `import`, `flags`. Depth = **2** (verb → key + flags). `Grammar.subcommands` covers verb dispatch. The first **positional after the verb is the registry key path** (`HKLM\Software\Foo`), not a flag.

### 2. Spelling forms
- Windows `/FLAG` lead.
- **Separated values**: `/v ValueName`, `/t REG_SZ`, `/d <data>`, `/s <separator>`, `/se <sep>`, `/f <pattern>`.
- **Bare**: `/ve` (default/empty value name), `/f` (force, in `add`/`delete` — **same `/f` is "force" in add but "search pattern" prefix elsewhere → context-dependent flag meaning**), `/s` (recurse in query/delete vs. "separator" in some), `/y`.
- **Suffix-style architecture selector**: `/reg:32` / `/reg:64` — `:`-attached fixed value (a colon-attach, again).
- Registry **key path positional** uses `\` separators and a hive prefix (`HKLM`, `HKEY_LOCAL_MACHINE`, optionally `\\machine\HKLM\...` for remote).

Forms the enum lacks: **`/reg:32` colon-attach**, and **a flag letter whose meaning depends on the verb** (`/f`, `/s`).

### 3. Multiplicity & order
- `/v`, `/t`, `/d`, `/f`: `Once` per invocation.
- `/d <data>` for `REG_MULTI_SZ` is a **`\0`-separated (default) multi-string list** — a sub-list inside one value, with a *configurable* separator via `/s`. So `/s` *reconfigures how `/d` is parsed* — a flag that changes another flag's grammar.
- `/reg:32` vs `/reg:64`: `Group("regview")`, exclusive.
- Key path positional: `Positional`, `Once`.

Missing: **flag-modifies-another-flag's-parsing** (`/s` ↔ `/d`); **verb-dependent flag semantics** (`/f`, `/s`).

### 4. Path-bearing slots
- `export <KeyName> <file.reg>` / `import <file.reg>` — **second positional is a PathOut (export) / PathIn (import)**.
- `save <KeyName> <file.hiv>` — PathOut (hive); `restore <KeyName> <file.hiv>` — PathIn.
- `load <KeyName> <file.hiv>` — PathIn.
- The **registry key path is itself a hierarchical "path" in a non-filesystem namespace** — content-addressable in spirit (the key + its subtree is the real input/output) but not a file. New Kind: **RegPath (non-FS hierarchical namespace)**.
- `/d` data values can contain filesystem paths (e.g. install location) — literal `Str` but semantically a path.

`Kind` gap: **registry key path** (hierarchical, non-filesystem), and the positional file-slot whose **direction flips with the verb** (export=out, import=in, same shape).

### 5. Model-breakers
- **Verb-dependent flag meaning**: `/s` = recurse (query/delete) vs. separator-spec (add); `/f` = force (add/delete) vs. find-pattern (query). The flag's *kind and policy depend on the chosen subcommand* — the static per-flag definition doesn't hold; you need per-subcommand flag tables (which the recursive Grammar *does* allow, since each subcommand carries its own `flags` — good, but the model must not share a global flag table).
- **`/s` reconfigures `/d` parsing** (separator for `REG_MULTI_SZ`) — flag-to-flag dependency.
- The key path is a namespace, not a file; its "content" (subtree) is what matters for caching.

### 6. Caching hazards
- `reg query` output ordering is **enumeration-order-dependent**, not sorted — must be canonically sorted to be a stable cache input.
- `export` writes a `.reg` file with a **version header line** (`Windows Registry Editor Version 5.00`) and possibly volatile values; content not stable across OS versions.
- Registry reads pull live machine state (`HKCU`, install GUIDs, timestamps in value data) — intrinsically impure inputs; reading the same key on two machines differs.
- Remote key paths (`\\machine\...`) make the result host-dependent.

---

## Cross-tool synthesis

### Novel spellings the enum lacks
- **`/FLAG` Windows lead** with **`:`-attached value** (`/reg:32`, `-cultures:en-us`, `-sice:ICE61`) — no `Colon` variant exists; `Equals` is `=` only.
- **Letter-set / bitfield suffix** (`/qb!`, `/l*v`, `/fpecms`) — a *set* of single-char modifiers glued to a flag, partly order-independent.
- **Response files** `@args.rsp` (WiX) — entirely absent from the model.
- **Base64-encoded payload** flag (`-EncodedCommand`).
- **Prefix-abbreviated, case-insensitive flag names** (PowerShell `-ep` == `-ExecutionPolicy`) — one canonical flag, unbounded spellings.
- **`--%` stop-parsing** and **`-Command`/`-File` terminal flags** that switch parse mode.

### Novel policies the enum lacks
- **KeyedAssign** (dedup by LHS, last-wins, sort-by-key) for `NAME=VALUE` — needed by msiexec properties (as *positionals*) and WiX `-d` defines. `Positional` says "never reorder/dedup" — the exact opposite.
- **Exclusive** (true mutual exclusion = error on conflict) distinct from `Group` (last-wins): msiexec action flags, signtool cert-selection.
- **AccumulateAcrossInvocations** (signtool `/as` multi-sign) — outside single-argv scope entirely.
- **VerbDependent** flag semantics (reg `/s`,`/f`) — policy/kind not fixed until subcommand known.

### Novel kinds the enum lacks
- **PathInOut** (mutated-in-place) — signtool signs the file argument in place.
- **Url(impure-network)** — signtool `/tr`,`/td` timestamp servers, PowerShell remoting.
- **RegPath** — hierarchical non-filesystem namespace (reg key path).
- **PathOrName union** — `-ext WixUtilExtension`, msiexec `{GUID}`-or-path, resolved against well-known dirs.
- **DefinePath** — a `-d`/property value that is itself a content-bearing path, not literal.
- **PathList** — `;`-separated path list inside one value (`TRANSFORMS=a.mst;b.mst`, `-cultures:`, `REG_MULTI_SZ`).
- **Stdin sentinel `-`** in a path slot (PowerShell `-File -`).

### The genuine model-breakers
1. **PowerShell `-Command`/`-EncodedCommand`**: flag value is a complete foreign (Turing-complete) program. No argv grammar can canonicalize it.
2. **Property/define values that are themselves sub-languages** (`REINSTALLMODE=ecmus` letter-set; `TRANSFORMS=a;b` path-list) — nested grammar inside a positional.
3. **WiX link-time input discovery**: true inputs (files referenced inside `.wixobj` resolved via `-b`) are not in argv.
4. **Verb-dependent and flag-dependent flag meaning** (reg `/s`,`/f`; `/s` reconfigures `/d`) — no fixed per-flag definition.
5. **In-place mutation** (signtool) — input and output are the same path.

### The caching hazards (canonicalizer must neutralize or mark impure)
- **Embedded build timestamps + auto GUIDs** in MSI (WiX `Product Id="*"`, PackageCode, summary stream) — impure.
- **Authenticode timestamping** (signtool `/tr`) injects server time into output — impure, network-dependent.
- **Secrets in argv** (signtool `/p <password>`, `/f cert.pfx`) — must be redacted from keys/logs, never hashed verbatim.
- **Log side-channels** (msiexec `/l*v`) — timestamps, machine name, temp paths in the log.
- **Live machine state** (reg reads `HKCU`/install GUIDs; PowerShell `$env:`/`Get-Date`/`[guid]::NewGuid()`) — impure inputs.
- **Non-deterministic ordering** (reg query enumeration order; MSI cabinet layout) — must canonically sort.
- **Absolute-path leakage** (msiexec `INSTALLDIR=C:\...`, WiX `-b`, PowerShell `-WorkingDirectory`) — leaks machine layout into the key.
- **EncodedCommand non-canonical base64** — distinct strings decode to identical scripts; must decode+renormalize.
