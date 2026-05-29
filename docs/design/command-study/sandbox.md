# Command study: Sandbox / exec wrappers

Tools: `sandbox-exec` (macOS), `bwrap` (bubblewrap), `firejail`, `landlock-exec`
(modeled on the landrun/sandlock family of minimal Landlock+seccomp wrappers).

The model under test:

```
Grammar { flags: [Flag], positionals: [Positional], subcommands: Map<String,Grammar> }
Flag { lead, spelling: Spelling, policy: Policy, kind: Kind }
Spelling = Bare | Attached | Separated | Equals | Passthrough(sep)
Policy   = Once | Group(name) | DedupUnordered | DedupOrdered | Positional
Kind     = Str | PathIn(dir?) | PathOut | Include | Lib | Define
```

The whole tool class shares a structural trait that strains the model harder than
compilers do: **every one of these tools is a launcher.** The argv is
`[wrapper-flags...] -- COMMAND [COMMAND-ARGS...]`, and everything right of the
program token is *the inner command's argv*, which is opaque to this grammar and
must be carried verbatim. The wrapper grammar therefore has a hard "stop parsing,
the rest is payload" boundary that is **not** the same as the `--` end-of-flags
convention (firejail and bwrap both treat the first non-flag token specially, and
the inner argv may itself contain `--`).

---

## 1. `sandbox-exec` (macOS, DEPRECATED but still shipped)

### 1.1 Invocation shape

Flat, no subcommands.

```
sandbox-exec [-f profile-file] [-n profile-name] [-p profile-string] [-D key=value ...] command [arguments ...]
```

`Grammar.subcommands` is unused (empty map). Depth 0. The "subcommand-like"
dispatch is really the *inner command* — `command [arguments...]` is a foreign
argv, not a Vixen subcommand. The recursive `subcommands` map cannot model this,
because the inner program is not a fixed keyword set — it is an arbitrary path.
This is the **program-token / payload boundary** noted in the preamble.

### 1.2 Spelling forms

| Flag | Spelling | Notes |
|------|----------|-------|
| `-f profile-file` | Separated | path to a `.sb` profile |
| `-n profile-name` | Separated | named builtin profile (`no-internet`, `pure-computation`, `no-network`, …) |
| `-p profile-string` | Separated | **inline SBPL profile as a single string** |
| `-D key=value` | Separated, repeatable | profile parameter binding |

All single-dash, all `Separated`. No attached/equals/bare forms. `-D` is
`-D` `key=value` as **two** argv tokens (separated), but the *value* token has
internal `key=value` structure — an Equals-shaped payload inside a Separated slot.
The Spelling enum has no way to say "Separated slot whose value is itself `k=v`".

`-f`/`-n`/`-p` are **three aliases for one logical slot** (the profile source).
Exactly one should win; they are mutually exclusive *across distinct spellings*,
which `Group` half-covers (see §1.3).

### 1.3 Multiplicity & order

- `-f` / `-n` / `-p`: **mutually-exclusive group, last-wins** — but the group
  members have *different flag letters*, not different values of one flag. This
  is `Group(name="profile-source")` spanning three separate `Flag` entries.
- `-D key=value`: a **map accumulator**. Multiple `-D` build a dict; later `-D` of
  the same key overrides. Canonical form = sort by key, dedup last-wins. This is
  **neither `DedupUnordered` (a set of opaque strings) nor `Once`** — it is a
  *keyed* dedup. The Policy enum has no `KeyedMap` / `DedupByKey` variant. Treating
  `-D` as `DedupUnordered` over the raw `key=value` strings is **wrong**: `-DA=1`
  and `-DA=2` are not distinct set members, the second must shadow the first.

### 1.4 Path-bearing slots

- `-f profile-file` → `PathIn` (and **content-hashed**: the profile text is the
  real input, the path is incidental).
- `-D key=value` → the value is frequently a path (`-D HOME=/Users/amos`,
  `-D DIR=/tmp/x`). But which keys are paths is **profile-dependent** — there is no
  way to know from the argv alone whether `-D FOO=/x` is a path or a literal. Kind
  cannot be assigned statically.
- `-p profile-string` → contains paths *inside the SBPL sub-language*
  (`(allow file-read* (subpath "/usr/lib"))`). These are real cache inputs but live
  inside an opaque Scheme-like string.

`Kind` covers `-f` as `PathIn`. It does **not** cover "path embedded in a
parameter value selected by a key the grammar can't see" nor "paths inside an SBPL
string."

