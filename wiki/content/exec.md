+++
title = "The exec primitive"

[extra]
type = "Doc"
tiers = ["2"]
tags = ["exec", "runtime", "command-model", "sandbox"]
+++

**Review draft.** This synthesizes three threads — the command-model projection
layer, the `exec`/`Run` success model, and the demand-driven runtime
(`vx-runtime`) execution + handle model — into one place. It is not yet
normative: once the open decisions (§12) are made, the settled parts land in the
Tracey spec (`r[exec.*]`), with examples in [Vixen Build](/vixen-build/) and
tests authored ahead of implementation.

Claims are tagged **[settled]** (in the design or stated), **[lean]** (a proposal
needing a yes/redirect), **[open]** (undecided — collected in §12).

## 0. The through-line — projection vs. runtime

**[settled]** One boundary makes the rest fall out:

- **command-model is pure projection.** Grammar + argv → a classified value
  (per-token I/O `Kind`, canonical form). **No I/O, no evaluation, no hashing.**
  Data, not behavior.
- **The runtime owns identity, transfer, and execution.** It resolves paths to
  content identities, moves bytes, runs processes, evaluates observers.

Whenever a question is "whose job is X," this answers it. command-model never
touches the filesystem or network; the runtime never re-derives command
structure — it consumes the projected, classified value.

## 1. The demand triple

**[settled]** An `exec` is `(upfront, progressive, produced)`, and the three
phases *are* the two cache levels (see [Cache and trust](/cache-and-trust/)):

- **① Upfront** — forced *before dispatch*; **is the L1 cache key.** The
  effective command's identity (§3): canonical argv with input paths replaced by
  content identities, resolved env, the program binary's identity, impurity
  marks. No bytes move. Cache hit ⇒ never dispatch.
- **② Progressive** — discovered *during the run*; **is the L2 read-set.** The
  executor pulls mounted-input bytes as the command opens each file. Inbound.
- **③ Produced** — outputs resolving over time, pulled **only by the requester**:
  stdout/stderr chunks, the write-set tree per path, and the observer's value.
  Outbound.

"`exec` is inert except for demand from both peers: the executor pulls inputs
(②), the requester pulls outputs (③)." The remote-part handle in `vx-runtime`
(`force_remote` + the producing handle) is exactly ③'s transport.

## 2. The `Command` value (what `@grammar[ … ]` evaluates to)

**[settled]** `exec` takes a **`Command`**, not `Array[ExecInput]`. A CLI literal
`@cc[ -c -O2 -I src main.c -o $out ]` (sigil + imported grammar name + command-
mode body with `$ident` / `${expr}` holes) is type-checked against the grammar:
static parts validated, holes typed by their position's `ValueDomain` + `Kind`. It
evaluates to the `Command`.

**[settled]** The projection classifies every token's `Kind`
(`Text | Input{dir} | Output{dir} | InOut`) **once**; the runtime consumes a clean
classified value and never re-walks the parsed args. The `Command` carries:

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
`Command` wrapper is an implementation choice; the contract is: I/O classification
happens once in projection, and the runtime gets the classified value **with
outputs included** (the dual of `inputs`, currently missing).

## 3. Identity — the L1 key

**[settled]** The key is the **resolved effective command**, computed by the
runtime from the projected value:

- canonical argv with **input paths replaced by content identities** (paths are
  environment-specific; identities aren't),
- resolved `env`,
- the **program binary's identity** (§4 — a different `cc` must invalidate),
- impurity marks.

The handoff is precise: *command-model gives canonical argv + input paths-by-role;
the runtime replaces paths with content identities to form the key.* Resolving
those identities is **① upfront demand**; serving the bytes is **② progressive**.

**[settled]** **Env-declared `Kind::Input` channels are inputs too.** `CPATH` /
`LIBRARY_PATH` are `Kind::Input{dir}` — an env-supplied include dir mounts +
hashes exactly like `-I`. So `env` feeds `inputs`, not just an env map.

## 4. Program, grammar, routing — all from the capability

**[settled]** This was posed as an open decision; it dissolves once you stop
splitting the grammar from the capability. **A capability probe package
(`@vixen/c`) owns the toolchain type, ships the probe, *and* owns its tool
grammars — and the probed capability exposes its tools as commands already bound
to their grammar.** So a `CToolchain`'s `cc` is **not** a bare path you pair with a
separately-imported `@vixen/grammar-cc`; it is a **command-with-grammar**: grammar
+ the concrete probed binary + the toolchain's sandbox needs (exemptions /
sysroot), one bundle. The capabilities a probe exposes *are*, in this case, the
commands you can run, already grammar-associated — see
[Executor abilities](/executor-abilities/).

