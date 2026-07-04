# Tooth 2 — frames and the calling convention (session agenda)

*The co-design session document for fable functions, prepared per the
standing agreement: the ABI is the one place a wrong shape means a
rewrite, so it gets ruled with Amos before code exists. Every question
is self-contained. My recommendation is marked on each; nothing here
is decided until the session says so.*

## What exists today (the ground truth this builds on)

- **The stencil discipline (weavy jit)**: a whole compiled chain runs
  in ONE driver-owned C-stack frame; threaded state is an explicit Ctx
  struct; `become` tail calls mean no stencil holds live C-stack state
  across its continuation; suspend = return up; resume = re-enter the
  chain at a recorded offset. Proven, including over real async.
- **The runner (weavy::run_dense)**: fable's plans already execute
  through weavy's generic runner, which owns an explicit program
  frame-stack (inline up to 16, spills to heap). Programs call blocks;
  tail calls reuse frames. This is CONTROL flow only — there is no
  DATA frame (locals/args) anywhere yet, because fable has no
  functions: plans are straight-line scripts over one borrowed value.
- **Layouts (weavy::mem + mem::declared)**: records at byte offsets
  with padding proofs; language-declared types get deterministic
  packed layouts. The same machinery can lay out anything record-
  shaped — including a frame.
- **Raw memory (weavy::mem::runtime)**: RawScratch (aligned raw
  allocations), ScratchSession (reserve/release Layout-shaped slots
  with reuse), array builders with drop discipline.
- **Rulings that constrain everything**: typed instructions over
  untagged operands (A6 — the frame's contents are typed by the
  lowering, never self-describing); suspension only for the started-
  and-blocked; one unified trace via strippable IR instrumentation;
  vix's memo boundary = a call boundary (aggregation unit = memo
  unit), and the demand driver will schedule MANY small tasks — frames
  must be cheap to create and cheap to park.

## Q1 — What is a frame, physically?

When fable gains `fn`, a call needs somewhere for arguments and
locals, and that somewhere cannot be the C stack (suspension must
survive returns through the driver). Options:

(a) **A per-task frame arena**: contiguous language-managed memory per
    task; calls bump-allocate a frame, returns pop it; a frame pointer
    lives in Ctx.
(b) **ScratchSession slots per frame**: each call reserves a slot;
    frames are non-contiguous; parking is per-slot bookkeeping.
(c) **Frames inside weavy's runner frame-stack**: extend the existing
    control frames with data payloads.

**Recommendation: (a)**, with the frame's SHAPE computed by the same
declared-layout machinery as any record: a frame IS a declared struct
of (args, locals, spill slots), packed deterministically, offsets
known at lowering time. Consequences that fall out: suspending a task
costs nothing (the live frame chain already sits in the arena — stop
running and the state is simply still there); the DWARF/introspection
story is uniform (a frame has a Layout like any value; wtf and the
debugger read frames the way they read values); arena create/park must
be cheap because vix will have thousands of small tasks (an arena is a
RawScratch growable region — measure, don't guess, but the shape
allows cheap).

## Q2 — How do arguments travel?

Options: operand-stack passing (callee pops what caller pushed);
register-style (first N args in dedicated Ctx slots); or
**frame-direct**: the caller writes each argument INTO the callee's
frame at its statically known offset before entering the callee.

**Recommendation: frame-direct**, because static typing makes it free
— the caller's lowering knows the callee's frame layout (it's a
declared record), so argument passing is ordinary typed stores; there
is no marshalling boundary, no variadic mystery, nothing to interpret.
Scalar returns come back in one designated Ctx slot; composite returns
are written by the callee through a caller-provided out-slot (the
caller reserves the return's layout in ITS frame and passes the
offset). This is the classic C-ABI sret shape, chosen because layouts
are first-class here.

## Q3 — Where exactly does suspension cut, and what must be true there?

Ruled already: suspension is task-level (the whole frame chain parks),
only for started-and-blocked. The ABI question is the INVARIANT AT THE
CUT: between await points, stencils and the register allocator may
keep values wherever they like; AT an await point, every live value
must be IN THE FRAME (or already consumed). Call it the **await-spill
rule**: awaits are spill boundaries, enforced by the lowering, so a
parked task's complete state is exactly (frame arena + resume offset
per chain + the await index it's blocked on) — nothing volatile,
nothing implicit.

**Recommendation**: state the await-spill rule as a named ABI rule now
and build the tooth-3 host-async work on it; it is also what makes
the suspended state INSPECTABLE (the debugger reads parked frames via
their layouts — the T0/T1 debuggability story generalizes from the
operand-stack snapshot to real frames).

## Q4 — Calls across the suspension boundary (tooth-3 preview, ABI reservation now)

A host call that cannot complete immediately is an await point: the
task parks, the host's future wakes the task's readiness slot, resume
re-enters the callee's chain at the recorded offset. The tooth-2
decision is only a RESERVATION: await points are numbered per
function (as in the proven protocol), and the frame layout includes
whatever the resume needs beyond the frame itself (nothing, if the
await-spill rule holds — that's the point of Q3).

## Q5 — What grows in weavy, in what order?

The typed instruction vocabulary (successor to the i64 spike, per the
A6 note in weavy::r#async): per-kind arithmetic (AddI64/AddF64/...),
typed load/store at frame offsets, call/return/frame ops, await, and
INSTRUMENTATION ops (the unified-trace ruling — emitted freely,
stripped by mode). **Recommendation on stencil order**: call/return/
frame-alloc stencils land BEFORE arithmetic variety — one arithmetic
op suffices to prove the frame ABI end to end; diversity is mechanical
afterward.

## Q6 — Trace events at frame granularity

Frame-enter/frame-exit/await-parked/task-resumed become instrumentation
ops with static ids. The milestone's absence-proofs then work at
function granularity ("this function never entered") and wtf's
suspended-task view reads parked frames through layouts.
**Recommendation**: the event vocabulary is part of the ABI document,
versioned with it, so the trace never lies about frames.

## What this unlocks, in order

Functions in fable (tooth 2 proper) → host async through the same
frames (tooth 3) → vix node bodies as functions whose call boundary is
the memo boundary, scheduled by the demand driver as tasks — the
milestone demo's shape: compound value, one path selected, unselected
producers provably never entering a frame at all.