### 1.5 Model-breakers

- **SBPL sub-language.** `-p` carries a full Scheme-dialect policy
  (`(version 1) (deny default) (allow file-read* (subpath "/x"))`). This is a
  nested grammar far richer than `-Wl,a,b` passthrough — it has balanced parens,
  string literals, comments. `Passthrough(sep)` (split-on-separator) cannot parse
  it. `-f` points at the *same language in a file*. Canonicalizing SBPL (sorting
  allow-rules? normalizing whitespace?) is a whole sub-canonicalizer.
- **Param substitution coupling.** `-D` values are textually substituted into the
  SBPL profile (`(param "DIR")`). The *meaning* of a `-D` (path vs literal) depends
  on how the profile uses it — a flag's kind depends on another argument's content.
- Profile-source group members are different flags, not different values — a
  cross-flag `Group`.

### 1.6 Caching hazards

- `-p`/`-f` profiles routinely embed **absolute host paths** (`/Users/amos/...`,
  `$HOME`) → absolute-path leakage into the cache key.
- `-D` values are the canonical injection point for **machine-specific paths and
  tmpdirs** (`-D TMP=/var/folders/xx/…`). Must be normalized or the key is
  per-machine / per-run.
- Named profiles (`-n`) resolve against the **OS version's** builtin profile set —
  same name, different effective policy across macOS releases. The name is not a
  stable cache input on its own.

---

## 2. `bwrap` (bubblewrap)

### 2.1 Invocation shape

Flat wrapper flags, then `COMMAND [ARGS...]`. No subcommands.

```
bwrap [OPTION...] [--] COMMAND [ARGS...]
```

Depth 0 for `subcommands`. Same launcher boundary as sandbox-exec: the first
non-option token is the program; the rest is its argv. `--` is accepted as an
explicit end-of-flags but bwrap will also stop at the first bare token.
**Crucially, filesystem ops are applied in argv order** (see §2.3) — so this is a
flat grammar whose *positionals' order is load-bearing among themselves*.

### 2.2 Spelling forms — the richest of the group

All flags are GNU-long, all `Separated`, but with **variable arity 0/1/2/3**:

| Arity | Examples |
|------:|----------|
| 0 | `--unshare-all`, `--unshare-net`, `--unshare-pid`, `--share-net`, `--clearenv`, `--new-session`, `--die-with-parent`, `--as-pid-1`, `--disable-userns`, `--level-prefix` |
| 1 | `--chdir DIR`, `--hostname H`, `--uid N`, `--gid N`, `--proc DEST`, `--dev DEST`, `--tmpfs DEST`, `--dir DEST`, `--mqueue DEST`, `--remount-ro DEST`, `--cap-add CAP`, `--cap-drop CAP`, `--perms OCTAL`, `--size BYTES`, `--argv0 VAL`, `--seccomp FD`, `--userns FD`, `--args FD`, `--unsetenv VAR` |
| 2 | `--bind SRC DEST`, `--ro-bind SRC DEST`, `--dev-bind SRC DEST`, `--bind-try`, `--ro-bind-try`, `--dev-bind-try`, `--symlink TARGET LINK`, `--setenv VAR VAL`, `--chmod OCTAL PATH`, `--file FD DEST`, `--bind-data FD DEST`, `--ro-bind-data FD DEST` |
| 3 | `--overlay RWSRC WORKDIR DEST`, `--ro-overlay DEST` (+ preceding `--overlay-src`), `--tmp-overlay DEST` |

**Forms the Spelling enum lacks:**

- **Multi-token values (arity 2 and 3).** `Separated` is implicitly one value. A
  `--bind SRC DEST` flag has *two ordered operands of different Kind* (PathIn,
  PathOut-ish). `--overlay` has three. There is no `Separated2`/`SeparatedN` /
  tuple-valued spelling. This is the single biggest gap for bwrap.
- **FD-valued options** (`--args FD`, `--seccomp FD`, `--userns FD`,
  `--file FD DEST`, `--bind-data FD DEST`). The value is a small integer naming an
  *inherited file descriptor*, not a path or literal. Its referent (the data behind
  the FD) is the real cache input but is **not present in argv at all**.
- **`--args FD` is a response-file mechanism**: it reads further arguments
  (NUL-separated) from that FD and splices them into the argv stream. This is
  `@argfile` semantics but sourced from an FD, not a path. The grammar must know
  that one flag *injects more argv*.