That collapses what I had wrongly split into three:

1. **Grammar** — comes from the capability's command (its type carries it), not a
   free import. The args you write are typed by *that* grammar.
2. **Concrete binary** — bound in the command by the probe (the host fact it
   detected, or a hermetic tree). It is an input; its identity folds into the key
   (a different `cc` invalidates).
3. **Routing** — **falls out of provenance** ([Executor abilities](/executor-abilities/)):
   the `CToolchain`-derived value flowing into `exec` *is* the routing key; the
   planner aggregates the ability values an `exec` consumes and picks an executor
   satisfying their intersection. No `executor:` kwarg, no string that "carries no
   ability" — the command-with-grammar carries the type.

So the program override (`@cc:${path}`) and a separate `exemptions:` kwarg for the
toolchain's own holes are the **fallback** — for when you hold a free grammar but
no capability — **not** the main path. The intended path: the capability gives you
the command; you apply per-invocation args. **[residual detail]** the surface
syntax for applying args to a capability's command vs the `@grammar[…]` literal for
a free-imported grammar (§11).

## 5. `exec` signature & the surviving kwargs

**[lean]**

```vixen
fn exec[T](
  command: Command,   // a capability's command-with-grammar (e.g. tc.cc), or a free @grammar[…] literal
) with {
  // No program/routing kwarg: the command carries grammar + binary, and routing
  // falls out of the ability values' provenance (§4).
  env: Map[String, ExecInput] = %{},      // value source for grammar-declared channels (§5)
  mounts: Map[Path, TreeEntry] = %{},     // extra content NOT named on argv [lean: narrowed]
  writable: Array[Path] = [],             // outputs the grammar can't classify [lean: supplement]
  create_dirs: Array[Path] = [],          // ditto [lean: supplement]
  exemptions: Array[SandboxExemption] = [],   // supplement; a capability command carries its own (§4)
  observer: Fn(Run) -> T,                     // Run = .ok (settled) / .stream (live), §7
} -> T
```

Per-kwarg verdict (**[settled]** unless noted):

- `env:` — **survives**; supplies the values for grammar-declared channels.
- `observer:` — **survives, untouched**; orthogonal to identity.
- `writable:` / `create_dirs:` — **survive as supplements** to grammar-derived
  outputs (a tool that writes a dir not on the argv; sidecar files). Grammar
  outputs are authoritative for grammar-covered tokens; these are *additive*.
- `mounts:` — **survives, narrowed**: only extra content not implied by the
  command. Grammar-covered inputs auto-mount.
- `exemptions:` — **survives as a supplement**: a capability's command already
  carries the toolchain's own host holes (§4); this kwarg adds extra ones, or
  covers the free-grammar-without-a-capability case. It's soundness policy
  (acknowledge an impurity, exempt a host path) overriding the `impurities` checks.

**[settled]** A `Tree`/`Blob` spliced into a hole (`-I ${some_tree}`) is a
**first-class `Input`** with the position's role — not tracked separately. Its only
difference from a path-input: it already carries its content identity (a CAS
value), so there's nothing to resolve; the runtime synthesizes a mount path and
substitutes it into the execution argv. The **mount layout** (which sandbox path
maps to what) is the runtime's to compute from `inputs` + `outputs` + the executor
class; the input/output **sets** arrive grammar-classified.

## 6. The executor & the observer

**[settled]** The orchestrator **does not run the process or inspect the
sandbox**. It serializes the **full payload** — the `Command`, the **observer
closure with its resolved captures**, and the verification obligations carried by
the inputs — and ships it to an executor chosen by type-driven routing (§4). The
executor:

