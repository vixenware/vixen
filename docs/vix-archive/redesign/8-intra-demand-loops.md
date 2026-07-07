+++
title = "Redesign · Intra-demand loops — self-tail-calls as backward jumps"
+++

# Intra-demand loops — self-tail-calls as backward jumps

Landed on `rodin` 2026-07-07 across five commits: `83373c674` (tail-position
analysis + diagnostics), `9ecfc1f5c` (lowering to `Op::Jump` + the await
ready-token fix), `37d93d83d` (100k-iteration proofs), `92ad4d719` (fallback on
schema-changing args), `fc0eff22d` (API tidy, no semantic change). This closes
the gap spike D found (doc 9): vix source recursion is structurally an INVOKE
demand boundary, so before this landed, no vix loop could keep an accumulator
molten across iterations — every iteration paid a fresh spawn/memo-key/intern
cycle. The fix: detect a self-call in tail position and lower it to a
same-frame backward jump instead of a host call.

## The mechanism

**Tail-position dispatch.** `tail_expr_expected` (`vix/src/machine/lower.rs:3072-3089`)
is the entry point: for `ast::Expr::Call` in tail position, it tries
`self.self_tail_call(call)?` before falling back to the ordinary expression
lowering. It's driven from `tail_block` (`lower.rs:3035-3039`, called once for
a function's body at `lower.rs:2980`) and recurses through `tail_match_expr` →
`match_expr_outcome` (`lower.rs:3539-3552`), so a self-call in the tail
position of a `match` arm is still tail position — this is what makes the
idiomatic shape (`match n { 0 => acc, _ => f(n - 1, acc') }`) loop, not just a
bare-block tail call. The outcome type, `TailOutcome::{Value(ValueSlot),
Jumped}` (`lower.rs:2691-2694`), lets a function body either `Ret` a value or
end having already jumped — `FnLowerer::lower` only emits `Op::Ret` on
`TailOutcome::Value` (`lower.rs:3000-3009`).

**Self-call detection.** `self_tail_call` (`lower.rs:4015-4067`) resolves the
callee name and checks it against `self.current_fn_name`/`current_fn_ref`
(both threaded through `LowerEnv`, `lower.rs:2818-2827`) — only an exact
same-function reference qualifies, not just same-name-different-module or a
polymorphic dispatch. It bails to `Ok(None)` (ordinary lowering) on: a
non-identifier callee, an unresolved name, a different function, a partial
application (`arg_list_has_partial`), or `self.force_tail_invoke` being set
(the `VIX_FORCE_TAIL_INVOKE` env var / `Machine::set_force_tail_invoke`,
`lower.rs:403-408`, `509` — a differential-testing knob that forces every
self-call through INVOKE even when it could loop, used as the baseline in
`tail_loop_matches_forced_invoke_result_and_readset` below).

**Parallel-staged param rebinding.** `emit_self_tail_jump`
(`lower.rs:4069-4099`) is the actual jump emission, in two passes:

```rust
let mut temps = Vec::with_capacity(args.len());
for (arg, schema) in args.into_iter().zip(param_schemas) {
    let arg = self.coerce_to_schema(arg, schema)?;
    if arg.schema != *schema {
        return Ok(false);
    }
    temps.push(self.copy_value(&arg));
}
for (param, temp) in self.param_slots.clone().into_iter().zip(&temps) {
    self.code.push(Op::CopyI64 { dst: param.slot, src: temp.slot });
}
self.code.push(Op::Jump { target: self.loop_header });
```

Pass one evaluates every new-argument expression and copies it into a *fresh*
temp slot; pass two overwrites the real `param_slots` from those temps; only
then does it jump. Staging through temps is what makes this safe for
argument-swapping recursion (e.g. `f(b, a)`): writing a param slot in the same
pass that's still being read by another argument's evaluation would clobber it
before it's read. `self.loop_header` is captured once, at function-entry
lowering time (`lower.rs:2972`, right after locals are laid out and before the
body lowers), so `Op::Jump { target: self.loop_header }` always lands on the
function's first body instruction — this is the whole "loop": one frame, one
spawn, params overwritten in place, jump backward, no new INVOKE, no new
demand.

**Fallback on schema-changing args.** If a coerced argument's schema doesn't
exactly match the declared parameter schema, `emit_self_tail_jump` returns
`Ok(false)` instead of emitting the jump (`lower.rs:4083-4086`) — this covers
the case where the accumulator's *shape* would change across iterations, which
a fixed-layout backward jump can't represent (the loop reuses the same frame
slots every iteration; a slot can't hold two different schemas). `self_tail_call`
snapshots the lowerer's state before attempting the jump
(`self.snapshot()`/`FnLowererSnapshot`, `lower.rs:3017-3032`) and restores it
on `Ok(false)` (`lower.rs:4063-4066`), so the abandoned jump attempt leaves no
trace in the emitted code, and lowering falls through to the ordinary `call()`
→ INVOKE path for that call site instead of erroring.