`--perms OCTAL` / `--chmod OCTAL PATH` / `--size BYTES` are "modifier prefix" flags:
`--perms 0700 --dir /x` means the *next* fs op uses those perms. The perms flag's
value attaches to a later, separate flag.

### 2.3 Multiplicity & order — order is the whole semantics

- **Filesystem ops are applied strictly in argv order, and order is meaningful.**
  `--bind / / --ro-bind /etc /etc` differs from the reverse. Two `--ro-bind` to the
  *same DEST* → **last-wins by overlay order**. This is `Positional`-like (never
  reorder) but expressed through *flags*, not bare positionals. **Canonicalization
  must NOT sort or dedup bind ops.** Any `Dedup*` policy here is actively wrong.
- `--unshare-*` flags: a **set** (idempotent, order-irrelevant) →
  `DedupUnordered` fits. `--unshare-all` then `--share-net` is order-sensitive
  re-enable, though — a negation pattern.
- `--setenv VAR VAL` / `--unsetenv VAR`: a **keyed map with deletes**. Last
  setenv of a VAR wins; an unsetenv after a setenv deletes it. Same `KeyedMap`
  gap as sandbox-exec `-D`, plus **negation/delete entries interleaved**.
- `--perms`/`--size` are **sticky modifiers consumed by the next op** — a flag
  whose effect is scoped to the following flag. No Policy expresses "modifies the
  next flag."
- `--cap-add` / `--cap-drop`: accumulate-with-negation over a capability set,
  exactly the `-Wfoo`/`-Wno-foo` shape, across two different flag names.

### 2.4 Path-bearing slots

Pervasive. `--bind/--ro-bind/--dev-bind SRC DEST`: SRC = `PathIn` (host,
content-hashed), DEST = in-sandbox path = a **PathOut-but-virtual** (it is a mount
point inside the namespace, neither a real input nor a written artifact). `--dir`,
`--tmpfs`, `--proc`, `--dev`, `--symlink`'s LINK, `--chdir`, `--remount-ro` all take
**sandbox-virtual paths** that should NOT be content-hashed (they don't exist on the
host yet) but DO matter structurally.

**New Kind needed:** `MountPoint` (a.k.a. virtual/in-sandbox destination path):
canonicalized as a path *string* (normalize `.`/`..`/trailing slash) but **never
mounted or content-hashed** — distinct from both `PathIn` (hash the bytes) and
`PathOut` (an artifact the run produces). Also a `Fd` kind for FD-valued slots whose
referent is out-of-band.

### 2.5 Model-breakers

- **Tuple-valued flags (arity ≥2)** — see §2.2. The model is fundamentally
  one-value-per-flag.
- **Sticky/positional modifier flags** (`--perms`, `--size`, `--overlay-src`
  chains): the flag modifies a *subsequent* flag. `--overlay-src A --overlay-src B
  --ro-overlay DEST` builds an ordered layer stack consumed by the later
  `--ro-overlay`. Cross-flag state machine.
- **Order-significant flag stream**: the entire fs-op sequence is positional even
  though it's spelled as flags. Vixen's `Positional` policy is for bare tokens, not
  flags; nothing says "these flags must retain mutual order and must not dedup."
- **FD-injected argv** (`--args FD`) — argv that doesn't exist in argv.

### 2.6 Caching hazards

- **`--bind-data`/`--file`/`--args`/`--seccomp` reference FDs** — the cache key
  must hash the *bytes behind the FD*, which are not in argv. Without that, two runs
  with identical argv but different piped data alias to the same key. Mark impure
  unless the FD contents are captured.
- DEST mount points are stable, but **SRC paths are absolute host paths** →
  leakage; same input tree under a different prefix yields a different key.
- `--tmpfs`, `--proc`, `--dev` create fresh, **nondeterministic** filesystem state
  (random inode numbers, fresh /proc) — harmless to the key but means the sandbox
  output is not reproducible byte-for-byte; the canonicalizer should treat tmpfs
  contents as impure.
- `--hostname`, `--uid`/`--gid` injected into the namespace can leak into program
  output (e.g. build stamps). Canonicalize to fixed sentinel values if hermetic.

---

## 3. `firejail`

### 3.1 Invocation shape

