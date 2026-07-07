+++
title = "Redesign · Molten reuse — functional-but-in-place mutation"
+++

# Molten reuse — functional-but-in-place mutation

Landed on `rodin` 2026-07-07 (commit `0b62d0ec1`, "vix: consume rebound molten
update receivers", plus the prior flesh→molten rename `60807ce53`). This
documents the semantics as they exist in the tree today: what "molten" means,
when in-place mutation is invisible, the consuming-move rule that makes the
flagship `let x = x.push(v)` idiom hit it, the differential oracle that proves
unobservability, and the one known gap (no `Drop` yet).

## What molten is

A molten value is the un-interned, no-content-identity representation an
aggregate (`Array`, `Map<K,V>`, a struct, an enum payload) has while it's being
built or mutated inside a demand region. `MoltenStore { entries: Vec<MoltenEntry>
}`, `MoltenEntry { refs: usize, value: MoltenValue }`, `MoltenValue::{Record {
schema, bytes }, Map { schema, pairs }, ArrayWords { elem_schema, words },
Interned(i64) }` (`vix/src/machine/driver.rs:958-983`). Molten handles are
negative words (`Handle::Molten`), store handles non-negative (`Handle::Store`)
— the two live in the same `i64` word space, so a frame slot doesn't need a
separate tag to tell "still mutable scratch" from "frozen, content-addressed."
`MoltenStore::alloc` always seeds a fresh entry at `refs: 1`
(`driver.rs:986-990`). One `MoltenStore` lives per `Execution` (the per-lane
running frame, `driver.rs:553`) — molten values are execution-local scratch,
never shared across executions or persisted, which is what makes in-place
mutation of them sound to begin with: nothing outside this one execution can
observe the store's internal identity.

## The reuse gate: `refs == 1`

Five host ops mutate a molten aggregate; four of them gate on the refcount
before deciding whether to mutate in place or copy-on-write, all in
`vix/src/machine/driver.rs`, all additionally gated on `!force_molten_copy`
(the oracle switch, below):

| Op (closure) | Lines | Reuse path | COW path |
|---|---|---|---|
| `map_insert` | 2890–3016, gate 2990–2998 | mutate `entry.value`'s `schema`/`pairs` in place, return the same handle | clone `pairs`, push, `alloc` a new `MoltenValue::Map` |
| `array_push` | 4558–4654, gate 4567–4577 | `words.push(value)` in place, same handle; `#[cfg(test)]` bumps `molten_stats.array_push_reused` | clone `words` via `array_entry`, push, `alloc` a new `ArrayWords`; bumps `array_push_copied` |
| `array_pop` | 4655–4707, gate 4660–4669 | `words.pop()` in place, same handle | clone `words`, pop, `alloc` a new `ArrayWords` |
| `array_set` | 4708–4789, gate 4715–4724 | `words[index] = value` in place, same handle | clone `words`, mutate the clone, `alloc` a new `ArrayWords` |
| `record_update` | 4804–4972, gate 4821–4836 | `bytes[field_offset..].copy_from_slice(...)` in place, same handle | clone `bytes` (from the molten entry, an already-`Interned` handle, or a store entry), mutate the clone, `alloc` a new `Record` |

The fifth mutating-adjacent op, `molten_intern` (`driver.rs:4524-4554`), is the
freeze operation — it doesn't gate on `refs`, it always interns via
`intern_molten_word` (`driver.rs:1581`, recursing through
`intern_value_bytes_children` for nested molten children, then
`store.alloc(...)`). This is the demand boundary: crossing it produces a
content-addressed `ContentHash`-identified store handle, at which point molten
identity stops existing for that value.

Each reuse path keeps the *same* molten handle; each COW path allocates a
*new* one via `MoltenStore::alloc`, which reseeds `refs: 1` for the fresh
entry. So reuse vs. copy is entirely a function of the refcount observed at
the moment of mutation — there is no separate "am I unique" flag, `refs == 1`
*is* uniqueness.

## Why in-place mutation is invisible

Molten + `refs == 1` is the exact condition under which mutating in place is
observationally identical to allocating a fresh value with the field changed:
if nothing else holds a reference to this molten entry, nothing can read the
pre-mutation state through some other name, so overwriting it in place has no
side channel. `refs != 1` (something else has aliased the handle — see
`molten_dup` below) forces the COW path precisely because that side channel
now exists: another binding is entitled to see the old value.

This is proven, not asserted: `molten_reuse_is_unobservable_for_aggregate_updates`
(`vix/src/machine/lower.rs:8238`) runs three cases — chained array `push`/`push`/`pop`,
chained map `insert`/`insert`/`get`, and a record spread-update (`Pair { right: 3,
..p }`) — each twice per lane, once with `set_force_molten_copy(false)` (reuse
enabled) and once with `set_force_molten_copy(true)` (forced copy, see below).
It asserts the numeric result, the full `Driver` event trace, and the exported
value bundle are identical between the two runs. Reuse changes *how* the value
gets built, never *what* it is or what got observed while building it.

## `VIX_FORCE_MOLTEN_COPY`: the differential oracle

Read once at `Driver` construction: `force_molten_copy:
std::env::var_os("VIX_FORCE_MOLTEN_COPY").is_some()` (`driver.rs:1822`, field
at `driver.rs:1765`), and exposed to tests directly via `Driver::
set_force_molten_copy` / `Machine::set_force_molten_copy` (`driver.rs:1863-1865`,
`lower.rs:399-401`). When true, it disables all five `refs == 1` fast paths
above, forcing every mutating op down its clone-and-realloc branch regardless
of refcount. Running the same program once normally and once forced-copy and
diffing trace + result + exported bytes is the standing correctness harness for
this whole feature — `cargo_molten_bench` (`vix/src/bin/cdcl_molten_bench.rs:24`)
and `vix/tests/rodin_fixtures.rs:164` use the same switch outside the unit-test
suite, so it also backs the perf ladders in the road-2 evidence doc (doc 9).

## The consuming-move rule

The gate that makes reuse actually fire for the flagship idiom — `let x =
x.push(v)` — lives in `vix/src/machine/lower.rs`. The mechanism has three
parts.

**Detecting the shape.** `consuming_rebind_receiver(binding_name, value)`
(`lower.rs:1962-1966`) delegates to `consuming_update_receiver`
(`lower.rs:1968-1982`), which recognizes a plain-identifier receiver of an
`aggregate_update_method` (`push | pop | set | insert`, `lower.rs:1987-1989`)
or the `.pop().0`-shaped access on such a receiver. `tail_block`'s `let`
handling calls it on every `let` statement (`lower.rs:3044`): `let x =
x.push(v)` tags the new lazy binding for `x` with `consume_receiver: Some("x")`
(`lower.rs:3042-3053`).

**Consuming the tag.** When that lazy binding is later forced,
`resolve_binding`'s `Lazy` arm (`lower.rs:3428-3444`) swaps `self.consume_receiver`
to the tagged name while lowering the RHS. Inside that RHS, `method_receiver`
(`lower.rs:4552-4561`) checks whether the call is an aggregate-update method on
a bare identifier matching the active `consume_receiver`, *and*
`!self.pending_lazy_alias_reads(name)` (below). If both hold, it calls
`resolve_binding_consuming_move(name, None)` (`lower.rs:3451-3477`) instead of
the normal path.

**Skipping `MOLTEN_DUP`.** `resolve_binding` (the normal path) emits a
`molten_dup` for every molten-typed slot read (`lower.rs:3421-3427`,
`3441-3444`, gated by `schema_can_be_molten` — `Array`, `Map<...>`, any
declared struct or enum, `lower.rs:3515-3520`). `molten_dup`
(`lower.rs:5494-5511`) emits `Op::HostCall { host: MOLTEN_DUP_HOST }`
(`MOLTEN_DUP_HOST = 47`, `driver.rs:162`), whose runtime closure
(`driver.rs:4790-4802`) does exactly one thing: `entry.refs =
entry.refs.saturating_add(1)`. It's a refcount bump, not a value copy — it
marks the entry as aliased, which is what forces the next mutating op onto the
COW path. `resolve_binding_consuming_move` instead returns
`self.copy_value(&slot)` — a plain `CopyI64` of the frame *word* (the handle
integer) into a new slot, with **no** `MOLTEN_DUP` host call, so `refs` stays
at 1 and the next `push`/`set`/`insert`/`pop` on that value takes the reuse
path.

**Last-use, concretely.** There's also a lexical last-use check outside the
`let`-rebind shape: `expr_expected_call_arg` (`lower.rs:4204-4222`, used when
lowering a self-tail-call's arguments — see doc 8) only activates
`consume_receiver` for a call argument if `identifier_uses.get(receiver) ==
Some(1)` — the name is used exactly once in that argument list. This is a
syntactic last-use test (count identifier occurrences in the AST fragment
being lowered), not a general dataflow analysis; it's sufficient for the
tail-position-argument case because tail position already implies nothing
after this point can read the old binding.

**Why aliasing doesn't break it.** `pending_lazy_alias_reads(name)`
(`lower.rs:3498-3513`) scans every *other* still-unforced lazy binding whose
captured environment contains `name`, and checks whether that binding's
not-yet-lowered expression textually mentions it. If some other binding (e.g.
`let b = a;` sitting before `let a = a.push(2);`) still needs to read `a`'s
pre-mutation value once it's forced, the consuming-move optimization is
refused and the ordinary `molten_dup` path runs instead — `refs` goes to 2,
the subsequent push takes the COW branch, and `b` still sees the old array.
`molten_consuming_rebind_preserves_pending_aliases`
(`lower.rs:8302`) is exactly this case (`let a = [1]; let b = a; let a =
a.push(2); a.len() * 10 + b.len()`) and asserts `array_push_reused == 0`,
`array_push_copied == 1` — the safety check firing, not the optimization.
Contrast `molten_consuming_rebind_reuses_array_push_receiver`
(`lower.rs:8329`, `let x = [0]; let x = x.push(1); let x = x.push(2); let x =
x.push(3); x.len()`, no aliasing binding), which asserts the opposite:
`array_push_reused == 3`, `array_push_copied == 0` under reuse mode, and the
mirror image under forced copy.

## The consuming-move rule, restated as "tail position ⟹ last use ⟹ consuming move"

The three mechanisms above compose into one rule: a rebind receiver skips
`MOLTEN_DUP` exactly when it's syntactically the last read of that name before
the name is shadowed (the `let`-rebind case) or the call is in tail/argument
position with a single lexical use (the self-tail-call-argument case), *and*
no other pending lazy binding still needs the old value. This is deliberately
conservative — it's a syntactic property of the lowering, not a full liveness
analysis — but it's exactly what the flagship accumulator idiom
(`let trail = trail.push(step)`, `let stack = stack.pop().1`) needs, and it's
what the road-2 CDCL ladder measures moving the named-rebind shape from
copy-amplified to reuse-ceiling performance (doc 9).

## Known gap: no Drop yet

`refs` starts at 1 (`MoltenStore::alloc`, `driver.rs:988`) and is only ever
compared (`== 1`, five sites above) or incremented (`saturating_add(1)`, the
one site in `molten_dup`, `driver.rs:4796`) — there is no decrement anywhere in
the tree. Grep for `.refs` in `driver.rs` returns exactly those six sites: five
reads-as-gate, one increment, zero decrements. Refcounts are monotone: once a
value is aliased (`refs >= 2`), it never becomes unique again for the rest of
that execution, even if every other alias has, in fact, gone out of scope and
will never be read again. A value read twice — even if the second read is
provably the last one — is copy-only from that point on; there is no way back
onto the reuse path.

This means the consuming-move rule is a **soundness-only** correctness
mechanism today: it's conservative about *not* reusing (never wrong, may miss
opportunities), never unsound about reusing when it shouldn't. The planned
unlock is `MOLTEN_DROP` via last-use analysis — decrementing `refs` when a
binding's final read is provably its last, so a value that was aliased and
then genuinely abandoned can drop back to `refs == 1` and re-enter the reuse
path. This is design work, not yet started: no `MOLTEN_DROP` host id, no
decrement path, exists in the current tree. The committee-ratified design
constraint for it (see doc 9) is that the risk to guard against is
aliasing-corruption, not merely missed reuse — a `MOLTEN_DROP` that fires one
read too early is a correctness bug (silent mutation of a value another
binding still expects to read), not just a missed optimization, so whatever
last-use analysis lands here needs the same kind of differential-oracle
guarding (`VIX_FORCE_MOLTEN_COPY`-style corpus-wide check) this doc's existing
mechanism already has.
