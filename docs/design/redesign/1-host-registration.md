+++
title = "Redesign · Host-fn registration (+ the typed frame path)"
+++

# Host-fn registration

Verdicts **V4** (registration with stable ids, not a fixed indexed array) and
**V7** (typed frame path, not raw poking). This is the fix Amos picked to start,
because the horror that surfaced here — 57 closures capturing per-burst mutable
state — is real. But part of it is *forced*, and getting that distinction right is
the whole point of this analysis: we fix the gratuitous part and leave the forced
part alone.

## How it works today (grounded)

Everything lives in one function: **`fn burst(&mut self, exec, exec_ix) -> Burst`
(`driver.rs:2387`)**, ~2,300 lines. It:

1. Sets up per-burst shared state — at least these six (a full catalog is step 0,
   below):
   - `invoke_region: usize` (`2389`), `primitive_region: usize` (`2393`) — frame
     offset bases for host-op args
   - `store_cell = &self.store` (`2414`) — the store, shared ref
   - `store_events = RefCell::new(Vec::new())` (`2421`) — event buffer
   - `projection_reads = RefCell::new(Vec::new())` (`2422`) — **read-set buffer**
   - `host_error = RefCell::new(None::<String>)` (`2664`) — error cell
2. Defines **57 closures inline** (`let mut store_alloc = |frame: &mut [u8]| {…}`),
   each reading `read_frame_word(frame, primitive_region + N)`, borrowing
   `store_cell`, and dumping failures into `host_error`.
3. Builds `let mut hosts: [HostFn<'_>; 57] = [ &mut invoke, &mut store_alloc, … ]`
   (`4627`), positionally — **the array index IS the host id**.
4. Runs `exec.task.advance(&self.program, …, &mut hosts)` (`4688`), drops the array
   (`4689`), then **drains the captured buffers**: `store_events → emit`
   (`4690`), `projection_reads → exec.read_set` (`4693`), `host_error → Burst::Error`
   (`4696`).

Addressing: the `pub const *_HOST: u32 = N` constants (`driver.rs:112–168`) are
those indices; lowering emits `Op::HostCall { host: STORE_ALLOC_HOST }` at ~30
sites in `lower.rs` (importing the constants). weavy dispatches `hosts[host]`.

## What is FORCED vs. what is gratuitous