Flat wrapper flags then program; also a set of **standalone "action" flags** that
don't launch anything (`--list`, `--tree`, `--shutdown=name`, `--join=pid`,
`--ls=name path`). No nested subcommands, but the action flags make it *mode-like*:

```
firejail [OPTIONS] [program and arguments]
firejail --list                      # action mode, no program
firejail --join=1234                 # attach to running sandbox
firejail --ls=mybrowser ~/Downloads  # inspect, takes a name + path
```

`subcommands` is empty, but firejail effectively has **mutually exclusive top-level
modes** (launch vs join vs list vs shutdown) selected by which flag appears. The
recursive grammar models keyword subcommands, not "mode chosen by presence of one of
N flags." Closest analog to `Group`, but groups are for *values* of one slot.

### 3.2 Spelling forms — both `Equals` and `Separated`, same flag

Firejail's hallmark: most long options use **`--flag=value`** but many also accept
bare or separated forms, and there are dozens of arity-0 toggles.

| Form | Examples |
|------|----------|
| Bare toggle (arity 0) | `--private`, `--seccomp`, `--noroot`, `--nonewprivs`, `--nosound`, `--no3d`, `--x11`, `--apparmor`, `--caps.drop=all` |
| Equals, one value | `--profile=name`, `--net=eth0`, `--ip=10.0.0.5`, `--hostname=h`, `--name=mybox`, `--private=~/dir`, `--dns=1.1.1.1`, `--cpu=0,1`, `--rlimit-nofile=1024`, `--timeout=01:30:00` |
| Equals, **list value** | `--whitelist=~/work`, `--read-only=~/x`, `--blacklist=/boot`, `--noblacklist=/x`, `--bind=/a,/b`, `--env=VAR=val`, `--caps.drop=chown,fowner` |
| Separated (2 tokens) | `--ls=name path` (name via `=`, **path as next argv token**), `--get=name path`, `--put=name src dest` |

**Forms the enum lacks / stresses:**

- **Dual-spelling same flag**: `--private` (bare) vs `--private=~/dir` (equals).
  The Spelling enum forces one variant per Flag. Firejail has **optional-value**
  flags: present-without-value means one thing, present-with-value means a refined
  thing. No `OptionalEquals` variant.
- **Dotted flag names as namespaces**: `--caps.drop=`, `--caps.keep=`, `--seccomp.drop=`,
  `--seccomp.keep=`, `--protocol=unix,inet`. The `.` sub-key is a structured name,
  not in the Lead/Spelling model.
- **Equals-value that is itself `VAR=val`** (`--env=DISPLAY=:0`): Equals slot whose
  payload is another Equals pair (same shape as sandbox-exec `-D`).
- **Mixed equals+positional** (`--put=name src dest`): name bound by `=`, then
  **two** following argv tokens. Hybrid of Equals and arity-2 Separated. Nothing in
  the enum mixes attachment styles within one flag.
- **`--profile=` is a profile sub-language file** (firejail's own `.profile`
  syntax: `blacklist`, `whitelist`, `caps.drop`, `seccomp`, `include other.profile`),
  another nested grammar like SBPL (§3.5).
- **Time-encoded values** (`--timeout=hh:mm:ss`) — Str by shape but with internal
  structure the canonicalizer might normalize.

### 3.3 Multiplicity & order

- `--whitelist`, `--read-only`, `--blacklist`, `--noblacklist`, `--bind`,
  `--read-write`, `--tmpfs`, `--mkdir`, `--mkfile`: **accumulating path lists.**
  Each repeatable AND each value may be comma-joined. Order **matters** for
  whitelist/blacklist interaction (`--noblacklist` must precede the `--blacklist`
  that would otherwise hide it; whitelist mounts tmpfs over the top dir, so two
  whitelists under the same top dir interact). → `DedupOrdered`-ish, but with the
  comma-split list expansion the model doesn't do.
- **`--noblacklist` is the negation of `--blacklist`** — accumulate-with-negation,
  across two flag names, exactly the `-W`/`-Wno-` shape, and **order-sensitive**.
- `--profile` / `--noprofile`: profile selection + negation.
- `--net=ethX` repeatable → ordered list of NICs (interface index is positional).
- `--env=VAR=val` repeatable → keyed map (last-wins per VAR) — `KeyedMap` again.
- Many toggles (`--seccomp`, `--noroot`) are idempotent set members → `DedupUnordered`,
  but `--seccomp` (bare) vs `--seccomp.drop=ptrace` (refined) interact.