1. sets up the sandbox (mounts from `inputs`, writable roots from `outputs`, COW),
2. runs the process, pulling input bytes from the orchestrator as it reads (②),
3. evaluates the **observer there** (`i[impl.exec.shipped-closures]` +
   `i[impl.exec.shared-evaluator]` — same evaluator both sides),
4. rechecks the host obligations on the concrete runner,
5. returns **only the observer's produced value** `T` across the boundary (③).

**[settled]** The observer's authority is **exactly** the `Run` it receives (§7) —
no `Host`, no ambient executor authority. It ships with code identity + lowered
body + resolved captures; a closure cannot be sent until all captures are resolved.

**[load-bearing]** This split, closure-shipping, and observer-on-the-executor are
present **from the first line of real code**, even with an in-process executor over
vox. A version where the orchestrator runs the command itself, or where there is no
observer, is a *different thing*, not a thin `exec`.

## 7. `Run` & the success model

The chisel: **no bare `exit_code`.** A raw integer invites everyone to write
`if exit_code == 0 { … } else { fail }` *and* makes the user assemble the failure
report — duplicated boilerplate, and bad errors because the party with the context
(the runtime) isn't reporting. So the observer receives a **`Run`**, not a raw
process, and reaches success through `.ok`. See
[Decide the process model](/process-as-first-class-value/).

**[settled] `Run` has two faces, each with a `.out: Tree`:**

- **`run.ok`** — the **settled, success-gated** face: an `Exit { duration, stdout,
  stderr, out: Tree }` record. Force it and the process exited 0; otherwise it
  **poisons**. `run.ok.out` is the whole resolved write-set; `run.ok.stdout` /
  `.stderr` are the captured output (bounded by capture kwargs), for run-and-parse
  tools. Used purely as a gate it's just a **demand edge** — the output depends on
  `.ok`, so forcing the output forces success first; no `if`, it falls out of the
  demand graph.
- **`run.stream`** — the **live, ungated** face: `{ stdout: Stream[Bytes], stderr:
  Stream[Bytes], out: Tree }`. `run.stream.out` is the **producing handle**
  (per-path, resolves as files finalize, **survives a later `.ok` failure**);
  `run.stream.stderr` is the live feed you parse (rustc `--json`) to know *when* a
  path finalized — the VFS alone can't tell you.

