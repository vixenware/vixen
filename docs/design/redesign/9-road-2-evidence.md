+++
title = "Redesign · Road-2 evidence — the measurements that settled the fork"
+++

# Road-2 evidence — the measurements that settled the fork

2026-07-07. Two measurement spikes (`spike-c-cdcl-rematch`, `spike-d-lr-loop`)
and the day's fold sequence on `rodin` together answer the question the whole
redesign set was building toward: does a vix hot loop have to live outside
vix (Python/C via FFI), or can it live in vix itself? This doc is the evidence
trail for that decision, not a new analysis — sources are
`spike-c-cdcl-rematch/REPORT.md`, `spike-d-lr-loop/REPORT.md` (both spike
worktrees), and `RESURRECTION.md`'s "DAY RESULTS" section (session handoff
notes, `~/oss/facet-cc`).

## The decision

Amos decided the fork: **hot loops live in vix, not behind FFI.** FFI is for
atomic-pure functions and effects only — not a kernel-offload escape hatch for
performance. The acceptance bar was relaxed from whatever stricter number the
inventory implied to **~50× Rust** ("probably conservative" — reuse and
correctness-for-free are judged to justify the relaxation). Both spikes below
were run against that bar.

## Ladder 1: the CDCL trail — hash confounder and copy amplification, separated