### 3.4 Path-bearing slots

- `--whitelist`/`--read-only`/`--blacklist`/`--bind`/`--tmpfs`/`--private=DIR`/
  `--chroot=DIR`/`--overlay-named=` → host paths. Mostly `PathIn` semantics but as
  **comma-separated lists** → need a `list-of-PathIn` shape; `Include`/`Lib` are
  single. `--bind=src,dest` is a *pair* inside one equals value.
- `--ls`/`--get`/`--put` take paths *inside an already-running sandbox* — paths
  relative to a foreign mount namespace, not the host. Neither PathIn nor PathOut.
- `--output=logfile` / `--output-stderr=logfile` → genuine `PathOut`.
- `--profile=` / `--profile=path.profile` → `PathIn` + sub-language + `include`
  directives that pull in **further files transitively** (like `-I` include graph,
  but resolved by firejail, not visible in argv).

### 3.5 Model-breakers

- **Optional-value flags** (`--private` vs `--private=dir`) — value presence
  changes meaning.
- **`--profile` sub-language with transitive `include`** — the real input set is a
  *closure* over included profile files, none of which appear in argv.
- **Mode selection by flag presence** (launch/join/list/shutdown/ls) — not keyword
  subcommands.
- **Comma-list values that expand to multiple ordered/negatable entries**, some of
  which are pairs (`--bind=a,b`). One argv token → a structured list the Policy/Kind
  layer must explode before applying policy.
- **Whitelist/blacklist ordering interaction** — semantics of one flag depend on the
  presence and position of another (`--noblacklist` guarding a later `--blacklist`).

### 3.6 Caching hazards

- `--name=`, `--hostname=`, `--net=` inject identity/network state that can leak
  into program output.
- `--private` mounts a **fresh tmpfs home each run** → nondeterministic; output not
  reproducible.
- `--profile` include-closure pulls in **system profiles under `/etc/firejail`**
  whose contents vary by firejail version/distro — version-dependent effective
  policy, must hash the resolved closure, not the name.
- `~`-relative paths (`--whitelist=~/work`) resolve against **`$HOME`** → per-user
  cache key; must canonicalize the tilde expansion.
- `--timeout=` and time-shaped values are fine, but profiles can embed
  `${HOME}`/`${CFG}` macros → host-path leakage.

---

## 4. `landlock-exec` (landrun / sandlock family)

Minimal Landlock+seccomp wrapper. Modeled on `landrun` (the canonical CLI of this
shape) and `sandlock`.

### 4.1 Invocation shape

```
landrun [FLAGS] -- COMMAND [ARGS...]
landrun --rox /usr/lib --ro /etc --env HOME -- /usr/bin/myapp
sandlock run -w /tmp -r /usr -m 512M -- python3 untrusted.py
```

`landrun` is flat (depth 0). **`sandlock` has a real subcommand**: `sandlock run ...`
— here `Grammar.subcommands = {"run": <inner grammar>}`, depth 1, dispatch on the
first bare token. This is the one tool in the group where the recursive
`subcommands` map is genuinely exercised — and even then it's shallow (the deepest
nesting is `run`, depth 1). After the subcommand, the `--` boundary re-introduces the
foreign inner argv.

### 4.2 Spelling forms

| Flag | Spelling | Notes |
|------|----------|-------|
| `--ro PATH` / `--rw PATH` / `--rox PATH` / `--rwx PATH` | Separated, **or comma-list** | `--ro /usr,/lib,/home` |
| `--bind-tcp PORT` / `--connect-tcp PORT` | Separated / comma-list | integer ports |
| `--env KEY=VALUE` *or* `--env KEY` | Separated | **optional payload**: `KEY` alone = pass-through current value |
| `--log-level error\|info\|debug` | Separated | enum |
| `--best-effort`, `--unrestricted-filesystem`, `--unrestricted-network`, `--add-exec`, `--ldd` | Bare | toggles |
| `-r DIR` / `-w DIR` / `-m SIZE` (sandlock) | Separated short | aliases of ro/rw + memory cap |

**Forms the enum lacks:**

- **Comma-list values** (`--ro /usr,/lib`): one argv token, multiple PathIn
  entries. Same gap as firejail.
- **`--env KEY` (optional value)**: `--env HOME` (capture current value) vs
  `--env HOME=/x` (explicit). Optional-Equals again.