So the two ways to the output tree: `run.ok.out` (gated — "the output, if it
succeeded") and `run.stream.out` (live — "the output as it's produced").

**[settled] Failure is the [failure model](/failure-model/), not a bare code.**
`run.ok` poisons on **any** non-success — nonzero exit *or* a signal. `run.ok?`
catches it as `Result[Exit, ProcError]`; you normalize (exit `7` ≡ success) or
re-`fail`. The domain error:

```vixen
struct ProcError {
  cause: Exited(U64) | Signaled(Signal),   // exit vs. kill — distinct (room for core_dumped)
  command: Command,                        // the full effective command that ran
  stderr_tail: Bytes,                      // bounded by capture kwargs
  vfs_trace: VfsTrace,                     // the read/write set
  duration: Duration,
}
```

`ProcError` is **pure domain data**; the span + strand stack + demand-path live in
the `FailureContext` envelope (`Failure { err: ProcError, context }`), uniform
across *all* failures — not stuffed into `ProcError` (see
[the failure model](/failure-model/)). An uncaught `run.ok` is that `Failure`
propagating (the runtime renders the report); a caught `run.ok?` hands you the same
`Failure` as data.

The three usage patterns (§11): **gate-and-select** (90%), **stream partial
events** (rustc metadata before codegen — mandatory), **catch & normalize**
(`run.ok?`).

## 8. Outputs & the write-set `Tree`

**[settled]** The sandbox write-set `Tree` is **incrementally available**: an entry
resolves when the process *finalizes that file* (close / atomic rename / the
package's own detection from `--json`), **not** at process exit. So
`tree["out/libC.rmeta"]` resolves mid-run and a dependent `exec` pipelines against
the still-running producer (Cargo pipelining). This *is* the producing handle the
remote-part handle in `vx-runtime` implements. See
[Design build-output materialisation](/materialise-build-output/).

**[settled]** **Success is its own handle, orthogonal to outputs.** `.ok` poisoning
must **not** poison the whole `Run`: a finalized `out/x.rmeta` or an emitted
stream event resolves on its own timeline and **survives `.ok` failing later**
(rustc emits `.rmeta`, then codegen fails — the metadata stands). The two ways to
the VFS:

- **`.ok`-gated take** — wait for success (poison if not), then take from the
  settled `out/`. **Default observer = the whole `out/` tree, gated on `.ok`.**
- **`.stream` early-emit** — emit handles to finalized paths before the verdict
  (§7).

**[settled]** If the observer returns `Stream[T]`, `exec` returns `Stream[T]`.
When the observer's value crosses back, any VFS content it references is
**auto-committed to the CAS** (`Blob`/`Tree` cross as store-backed values, not
executor-local handles), so the executor can be released.

## 9. Errors & poison

**[settled]** `exec` itself **poisons** only if the executor can't start / loses
the process / fails before handing the `Run` to the observer (and a `Stream[T]`
result terminates, its pending items poisoned). **Ordinary process failure is
observer-visible** — surfaced through `run.ok` / `run.ok?`, not an automatic `exec`
poison. Verification drift is its own distinct poison kind.

**[settled]** Failure reporting is the **runtime's** job, carried by the `Failure`
envelope (`context`: span / strand stack / demand-path; `err`: the domain
`ProcError`), never assembled by the user — the [failure model](/failure-model/),
which is the I6 source-fidelity discipline generalized from "carry a span" to
"carry a typed `err` + `context`." `run.ok` poisons with a `Failure { err:
ProcError, context }`; `run.ok?` catches the same value as `Result[Exit, ProcError]`.

## 10. Caching (deferred; shaped here)

**[settled, not-yet-built]** L1 = the effective-command identity (§3); hit ⇒ never
dispatch. L2 = the read-set (②). The cache stores the **observer's `T`**, sound iff
`T` is **content-determined, not timing-determined** — a static check over the
observer's combinator graph (`.next()` / racing `.find` make it non-cacheable;
`map`/`filter`/`.only()` are fine). `.ok` and `ProcError` are content-determined ⇒
cacheable. Detail in [Cache and trust](/cache-and-trust/). Not in the first
implementation slice, but the `Command` carries `impurities` and the
observer-cacheability rule so nothing here is precluded.

## 11. Sketches

**1 — gated (`cc` compiles an object; the easy 90%).**

```vixen
// `@vixen/c`'s probe yields a CToolchain whose `cc` is a command already bound to
// the cc grammar + the probed binary + the toolchain's sandbox needs — one bundle.
// No grammar import, no binary override, no exemptions kwarg. Routing falls out of
// `tc`'s provenance. ($src is Kind::Input, $obj Kind::Output — the mount set and
// the writable root fall out of the grammar.)
fn object(tc: CToolchain, src: Path, obj: Path) -> Tree (
  let { cc } = tc;                                 // the grammar-bound command
  exec(@cc[ -c $src -o $obj ]) with {
    observer: (run) => run.ok.out,                 // gate on success, return the write-set tree
  }
)

// run-and-parse: `run.ok` is the Exit record { duration, stdout, stderr, out }
fn cc_version(tc: CToolchain) -> SemVer (
  let { cc } = tc;
  let exit = exec(@cc[ --version ]) with { observer: (run) => run.ok };
  parse_semver(exit.stdout)
)

// normalize: exit 7 means "nothing to do" — catch run.ok? and decide
fn run_cc(tc: CToolchain, …) -> Tree (
  let { cc } = tc;
  exec(@cc[ … ]) with {
    observer: (run) => match run.ok? (
      Ok(exit)                                                 => exit.out,
      Err(Failure { err: ProcError { cause: Exited(7) }, .. }) => run.stream.out,  // 7 ≡ success — take the live output
      Err(f)                                                   => fail f,
    ),
  }
)
```

Here `@cc[…]` takes its grammar from the capability command `cc` (destructured
from `tc`), **not** a free `@vixen/grammar-cc` import — that grammar-from-a-value's
-type binding is the residual syntax detail from §4.

**2 — streaming (`rustc` pipelines `.rmeta` before `.rlib`; mandatory).**

```vixen
// Each artifact flows the instant rustc finalizes it — the .rmeta event reaches a
// dependent crate before codegen finishes, and stands even if codegen then fails.
fn compile(tc: RustToolchain, name: String, src: Path, out: Path) -> Stream[Artifact] (
  let { rustc } = tc;                              // command-with-grammar from the capability
  let { parse_artifact } = import "vixen:rust";    // the package owns the --json artifact parser
  exec(@rustc[ --crate-name $name --edition 2021 --emit metadata,link --out-dir $out $src ]) with {
    observer: (run) => (
      // finalization isn't knowable from the VFS; rustc announces it on stderr as
      // JSON. We correlate each message to a live handle into the producing tree.
      run.stream.stderr.lines()
        .filter_map(parse_artifact)                            // line -> Option[ArtifactMsg]
        .map((m) => Artifact { kind: m.emit, file: run.stream.out.file(m.path) })
    ),
  }
)
```

See [Implement streams as the first Tier-2 primitive](/streams-first-primitive/)
for the stream substrate this leans on.

## 12. Decision ledger (what needs deciding)

**[open] — remaining:**

1. **§4 residual (small)** — the surface syntax for applying per-invocation args to
   a capability's command (grammar-from-the-value's-type) vs the `@grammar[…]`
   literal for a free grammar. The *model* is settled; only the spelling is open.