Spike C's job was to split two effects that a prior "flesh reuse" measurement
had conflated: content-hashing cost and copy-avoidance. The CDCL trail
(monotonically-growing `Vec<i64>`, intern each growing prefix — the shape
`rodin`'s solver produces once per decision) was benchmarked in isolation
across several shapes (`vix/src/bin/cdcl_molten_bench.rs`).

**The direct store-intern path (4096 prefixes), against a 267µs Rust
`rodin_core::solve_round3_record` baseline:**

| Stage | Median | Factor vs Rust | What changed |
|---|---:|---:|---|
| Before | 1948.417 ms | 7,292× | Direct growing-prefix intern, O(N²) rehash from scratch every intern |
| After append-hash (SHA-256) | 4.491 ms | 16.8× | `ValueStore::alloc_array_words_append`: prefix metadata + O(1) append-combine |
| Synthetic blake3 projection | 1.693 ms | 6.3× | Benchmark-only hash-family swap, not a runtime vix change |

stax confirmed the mechanism at each stage: before, 88% of active time was
`sha2::sha256::compress256` under `ValueStore::alloc_array_words`, called once
per prefix per element — the O(N²) walk. After, 95.9% of samples fell under
`alloc_array_words_append_for_bench`, with SHA-256 still the largest residual
(~50%) but the O(N²) trunk itself gone — remaining cost is per-append hashing
plus `HashMap`/SipHash dedup bookkeeping (~25-30%). **This ladder proves the
incremental append-hash technique**: the fix is a data-structure gap (no
memory between intern crossings), not an algorithm gap — even blake3
dropped in 1:1 is still O(N) per intern without it. It's slated to
productionize inside the planned V2 hash epoch (below), which is where the
blake3 swap becomes a real runtime change instead of a synthetic projection.

**The language-shape ladder — same workload, but through the vix language
instead of driving `ValueStore` directly:**

| Shape | Steps | Median (first pass) | After `0b62d0ec1` (consuming-move) |
|---|---:|---:|---:|
| `machine-rebind`, `let trail = trail.push(lit)` | 2048 | 1.6–3.7 ms (6.0–14.0×) | 70.792 µs (0.27×) |
| `machine-chain-reuse` (fresh-temporary chain) | 2048 | 0.10–0.20 ms (0.39–0.73×) | 66.916 µs (0.25×) |
| `machine-chain-copy` (forced-copy comparator) | 2048 | 1.4–2.4 ms (5.4–9.0×) | 786.583 µs (2.94×) |

Before the consuming-move fix (doc 7), the named-rebind idiom —
`let trail = trail.push(lit)`, the shape every accumulator loop actually
writes — didn't hit molten reuse at all: `resolve_binding` emitted
`MOLTEN_DUP` for the rebound receiver, `refs` went to 2, and the `ARRAY_PUSH`
`refs == 1` gate never fired. The number in that row was measuring **copy
amplification**, not the reuse mechanism. After `0b62d0ec1` landed (doc 7's
consuming-move rule), named-rebind measures **70.8µs at 2048 pushes** — within
6% of the fresh-temporary reuse ceiling (66.9µs) and *faster than the Rust
reference* (0.27× / 0.25×, though the reference here does a full CDCL solve
round, not an isolated push loop — a like-for-like caveat, not a like-for-like
claim). A stax profile of the fixed named-rebind path shows **zero SHA-256
samples**: 97.2% of active time is under `weavy::task::Task::run_hosted` /
`Driver::burst`, dominated by `String::clone`/allocator work, not content
hashing — the molten-reuse thesis (mutation in place, no per-push
intern/hash) is now directly visible in a flame graph, not just inferred from
wall-clock numbers.

## Ladder 2: the LR loop — a 232,000× headline, decomposed into three fixable causes

Spike D asked a harder question: can a parser's LR shift/reduce loop live in
vix as-is, informing the Snark-for-binary lowering decision (doc 6 of the
prior set)? The measured result at the time: **~232,000× Rust** at 10k tokens
(interpreter), **~1.30M×** on the JIT lane (JIT was *slower* than interp — a
separate anomaly, below), and a 100k-token run **didn't finish within a 120s
alarm** (lower bound `>770,000×`). Against the relaxed ~50× bar, that's not
close — but the report's contribution isn't the headline number, it's the
attribution: the 232,000× decomposes into three independent, separately
fixable costs, only one of which is inherent to the LR shape itself.

**1. O(N²) copy amplification from named reads — fixed by consuming-move
(doc 7).** The committed LR benchmark reads its token/parse stacks by name
every iteration (`stack.push(...)`, `tokens.pop()...` as call arguments, not
the `let x = x.push(...)` shadowing shape) — `resolve_binding` dups every
read, `refs` never stays 1, the reuse gate never fires. Spike D's own
"fresh-temporary reuse control" (array ops through fresh temporaries, not
named rebinds) isolates what reuse alone is worth: interpreter copy → reuse is
a **2.64× win**, with a reuse-on floor of ~77.5 ns/array op (67.5× Rust
`Vec`, not near-Rust, but not the LR loop's problem either). Caveat worth
keeping precise: re-running the *unchanged* LR benchmark after `0b62d0ec1`
landed showed **no improvement** (679,732 ns/action vs. 672,316 before,
essentially flat) — because the committed generated source doesn't use the
shadowing idiom the consuming-move rule matches. The fix is proven (ladder 1),
but this specific benchmark artifact needs rewriting to `let stack =
stack.push(...)` / `let stack = stack.pop().1` shape to actually exercise it;
that rewrite wasn't done as part of this spike.

**2. Per-iteration INVOKE interning — fixed by tail loops (doc 8), landed
later the same day.** Spike D's most load-bearing finding: *every*
source-level vix call lowers through `Op::HostCall { host: INVOKE_HOST }` (`
lower.rs:2838`→`3297-3332` at spike time), never a task-local `Op::Call` — so
recursion, the only way vix could express a loop before this day's tail-loop
work landed, is *always* a demand boundary. `INVOKE_HOST`'s closure interns
molten arguments for the memo key on every call (`intern_molten_word`), so a
recursive LR loop paid a full canonical-hash-and-intern cycle on its
accumulator every single iteration — this is the actual mechanism behind the
232,000× number, confirmed by stax: 77–84% of active time under
`intern_molten_word`, 60–63% under `ValueStore::alloc_array_words`, SHA-256 as
the dominant leaf. A generated straight-line control (all iterations unrolled
into one function body, consuming-move idioms throughout, no recursion) proved
the fix direction: SHA-256 and `intern_molten_word` **vanish from the
profile entirely**, landing at ~878 ns/action (≈217× Rust — still above the
50× bar, residual now host-call/burst dispatch and allocation, not hashing).
That control isn't a parser loop and stack-overflows lowering past ~576 terms
(recursive-descent code generation, not the language runtime), so it bounds
the ceiling without being a usable benchmark. The real fix — a loop construct
that keeps a self-tail-recursive accumulator inside one demand region — is
exactly doc 8's self-tail-call → `Op::Jump` lowering, landed in the five
commits immediately following this spike's write-up. Doc 8's own proofs
(`tail_loop_array_accumulator_handles_100k_iterations`,
`store_alloc_count == 1` across 100k iterations) are the direct, later
confirmation that per-iteration INVOKE interning is eliminated for exactly
this shape.

**3. JIT recompile-per-spawn — root-caused, fixed by a compile cache (folded
same day, `jit-compile-cache`).** The JIT lane measured *slower* than the
interpreter throughout spike D (2.58×–5.6× slower depending on rebase point),
which is not supposed to happen. stax on the unrolled straight-line control
isolated it: 60.6% of JIT-lane active time was under `Driver::spawn →
JitProgram::compile → compile_fn`, i.e. the JIT was recompiling the program on
every spawn instead of caching compiled code per-program. This was root-caused
from spike D's evidence and fixed the same day by a separate fold (JIT compile
cache, per-program not per-spawn — listed in `RESURRECTION.md`'s end-of-day
fold sequence), independent of the tail-loop and consuming-move work.

## What remains

With causes 1 and 2 fixed (consuming-move + tail loops) and cause 3 fixed
(compile cache), the residual costs identified across both spikes are:

- **Host-call/task dispatch overhead.** The fixed named-rebind CDCL profile
  (ladder 1) still spends its active time under
  `weavy::task::Task::run_hosted` / `Driver::burst`, dominated by
  `String::clone` and allocator work — not algorithmic, but real per-op
  overhead in the interpreted dispatch/frame path.
- **Allocation.** Both spikes' post-fix profiles show allocator
  (malloc/free/memmove) frames as visible residuals once hashing and copy
  amplification are gone — smaller than either fixed cost, not yet zero.
- **Store-side hashing and dedup**, for the *direct* per-prefix store-intern
  path specifically (not the language-loop path, which no longer touches the
  store per iteration once molten): SHA-256 is still ~50-54% of active time,
  `HashMap`/SipHash dedup bookkeeping ~20-30%. The synthetic blake3 projection
  (6.3× → 5.2× across the two spike-C passes) is close to the 50× bar but not
  a clean pass against the measured Rust baseline, and it's a benchmark-only
  projection, not a landed runtime path yet.
- **Dense-table array indexing.** Spike D's LR benchmark couldn't express the
  requested dense 2D `(state, token)` action table because vix arrays have
  `.push`/`.pop`/`.set`/`.len` but no dynamic `.get`/index op — so the LR
  measurements exclude table-lookup cost entirely and aren't an optimistic
  result for that reason alone. Unmeasured, not yet built.

## Planned next steps

- **Productionize the incremental append-hash + blake3 swap** inside the
  planned V2 hash epoch (branch `v3-hash-epoch`, fired but not yet landed at
  time of writing) — turns ladder 1's proven technique and synthetic blake3
  projection into an actual runtime hashing path, alongside the V1/V3
  container-descriptor work it's staged with.
- **`MOLTEN_DROP` via last-use analysis** (doc 7's known gap) — the array
  reuse mechanism that made ladder 1's named-rebind number possible is
  soundness-only today (never wrong, may miss reuse after a value has been
  aliased and later abandoned); this is the next unlock for cases that don't
  fit the current syntactic consuming-move pattern.
- **Array indexing (`ARRAY_GET`) + bounds-check elision for proven table
  dimensions** — needed before a real dense-table LR/parser benchmark can be
  built and measured at all; currently the gating unknown for whether Snark
  parse kernels can move into vix.
- **Rewrite the LR benchmark to the shadowing idiom** (`let stack =
  stack.push(...)`) so the consuming-move fix (already proven in isolation,
  ladder 1) can actually be measured against the LR shape it was motivated
  by, rather than remaining a plausibility argument.
- **Re-run the LR ladder with tail loops + the JIT compile cache both
  active** — neither fix existed yet when spike D's numbers were taken; the
  212× unrolled-control ceiling was measured without them, and the report's
  own projection is that the combination moves the plausible ceiling from
  "hundreds-of-thousands-times Rust" to "low hundreds," which is still above
  the ~50× bar and needs a real measurement, not just the projection, before
  the Snark-parsers-in-vix question can be closed either way.