- **Access-mode encoded in the flag name** (`ro`/`rw`/`rox`/`rwx`): the *flag
  identity* carries a 2-bit permission (read/write/exec). These four flags are
  really one logical "grant" slot parameterized by mode. Modeling them as four
  independent `DedupOrdered` flags loses the fact that `--ro /x` and `--rw /x` for
  the **same path** is a conflict/merge (rw supersedes ro), across flag names.

### 4.3 Multiplicity & order

- `--ro`/`--rw`/`--rox`/`--rwx`: **a set of (path, mode) grants.** Order does not
  matter (Landlock unions rulesets); duplicates should dedup; **same path under two
  modes must merge to the union of rights**, not coexist. → needs `DedupUnordered`
  keyed *by path* with **value-merge**, not plain set dedup over raw strings. The
  Policy enum has no "dedup by sub-key with merge function."
- `--bind-tcp`/`--connect-tcp`: integer **sets**, order-irrelevant, dedup, and
  canonically **sorted numerically** → `DedupUnordered` fits cleanly (the one clean
  case in this whole group).
- `--env`: keyed map, last-wins → `KeyedMap`.
- `--add-exec`/`--ldd`: bare toggles that **synthesize further grants** at runtime
  (`--ldd` adds `--rox` for every shared lib resolved by ldd). A flag that *expands
  into more flags computed from the environment* — not representable; the expansion
  depends on the host's libraries.

### 4.4 Path-bearing slots

- `--ro`/`--rw`/`--rox`/`--rwx` values → `PathIn`-with-mode. The `rox`/`rwx` exec
  bit means the path is **also an executable input** (its bytes matter to the run).
- `--add-exec` implicitly adds **the COMMAND path** (the inner program) as a `rox`
  grant → a path-bearing slot whose value is *the first token of the foreign argv*,
  across the `--` boundary.
- `--ldd` adds the **transitive shared-library closure** → an `Lib`/`Include`-style
  dependency graph that is *not in argv* and is host-specific.

`Kind` has `Lib`/`Include` for compiler search semantics, but here the need is a
`PathIn` that also encodes access mode (r/w/x). **New Kind: `Grant(mode)`** — a path
plus a read/write/exec bitmask, deduped by path with mode-union.

### 4.5 Model-breakers

- **Permission encoded in flag identity** (`ro`/`rw`/`rox`/`rwx`) with **cross-flag
  merge by path** (§4.3).
- **`--ldd` / `--add-exec`: flags that expand into a host-computed set of further
  grants** — the effective ruleset depends on the machine's libraries and on the
  inner command path. Pure-argv canonicalization cannot capture it.
- **`--best-effort` changes the meaning of every other flag** (silently drops
  unsupported Landlock rules on old kernels) → a global mode flag that mutates the
  semantics of the rest of the grammar; the *same argv* yields different effective
  policy on different kernels.
- The `--`-then-foreign-argv launcher boundary (shared with all four tools).

### 4.6 Caching hazards

- `--add-exec`/`--ldd` make the effective grant set **host- and kernel-dependent**
  (lib closure differs per machine, Landlock ABI differs per kernel). Mark impure or
  hash the resolved closure.
- `--best-effort` → the realized sandbox differs across kernels for identical argv;
  the cache key is not portable across machines.
- Absolute host paths in every `--ro`/`--rw` → path leakage.
- `--rwx`/`--rw` targets are **written to** at runtime → those are `PathOut`-ish
  artifacts whose post-run content is nondeterministic; must not be folded into the
  *input* key.

---

## Cross-tool synthesis

### Novel spellings (not in `Spelling`)
1. **Multi-token / tuple values** — `bwrap --bind SRC DEST` (2), `--overlay A B C`
   (3), `firejail --put name src dest`. `Separated` is single-valued.
2. **Optional-value (optional-equals)** — `firejail --private` vs `--private=dir`;
   `landrun --env KEY` vs `--env KEY=VAL`.
3. **Comma-list values** — `landrun --ro /a,/b`; `firejail --bind=a,b`;
   `sandbox-exec` none. One token → ordered/keyed multi-entry list.
4. **FD-valued slots** — `bwrap --args FD`, `--seccomp FD`, `--bind-data FD DEST`.
   Value is an inherited descriptor; referent is out-of-band.
5. **Dotted namespace flag names** — `firejail --caps.drop=`, `--seccomp.keep=`.
6. **Mode-in-name flag families** — `landrun --ro/--rw/--rox/--rwx` are one slot
   parameterized by an r/w/x bitmask carried in the flag spelling.