**Non-tail self-calls stay INVOKE, with a diagnostic.** `self_tail_call` is
only ever invoked from `tail_expr_expected` — the non-tail expression path
(`expr_expected`, `lower.rs:3397`, `Call` arm) always goes through `call()`
(`lower.rs:3871-3968`) → `invoke_fn` (`lower.rs:3974-4013`), which emits the
normal `Op::HostCall { host: INVOKE_HOST }` + `Op::Await` pair — full memo
boundary. A separate diagnostics pre-pass, `tail_self_call_diagnostics`
(`lower.rs:1639-1648`, walking `TailPosition::{Tail, NonTail}` via
`collect_tail_self_calls_expr`, `lower.rs:1689+`), runs once per function
during module compilation (`lower.rs:670-675`) and records a message —
`"self-call at {start}..{end} is a demand boundary (not tail position) - not
looped"` (`lower.rs:1698-1707`) — for every self-call it finds outside tail
position, surfaced via `Machine::diagnostics()` (`lower.rs:410-411`). This is
advisory, not an error: the function still lowers and runs correctly through
INVOKE, the diagnostic just tells you why a given self-call didn't get the
loop treatment.

## The await ready-token protocol

A backward jump means the same `Op::Await` site inside a loop body (any
non-self `INVOKE` a loop iteration makes) gets re-entered on every iteration —
something that never happened before this change, since without loops every
`Await` executed at most once per spawn. The pre-existing protocol left the
`ready` flag unconsumed on the successful-read path ("NOT consumed on the
pending path so a resume re-reads the same descriptor, idempotent per suspend
point" — the old comment, replaced in this change), which is exactly wrong for
a re-entered await: a stale `true` from a prior iteration's already-resolved
await would let a *new* iteration's await at that same program position read
a value that was never produced for it. `9ecfc1f5c` fixes this by making
`ready` a consumable token across all three execution lanes:

- **Interpreter** — `Op::Await { dst, input }` (`weavy/src/task.rs:110-116`,
  doc comment updated to "if `input` is ready, consume that ready token...").
  The read path now does `if let Some(is_ready) = ready.get_mut(idx) &&
  *is_ready { *is_ready = false; ... }` (`weavy/src/task.rs:485-490`) —
  `ready` changed from `&[bool]` to `&mut [bool]` throughout
  (`Task::run`/`run_hosted`, `weavy/src/task.rs:294`, `299`; the `Advance`
  trait, `:517`; `Running::advance`, `:533`).
- **JIT driver** — `weavy/src/jit/task_lane.rs`: `Ctx.ready` becomes `*mut i64`
  (`:29`, was `*const i64`). `JitTask::run_hosted` copies the caller's `&mut
  [bool]` into an owned `ready_scratch: Vec<i64>` before entering native code
  (`:513-515`), hands the scratch buffer's mutable pointer to the stencil ctx
  (`:538`), and copies the (now-mutated) scratch buffer back into the caller's
  slice after the native call returns (`:550-552`) — round-tripping the
  consumption the native code performed.
- **Stencils (copy-and-patch native code)** — `weavy/stencils/task_ops.rs`:
  `Ctx.ready` is `*mut i64` (`:35-36`), and `weavy_task_await`
  (`:238-258`) now does `let ready = c.ready.add(index); if *ready != 0 {
  *ready = 0; ...}` — the actual token-clearing instruction sequence emitted
  into the native code path, with the doc comment updated to "The ready token
  is consumed on the successful read path."

(There's a separate, older `weavy/stencils/async_ops.rs` "ASYNC (suspend/resume)"
lane whose `weavy_async_await` still uses `*const i64` and does not consume the
token — it predates this feature and is not part of the task-frame lane that
self-tail loops run on; not touched by this change.)

## Proof: the nine tripwires

All in `vix/tests/demand_driven.rs`, all run across both lanes (`lanes()` →
`Lane::{Interp, Jit}`, `vix/src/machine/driver.rs:174-179`):

1. **`tail_loop_emits_one_demand_per_entry_zero_per_iteration`** (`:990-1004`)
   — `countdown(12, 0)` (12 self-tail iterations): `demanded_count == 1`,
   `spawned_count == 1`, `spawned_invocation_count == 1`, `completed_count ==
   1`. One demand, one spawn, one completion covers all twelve iterations —
   this is the headline property, "one demand per loop entry, zero per
   iteration."
2. **`tail_loop_matches_forced_invoke_result_and_readset`** (`:1006-1141`) —
   runs a struct-carrying loop with the jump lowering enabled vs.
   `set_force_tail_invoke(true)` (forced INVOKE) and asserts equal results,
   equal exported values, that the enabled trace is a strict subsequence of
   the forced-INVOKE trace (`:1082-1085`), and equal projection-verified
   counts after a change to an unread struct field (`:1136-1139`) — the loop
   lowering is behaviorally and read-set equivalent to the INVOKE path it
   replaces, differentially checked against that path as ground truth.
3. **`tail_loop_interp_jit_trace_equal`** (`:1143-1152`) — `countdown(9, 0)`
   produces byte-identical `DriveEvent` traces on both lanes.
4. **`tail_loop_accumulator_stays_molten`** (`:1154-1184`) — `grow(24, seed)`
   pushing onto an `Array` accumulator: `store_alloc_count == 1` ("only the
   final accumulator should intern") — no store allocation per iteration, only
   at the end, tying this doc to doc 7's molten-reuse mechanism: the tail loop
   is what lets an accumulator stay molten across iterations at all, since
   without it every iteration would cross the INVOKE demand boundary and force
   an intern.
5. **`non_tail_self_call_stays_invoke`** (`:1186-1211`) — `sum(n)` recursing
   as `n + sum(n - 1)` (non-tail): asserts the diagnostic string fires, then
   `demanded_count == spawned_count == completed_count == 7` for `sum(6)` —
   one INVOKE per call, no collapsing.
6. **`self_tail_call_inside_match_arm_does_not_spawn_per_iteration`**
   (`:1213-1220`) — `countdown(20, 0)`, `spawned_count == 1` — confirms tail
   position inside a `match` arm (not just a bare block tail) is detected.
7. **`tail_loop_body_invokes_still_demand_children`** (`:1222-1242`) —
   `sum_child` tail-loops over `n` while calling a *non-self* `child(n)` each
   iteration: `spawned_count(sum_child) == 1` but `demanded_count(child) ==
   spawned_count(child) == completed_count(child) == 6` — the loop collapses
   only the self-tail-recursion; ordinary calls inside the body still demand
   normally every iteration.
8. **`tail_loop_countdown_handles_100k_iterations`** (`:1244-1256`) —
   `countdown(100_000, 0)` completes with `spawned_count == 1`,
   `completed_count == 1` — no stack growth across 100k backward jumps in one
   spawn.
9. **`tail_loop_array_accumulator_handles_100k_iterations`** (`:1258-1286`) —
   `grow(100_000, seed)` produces a 100,001-element array with
   `store_alloc_count == 1` — the 100k-iteration and stays-molten properties
   hold together at scale.

## Memoizing tree-recursion is structurally non-tail — nothing regresses

`fib_runs_linear_on_the_machine` (`vix/src/machine/lower.rs:8108-8139`, a
pre-existing test untouched by any of the six tail-loop-landing commits per
`git log -L`) recurses as `fib(n - 1) + fib(n - 2)` — both calls are operands
of `+`, neither is in tail position, so `self_tail_call` never engages for
either. Every call still goes through `call()` → `invoke_fn` → INVOKE, and the
driver's memo on `(fn, args)` collapses the naive-exponential 13,530-body call
tree for `fib(20)` down to 21 spawns (one per distinct `n` in `0..=20`),
asserted directly (`spawns == 21`). Branching self-recursion is not tail
self-recursion by construction — a call inside a binary-op operand, a
non-final match arm expression, or any position with more work after the
call returns cannot be `TailOutcome::Jumped` — so memoized tree recursion
keeps its existing INVOKE-per-distinct-argument-set behavior unconditionally.
This is the concrete form of the doctrine `RESURRECTION.md` (2026-07-07)
states as design intent: interior iteration over a linear accumulator is not
demand and gets the jump; branching search stays memoized because each branch
is a genuinely distinct sub-problem worth caching, not a loop step.

## What this doesn't yet do

There's no general loop syntax (`while`/`for`/`loop`) — the mechanism is
specifically "a call resolves to the enclosing function, in tail position."
Mutual tail recursion (`f` tail-calls `g`, `g` tail-calls `f`) isn't covered —
`self_tail_call` requires `resolved_name == self.current_fn_name` and `fn_ref
== self.current_fn_ref`, i.e. only literal self-recursion loops today.
Schema-changing accumulators (a loop whose accumulator's type would need to
change shape across iterations) fall back to INVOKE per iteration rather than
erroring, per the fallback above — correct, but not accelerated.