**Forced by weavy** (do not "fix" — you'd be fighting the substrate):
`weavy::task::HostFn = &mut dyn FnMut(&mut [u8])` (`task.rs:168`) gives a host op
**no context parameter** — only the frame. And `run_hosted(hosts: &mut [HostFn])`
holds all 57 closures live simultaneously. In Rust, N closures each needing
mutable access to shared state → **interior mutability (`RefCell`) captured by
shared ref** is the idiomatic, and here essentially the only, solution. The
capture is not idiocy; it's the shape weavy's calling convention imposes.

**Gratuitous** (this is what we fix):
- **Six separate ad-hoc captures** instead of one bundled context.
- **57 closure bodies inlined in a 2,300-line function** — unreadable, untestable.
- **Hand-assigned magic-number ids** (`_HOST: u32 = N`) — no registration, and no
  stable identity, so a warm-start cache of the lowered weavy program (which
  stores `host: N`) would break the instant anyone reorders the table.
- **Raw frame poking** (`read_frame_word`/`as usize`/silent `unwrap_or`) — V7,
  separable (see end).

## The fix

### ADD
- **`HostCtx`** — a struct bundling the per-burst shared state. The **complete
  catalog** (grounded, `burst` setup `2388–2664`) is ~15 things, not six, and this
  matters:
  - shared refs: `descriptors` (`2412`), `schema_refs` (`2413`), `store = &self.store`
    (`2414`), `lowered_fns = &self.fns` (`2420`); `invoke_region`/`primitive_region`;
    `force_flesh_copy: bool` (`2425`)
  - buffers: `store_events` (`2421`), `projection_reads` (`2422`), `host_error` (`2664`)
  - **`RefCell<&mut self.…>` borrows of five disjoint mutable fields**:
    `flesh` (`2415`, `&mut exec.flesh`), `ast_roots` (`2416`), `oci_file_memo`
    (`2417`), `journal` (`2418`), `trace_clock` (`2419`)
  Held behind **one** shared cell. This is the *forced* capture, reduced from ~15
  ad-hoc captures to one bundle — the real win.
- **A capability registry** — an ordered list of registered host ops, each:
  `{ name: &'static str /* stable id */, body: fn(&mut HostCtx, &mut [u8]) -> Result<(), String> }`.
  The **index is derived from registration order** (weavy still dispatches by
  index — that's its convention), but the **name is the stable identity** that
  lowering and the warm-start cache reference.

### CHANGE
- The 57 closure **bodies → free functions** `fn op(ctx: &mut HostCtx, frame:
  &mut [u8]) -> Result<(), String>`, moved **out of `burst`**. Now readable and
  unit-testable in isolation.
- The dispatch table is **built from the registry**, not hand-written: iterate
  registered ops → `Vec<BoxedHostFn>` (weavy's `BoxedHostFn = Box<dyn FnMut(&mut
  [u8])>`, `task.rs:171`; `TaskExec` already accepts `Vec<BoxedHostFn>`), each a
  thin wrapper `move |frame| body(&mut ctx.borrow_mut(), frame)` capturing the one
  `&RefCell<HostCtx>`. The hand-maintained `[HostFn; 57]` at `4627` disappears.
- **Lowering references ops by name**: `self.host_id("store_alloc")` resolves via
  the registry to the index at lowering time. The `_HOST` constants either vanish
  or become generated-from-registry.

### REMOVE
- The inline `[HostFn; 57]` array (`4627`).
- The hand-assigned `pub const *_HOST: u32 = N` block (`112–168`).
- The 57 inline closure definitions inside `burst` (bodies relocated).

### HOW WARM-START WORKS (the point of V4)
The lowered weavy program records capability **names**, not raw indices. On reload,
the registry maps each name → whatever index it has this run. A lowering cached to
disk therefore survives the table being reordered or extended — which a position
index never could. That is exactly the warm-start property note 5 asked for.

**The honest caveat:** capture does **not** go to zero. weavy won't pass a context
parameter, so the ops still reach shared state through *one* captured
`&RefCell<HostCtx>` instead of six ad-hoc captures. If "kill the capture entirely"
was the mental model, that's not achievable without fighting weavy — and it
shouldn't be the goal. The goal is: one bundled context, bodies out of the
mega-function, stable names.

## Blast radius
- `burst` shrinks by ~1,500+ lines (57 bodies leave).
- ~30 `Op::HostCall` sites in `lower.rs` change from a constant to a name lookup.
- `_HOST` constants (112–168) removed/regenerated.
- weavy dispatch: **unchanged** (still index-into-slice).
- Behavior: **unchanged** → corpus stays green (that's the guardrail).

## Incremental sequence (corpus green at every step)
0. **Capture catalog — DONE** (the ~15 fields above). The one surprise: the ops
   mutate **five disjoint `&mut self` fields** through RefCells, not just the store
   — so `HostCtx` is a bundle of several borrows, which is exactly what makes the
   reentrancy question below real.
1. **Bundle → `HostCtx`.** Move the six (+catalog) shared fields into one struct;
   closures capture `&RefCell<HostCtx>` instead of six things. Pure de-scatter, no
   behavior change. Green.
2. **Bodies → free functions**, in batches; the array entries become wrappers.
   Green after each batch.
3. **Introduce the registry**; build the dispatch `Vec<BoxedHostFn>` from it;
   delete the hand array. Green.
4. **Name-based lowering**; remove the `_HOST` constants. Green.
5. *(separate fix)* **V7 typed frame accessors** — see below.

## Risks / open questions for Amos
- **Reentrancy — the one real correctness risk.** `HostCtx` doesn't just hold the
  store; it holds `RefCell<&mut …>` borrows of **five disjoint `self`/`exec`
  fields** (`ast_roots`, `oci_file_memo`, `journal`, `trace_clock`, `flesh`).
  Today that's exactly why the current code uses per-field RefCells rather than
  `&mut self`. If any host op **re-demands** — re-enters the machine, which would
  want `&mut self` — while those borrows are live, it panics. `invoke` (`2426`)
  and the `pending` ops are the suspects. **Audit re-entry before choosing the
  cell granularity**: a single `RefCell<HostCtx>` is cleanest but only safe if no
  op re-enters; otherwise keep the borrows field-granular (as today) inside the
  bundle. This is the audit that gates step 1, and it's a genuine open question,
  not a formality.
- **Stable identifier form** (note 5): a `&'static str` name, or a content-hash of
  the op's signature? Name is simpler and human-legible; hash is
  rename-stable. My lean: name.
- **Scope of "registration."** This proposal is the *full* version (bodies become
  ctx-taking free fns — kills the inline-in-`burst` horror). A *minimal* version
  (registry owns names+order, closures stay inline) is faster but leaves the
  horror. I recommend the full version, staged as above; it's more work but it's
  the thing you reacted to.
- **V7 coupling.** Typed frame accessors (replacing `read_frame_word` with the
  declared-record accessors weavy's `Op::HostCall` frame already implies) is a
  *per-op* change that rides naturally on step 2 (each body, once relocated, can
  adopt typed reads). I'd keep it a **separate, later** pass so the registration
  fix stays behavior-preserving and mechanical — but note the two are adjacent.