### Novel policies (not in `Policy`)
1. **KeyedMap (dedup-by-key, last-wins, with deletes)** — `sandbox-exec -D k=v`,
   `bwrap --setenv/--unsetenv`, `firejail --env=`, `landrun --env`. Distinct from
   `DedupUnordered` (opaque set) because the key, not the whole string, is the
   identity; and `--unsetenv`/delete entries interleave.
2. **DedupByKeyWithMerge** — `landrun --ro/--rw` for the same path must union to the
   strongest rights; `bwrap` repeated binds to the same DEST → last-wins-by-order.
   Dedup identity is a sub-key; collision resolution is a merge fn, not drop.
3. **OrderedFlagStream (positional-among-flags)** — `bwrap` filesystem ops: spelled
   as flags but mutually order-significant and must never reorder/dedup. `Positional`
   is defined for bare tokens, not flags.
4. **StickyModifier** — `bwrap --perms 0700`/`--size N` scope to the *next* fs flag;
   `--overlay-src` accumulates into the *next* `--ro-overlay`.
5. **Accumulate-with-negation across flag names** — `bwrap --cap-add/--cap-drop`,
   `firejail --blacklist/--noblacklist`, `--profile/--noprofile`. The `-Wfoo`/
   `-Wno-foo` shape but split across two distinct flags, order-sensitive.
6. **ModeFlag (global semantics mutation)** — `landrun --best-effort`,
   `firejail --apparmor`: changes how the rest of the grammar is realized.

### Novel kinds (not in `Kind`)
1. **MountPoint** — bwrap/firejail in-sandbox DEST paths: canonicalize as a path
   string, never mount, never content-hash. Neither `PathIn` nor `PathOut`.
2. **Grant(mode)** — landrun path + r/w/x bitmask; deduped by path with mode-union.
3. **Fd** — bwrap FD-valued slots; referent (bytes) is out-of-band and must be
   hashed separately or marked impure.
4. **PolicyScript** — sandbox-exec SBPL (`-p`/`-f`), firejail `.profile`: a nested
   sub-language with its own include-closure, far richer than `Passthrough(sep)`.
5. **EnvPair** — `KEY=VALUE` payload riding inside a Separated/Equals slot
   (`-D k=v`, `--env k=v`, `--setenv k v` even splits it across two tokens).

### Genuine model-breakers
- **Launcher/payload boundary**: every tool ends with `[--] COMMAND [ARGS...]`
  where the tail is a foreign argv carried verbatim — not a `subcommands` keyword,
  not the `--` end-of-flags convention. The inner argv may itself contain `--`.
- **Nested policy sub-languages with transitive includes** (SBPL, firejail
  profiles): the true input set is a file closure invisible to argv.
- **Flags that expand into host-computed grants** (`landrun --ldd`/`--add-exec`):
  effective argv depends on the machine.
- **Cross-flag value coupling**: sandbox-exec `-D` value meaning depends on how the
  SBPL profile uses the key; firejail `--noblacklist` guards a later `--blacklist`.
- **Mode flags that re-interpret the whole grammar** (`--best-effort`).
- **Tuple/sticky/ordered flag semantics** that the one-value-per-flag,
  reorderable-policy model cannot host.

### Caching hazards (canonicalizer must neutralize or mark impure)
- **FD-referenced data** (`bwrap --args/--seccomp/--bind-data`) — not in argv; hash
  the bytes or mark impure.
- **Host-computed closures** (`--ldd` libs, profile `include` graphs) — hash the
  resolved set, not the flag.
- **Absolute host-path leakage** in every bind/whitelist/ro/rw slot and inside
  embedded profiles; tilde/`$HOME` expansion is per-user.
- **`-D`/`--env`/`--setenv` tmpdir & machine-specific values** (`/var/folders/...`).
- **Fresh tmpfs / `--private` / `--proc` / `--dev`** → nondeterministic sandbox
  state; outputs not byte-reproducible.
- **Kernel/OS-version-dependent realization** — `landrun --best-effort` (Landlock
  ABI), `sandbox-exec -n` builtin profiles (macOS version), firejail system
  profiles (distro/version). Same argv, different effective policy across hosts;
  the key is not portable.
- **Identity injection** (`--hostname`, `--uid/--gid`, `firejail --name`) leaking
  into program output → canonicalize to sentinels for hermetic runs.