2. **Streams mechanics** — the per-item ③ wire shape, backpressure, and
   session/infinite streams. The *model* is sketched (§8, [the streams Task](/streams-first-primitive/));
   these mechanics are open.
3. **Closure-shipping** — what an observer may capture, and the exact "resolved
   captures" boundary. (Still thinking.)

**[settled] — pinned this round:**

- **§4 program / grammar / routing** — a capability provides the
  command-with-grammar; routing falls out of provenance. No grammar-import +
  binary-override + `exemptions:`-kwarg composition.
- **§7 `Run`** — `.ok` (settled `Exit` record, success-gated) and `.stream` (live),
  each with a `.out: Tree`; `run.ok?` → `Result[Exit, ProcError]`; `ProcError`
  slimmed to domain data, span/stack in the `FailureContext` envelope.
- **The [failure model](/failure-model/)** — one `Failure { err, context }`, poison
  (propagating) ↔ `Result.Err` (caught via `?`), the inferred error union
  `E1 | E2 | …` exhaustively matched.

**[lean] — confirm or redirect:** the `Command` carries `outputs` + both execution
and canonical argv (§2); the surviving-kwargs verdict (§5).

**[settled] — building toward:** the projection/runtime boundary (§0); the demand
triple (§1); identity from content hashes + program identity + env (§3); the
executor/observer split + closure-shipping (§6); incremental write-set +
success-orthogonal-to-outputs + auto-commit (§8).

## 13. First implementation slice (orientation)

The thin-but-**complete** spine — no demo shortcuts:

- a real orchestrator↔executor split over vox (in-process executor first; remote is
  a different transport, not a different design),
- real closure-shipping of the observer + executor-side evaluation (shared
  evaluator),
- the `Command` value consumed by `exec`, mounts/outputs derived from it,
- ③ produced outputs crossing back via the remote-part handle (`.ok`, the write-set
  tree, streams as produced handles).

**Deferred** (design accommodates, slice omits): L1/L2 caching, the capability
registry + real remote executors, `OpenForeign`/nested grammars, the impurity
escape hatch, `InOut` rewrite inputs.

## Related

- [The demand-driven runtime](/runtime-semantics/) — the evaluation model, values,
  poison, and the producing handle `exec` outputs ride on.
- [The strand scheduler](/strand-scheduler/) — the demand-driven concurrency that
  parks a strand on a remote part and resumes it.
- [Failures, poison, and Result](/failure-model/) — `run.ok` / `run.ok?`,
  `ProcError`, the poison ↔ `Result` model.
- [Decide the process model](/process-as-first-class-value/) — `.ok` / `.stream`.
- [Cache and trust](/cache-and-trust/) — the two cache levels and observer
  cacheability.
- [Executor abilities](/executor-abilities/) ·
  [Design the executor scheduler](/design-executor-scheduler/) — routing & placement.
- [Implement streams as the first Tier-2 primitive](/streams-first-primitive/).
- [Design build-output materialisation](/materialise-build-output/).
