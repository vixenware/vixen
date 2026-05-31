# `exec` — the build primitive (synthesis spec, for review)

**Status: review draft.** This synthesizes three sources — the command-model
agent (the projection layer), the `exec`/`Process` direction from Amos (the
success model), and the demand-driven runtime being built in `vx-runtime` (the
execution + handle model). It is **not** canonical yet: once Amos decides the
open points, it reconciles into `docs/content/spec/stdlib/build.md` (the
normative `r[exec.*]`), `crates/vx-stdlib/std/build.vx`, the wiki, and `zoo/`
examples, with tests authored ahead of implementation.

Every claim is tagged:

- **[settled]** — already in the design/docs or directly stated by Amos.
- **[lean]** — a proposal (the command-model agent's intent, or my inference);
  needs Amos's yes/redirect.
- **[open]** — genuinely undecided; the decision ledger (§12) collects these.

---

## 0. The through-line — projection vs. runtime

**[settled]** There is one boundary that makes the rest fall out:

- **command-model is pure projection.** Grammar + argv → a classified value
  (per-token I/O `Kind`, canonical form). **Zero I/O, zero evaluation, zero
  hashing.** It is data, not behavior.
- **The runtime owns identity, transfer, and execution.** It resolves paths to
  content identities, moves bytes, runs processes, evaluates observers.

Whenever a question is "whose job is X," this answers it. command-model never
touches the filesystem or the network; the runtime never re-derives command
structure (it consumes the projected, classified value).

---

## 1. The demand triple (recap — settled)

**[settled]** (`runtime-semantics.md` §5.) An `exec` is `(upfront, progressive,
produced)`, and the three phases *are* the two cache levels:

- **① Upfront** — forced *before dispatch*; **is the L1 cache key.** The
  effective command's identity (§3): canonical argv with input paths replaced by
  content identities, resolved env, the program binary's identity, impurity
  marks. No file bytes move. Cache hit ⇒ never dispatch.
- **② Progressive** — discovered *during the run*; **is the L2 read-set.** The
  executor pulls mounted-input bytes as the command opens each file. Inbound.
- **③ Produced** — outputs that resolve over time, pulled **only by the
  requester**: stdout/stderr chunks, the write-set tree per path, and the
  observer's value. Outbound.

"`exec` is inert except for demand from both peers: the executor pulls inputs
(②), the requester pulls outputs (③)." The remote-part bridge already built in
`vx-runtime` (`force_remote` + the producing handle) is exactly ③'s transport.

---

## 2. The `Command` value (what `@grammar[ … ]` evaluates to)

**[settled]** `exec` takes a **`Command`**, not `Array[ExecInput]`. A CLI literal
`@cc[ -c -O2 -I src main.c -o $out ]` (sigil + imported grammar name + command-
mode body with `$ident` / `${expr}` holes) is type-checked against the grammar:
static parts validated, holes typed by their position's `ValueDomain` + `Kind`.
It evaluates to the `Command`.

**[settled]** The projection classifies every token's `Kind`
(`Text | Input{dir} | Output{dir} | InOut`) **once**; the runtime consumes a
clean classified value and never re-walks the parsed args. The `Command` carries:

| field | what | note |
| --- | --- | --- |
| `program` | the **abstract** program name from the grammar | resolved to a binary by the runtime (§4) |
| execution argv | literal args, holes substituted — **what runs** | |
| canonical argv | compat-normalized — **what the key is built from** | *different* from execution argv |
| `env` | resolved declared channels (the intersection, §5) | |
| `inputs` | `role + Kind + (path \| value)` | paths, **not** hashes (§3) |
| `outputs` | `role + path` | **new — not in `EffectiveCommand` today; add it** |
| `impurities` / `lints` | soundness + diagnostics | |

**[lean]** Whether this is an extended `EffectiveCommand` or an exec-facing
`Command` wrapper is an implementation choice; the contract is: I/O
classification happens once in projection, the runtime gets the classified value
**with outputs included** (the dual of `inputs`, currently missing).

---

## 3. Identity — the L1 key (settled)

**[settled]** The key is the **resolved effective command**, computed by the
runtime from the projected value:

- canonical argv with **input paths replaced by content identities** (paths are
  environment-specific; identities aren't),
- resolved `env`,
- the **program binary's identity** (§4 — a different `cc` must invalidate),
- impurity marks.

The handoff is precise: *command-model gives canonical argv + input paths-by-role;
the runtime replaces paths with content identities to form the key.* Resolving
those identities is **① upfront demand** (you cannot form the key without them);
serving the bytes is **② progressive** (lazy, per-read).

**[settled]** **Env-declared `Kind::Input` channels are inputs too.** `CPATH` /
`LIBRARY_PATH` are `Kind::Input{dir}` — an env-supplied include dir mounts +
hashes exactly like `-I`. So `env` feeds `inputs`, not just an env map.

---

## 4. Program resolution & executor routing — the least-pinned piece

**[settled-intent]** The binary is itself an input: the runtime resolves the
program to a binary identity (host fact: which binary + version/hash; or
hermetic: a tree id) and folds it into the key.

**[lean / open — the #1 decision]** The grammar's `program` is the *abstract*
name ("cc"). Two jobs hang off resolving it to a concrete binary, and they're the
fuzziest wiring in the whole design:

1. **Concrete binary.** Supplied by the **program override** — `@cc:${tc.cc}[ … ]`
   (inline shorthand) or an `exec` kwarg (the primitive: hand `exec` an absolute
   path or a hermetic tree). Absent an override, a toolchain **ability** in scope
   resolves the grammar's program name to a binary.
2. **Executor routing.** Type-driven: a `CToolchain` value routes to a C executor
   class, `RustToolchain` to a Rust one — by predicate match, no stringly-typed
   selector.

**The open tension:** if the only thing flowing into `exec` is the spliced
*string* `tc.cc`, `exec` cannot route by `CToolchain` *type* — a string carries
no ability. So the **ability value itself must reach `exec`** (a kwarg, or the
executor context), serving as both the binary resolver and the routing key. My
lean: `exec` takes the toolchain ability explicitly; the grammar's program name
is the lookup key into it. **This is the part the command-model agent explicitly
left to us ("build it the way that's cleanest and we'll converge") — flagging it
as the first thing for you to decide**, because the `exec` signature depends on
it.

---

## 5. `exec` signature & the surviving kwargs

**[lean]** Synthesizing the command-model agent's Q6 + the program-resolution
question:

```vixen
fn exec[T](
  command: Command,                 // from a @grammar[ … ] literal
) with {
  // --- program/routing (§4, open spelling) ---
  // the toolchain ability that resolves `command.program` -> binary AND routes;
  // exact kwarg name/shape is the §4 decision (or the @cc:${…} override).
  toolchain: <Ability>,             // [open]
  // --- env: value source for the grammar's declared channels (§5) ---
  env: Map[String, ExecInput] = %{},
  // --- sandbox supplements the grammar can't see ---
  mounts: Map[Path, TreeEntry] = %{},     // extra content NOT named on argv [lean: narrowed]
  writable: Array[Path] = [],             // outputs the grammar can't classify [lean: supplement]
  create_dirs: Array[Path] = [],          // ditto [lean: supplement]
  exemptions: Array[SandboxExemption] = [],   // soundness policy overriding `impurities`
  // --- the observer (§6) ---
  observer: Fn(Process, Tree) -> T,
} -> T
```

Per-kwarg verdict (command-model agent, **[settled]** unless noted):

- `env:` — **survives**; supplies the values for grammar-declared channels.
- `observer:` — **survives, untouched**; orthogonal to identity.
- `writable:` / `create_dirs:` — **survive as supplements** to grammar-derived
  outputs (a tool that writes a dir not on the argv; sidecar files). Grammar
  outputs are authoritative for grammar-covered tokens; these are *additive*.
- `mounts:` — **survives, narrowed**: only extra content not implied by the
  command. Grammar-covered inputs auto-mount.
- `exemptions:` — **survives**: soundness policy the grammar can't express
  (acknowledge an impurity, exempt a host path); it overrides the `impurities`
  checks, so it lives outside the grammar.

**[settled]** A `Tree`/`Blob` spliced into a hole (`-I ${some_tree}`) is a
**first-class `Input`** with the position's role — *not* tracked separately. Its
only difference from a path-input: it already carries its content identity (a CAS
value), so there's nothing to resolve; the runtime synthesizes a mount path and
substitutes it into the execution argv. The **mount layout** (which sandbox path
maps to what) is the runtime's to compute from `inputs` + `outputs` + the
executor class; the input/output **sets** arrive grammar-classified.

---

## 6. The executor & the observer

**[settled]** The orchestrator **does not run the process or inspect the
sandbox**. It serializes the **full payload** — the `Command`, the **observer
closure with its resolved captures**, and the verification obligations carried by
the inputs — and ships it to an executor chosen by type-driven routing (§4). The
executor:

1. sets up the sandbox (mounts from `inputs`, writable roots from `outputs`,
   COW),
2. runs the process, pulling input bytes from the orchestrator as it reads (②),
3. evaluates the **observer there** (`i[impl.exec.shipped-closures]` +
   `i[impl.exec.shared-evaluator]` — same evaluator both sides),
4. rechecks the host obligations on the concrete runner,
5. returns **only the observer's produced value** `T` across the boundary (③).

**[settled]** The observer's authority is **exactly** the `Process` and the
sandbox `Tree` — no `Host`, no ambient executor authority. It ships with code
identity + lowered body + resolved captures; a closure cannot be sent until all
captures are resolved.

**[load-bearing]** This split, closure-shipping, and observer-on-the-executor are
**present from the first line of real code**, even with an in-process executor
over vox. A version where the orchestrator runs the command itself, or where
there is no observer, is a *different thing*, not a thin `exec`.

---

## 7. `Process` & the success model

The chisel: **no bare `exit_code`.** A raw integer invites everyone to write
`if exit_code == 0 { … } else { fail }` *and* makes the user assemble the failure
report — duplicated boilerplate, and bad errors because the party with the
context (the runtime) isn't reporting.

**[settled] `Process` (executor-side, given to the observer):**

- `stdout: Stream[Bytes]`, `stderr: Stream[Bytes]` — live; readable before the
  verdict, independent of it.
- **`.ok`** — depend on it to gate on success. It **poisons** on failure (nonzero
  exit *or* signal), and the **runtime** fills that poison with the full context.
  Used directly it's a gate; chaining output on it is just a **demand edge** (the
  output node depends on `.ok`, so forcing the output forces success first — no
  `if`, it falls out of the demand graph).
- **`.ok?`** — `?` reifies the would-be poison into a **`Result`** (Amos's word,
  not "Fallible") whose `Err` carries the structured cause; you inspect and
  normalize (exit `7` ≡ success for some tools).

**[settled] Signals are distinct from exit codes** and modeled as the failure
*cause*: `Exited(U64) | Signaled(Signal)` (room for `core_dumped` later). `.ok`
poisons on either; `.ok?` exposes which, so a specific signal can be normalized
just like a specific exit code. The default failure report carries the signal,
the command, and the **tails** of stdout/stderr.

**[settled] `fail` / an uncaught poison** = that same structured error **plus
source location plus more context** — a strict superset of the caught `Err`.
Capture is bounded by **keyword args** (max stdout/stderr retained).

**[lean] `.ok`'s *success* value** is a small record — `Exit { duration, stdout,
stderr }` (captured per the limits) — so a run-and-parse tool reads `r.stdout`
while a compile ignores it (pure gate). (You floated unit vs. "timing + output";
this is the latter, concrete. **Decide.**)

**[lean] `.stream`** is the second way to the VFS: the **live** write-set + live
stdout/stderr, where *you* decide when a path is finalized — finalization is
**not** knowable from the VFS alone; you parse the tool's own notifications
(rustc `--json`) and emit handles early. Whether `.stream` is a `Process`
accessor or a second observer signature is **[open]**.

**[lean] `ProcError`** ≈ `{ cause: Exited|Signaled, command, stdout_tail,
stderr_tail, duration }`. **Decide its exact shape.**

The three usage patterns (all in §11): **gate-and-select** (90%), **stream
partial events** (rustc metadata before codegen — mandatory), **catch &
normalize** (`.ok?`).

---

## 8. Outputs & the write-set `Tree`

**[settled]** The sandbox write-set `Tree` is **incrementally available**: an
entry resolves when the process *finalizes that file* (close / atomic rename /
the package's own detection from `--json`), **not** at process exit. So
`tree["out/libC.rmeta"]` resolves mid-run and a dependent `exec` pipelines against
the still-running producer (Cargo pipelining). This *is* the producing handle
the remote-part bridge already implements.

**[settled]** **Success is its own handle, orthogonal to outputs.** `.ok`
poisoning must **not** poison the whole `Process`: a finalized `out/x.rmeta` or an
emitted stream event resolves on its own timeline and **survives `.ok` failing
later** (rustc emits `.rmeta`, then codegen fails — the metadata stands). The two
ways to the VFS:

- **`.ok`-gated take** — wait for success (poison if not), then take from the
  settled `out/`. **Default observer = the whole `out/` tree, gated on `.ok`.**
- **`.stream` early-emit** — emit handles to finalized paths before the verdict
  (§7).

**[settled]** If the observer returns `Stream[T]`, `exec` returns `Stream[T]`.
When the observer's value crosses back, any VFS content it references is
**auto-committed to the CAS** (`Blob`/`Tree` cross as store-backed values, not
executor-local handles), so the executor can be released.

---

## 9. Errors & poison

**[settled]** `exec` itself **poisons** only if the executor can't start / loses
the process / fails before handing `Process` + `Tree` to the observer (and a
`Stream[T]` result terminates, its pending values poisoned). **Ordinary process
failure is observer-visible** — surfaced through `.ok` / `.ok?`, not an automatic
`exec` poison. Verification drift is its own distinct poison kind.

**[settled-principle]** Failure reporting is the **runtime's** job, carried by the
poison (command / cause / tails / sandbox), same discipline as I6 (the poison
carries everything to diagnose, pointed at the source).

**[open] The poison ↔ `Result` ↔ `?` story** is bigger than `exec`: for `.ok?` to
return an `Err` carrying structured data, a process failure is a **typed value**,
not a string — uncaught it becomes a (context-rich) poison; `?`-caught it becomes
`Result[Exit, ProcError]`. This touches the runtime's `Poison` type (today
`{ reason: String, span }` — it needs a structured payload) and the language's
catchable-failure semantics generally. **Pin this with Amos** — it generalizes
well past `exec`.

---

## 10. Caching (deferred; shaped here)

**[settled, not-yet-built]** L1 = the effective-command identity (§3); hit ⇒ never
dispatch. L2 = the read-set (②); rerun-avoidance after an L1 miss. The cache
stores the **observer's `T`**, which is sound iff `T` is **content-determined,
not timing-determined** — a static check over the observer's combinator graph
(`.next()` / racing `.find` make it non-cacheable; `map`/`filter`/`.only()` are
fine). `.ok` and `ProcError` are content-determined ⇒ cacheable. **Not in the
first implementation slice**, but the `Command` carries `impurities` and the
observer-cacheability rule so nothing here is precluded.

---

## 11. Sketches

**1 — gated (`cc` compiles an object; the easy 90%).**

```vixen
let { cc } = import "@vixen/grammar-cc"

// $src is a Kind::Input, $obj a Kind::Output — the mount set and the writable
// root fall out of the grammar. The concrete binary + host holes come from the
// toolchain ability (§4 — exact wiring open).
fn object(tc: CToolchain, src: Path, obj: Path) -> Tree (
  exec(@cc:${tc.cc}[ -c $src -o $obj ]) with {
    exemptions: tc.coarse_mounts,                // host read/exec holes the grammar can't express
    observer: (proc, out) => ( proc.ok; out ),   // default observer, spelled out
  }
)

// run-and-parse: `.ok`'s success value carries captured stdout
fn cc_version(tc: CToolchain) -> SemVer (
  let r = exec(@cc:${tc.cc}[ --version ]) with { observer: (proc, _) => proc.ok };
  parse_semver(r.stdout)
)

// normalize: exit 7 means "nothing to do" — catch .ok? and decide
fn run(tc: CToolchain, …) -> Tree (
  exec(@cc:${tc.cc}[ … ]) with {
    observer: (proc, out) => match proc.ok? (
      Ok(_)                          => out,
      Err(e) if e.cause == Exited(7) => out,                  // 7 ≡ success here
      Err(e)                         => fail(`cc failed: ${e}`),
    ),
  }
)
```

**2 — streaming (`rustc` pipelines `.rmeta` before `.rlib`; mandatory).**

```vixen
let { rustc } = import "@vixen/grammar-rustc"

// Each artifact flows the instant rustc finalizes it — the .rmeta event reaches a
// dependent crate before codegen finishes, and stands even if codegen then fails.
fn compile(tc: RustToolchain, name: String, src: Path, out: Path) -> Stream[Artifact] (
  exec(@rustc:${tc.rustc}[ --crate-name $name --edition 2021 --emit metadata,link --out-dir $out $src ]) with {
    observer: (proc, _out) => (
      // finalization isn't knowable from the VFS; rustc announces it on stderr as
      // JSON. The package owns the parser; we correlate each message to a live handle.
      proc.stream.stderr.lines()
        .filter_map(rustc.parse_artifact)                      // line -> Option[ArtifactMsg]
        .map((m) => Artifact { kind: m.emit, file: proc.stream.file(m.path) })
    ),
  }
)
```

---

## 12. Decision ledger (what needs Amos)

**[open] — decide before the canonical pass:**

1. **§4 program/routing wiring** — does `exec` take the toolchain ability
   explicitly (kwarg / context) as both binary-resolver and routing key? Exact
   spelling of the program-override vs. ability kwarg. *(Blocks the signature.)*
2. **§7 `.ok` success value** — unit, or the `Exit { duration, stdout, stderr }`
   record?
3. **§7 `.stream`** — a `Process` accessor, or a distinct observer signature?
4. **§7 `ProcError` shape** — fields beyond `cause`; is the command/tails in the
   caught `Err` or only in the uncaught poison?
5. **§9 poison ↔ `Result` ↔ `?`** — the catchable-failure semantics (typed
   failure → poison-if-uncaught / `Result`-if-`?`-caught), and the `Poison` type
   change it implies. *(Generalizes past `exec`.)*

**[lean] — confirm or redirect:**

6. The `Command` value carries `outputs` (add to `EffectiveCommand`) and both
   execution + canonical argv (§2).
7. Surviving kwargs and their narrowed meanings (§5).

**[settled] — building toward these:** the projection/runtime boundary (§0); the
demand triple (§1); identity from content hashes + program identity + env (§3);
the executor/observer split + closure-shipping (§6); incremental write-set +
success-orthogonal-to-outputs + auto-commit (§8); ordinary-failure-is-data +
runtime-owned reporting (§9).

---

## 13. First implementation slice (orientation, not now)

When we build, the thin-but-**complete** spine — no demo shortcuts:

- a real orchestrator↔executor split over vox (in-process executor first; remote
  is a different transport, not a different design),
- real closure-shipping of the observer + executor-side evaluation (shared
  evaluator),
- the `Command` value consumed by `exec`, mounts/outputs derived from it,
- ③ produced outputs crossing back via the existing remote-part bridge
  (`.ok`, the write-set tree, streams as produced handles).

**Deferred** (design accommodates, slice omits): L1/L2 caching, the capability
registry + real remote executors, `OpenForeign`/nested grammars, the impurity
escape hatch, `InOut` rewrite inputs.
