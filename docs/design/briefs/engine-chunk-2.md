# Brief: engine chunk 2 — the exec seam; lua.vix through the engine

*Self-contained delegation prompt. Repo: ~/oss/facet-cc, branch
snark-playground-rebased (freshly rebased onto main — pull first). Verify:
`cargo nextest run -p vix`; clippy warning-free on `-p vix --all-targets`;
`cargo build -p snark-wasm --target wasm32-unknown-unknown --release` must
still pass (vix stays wasm-clean: no tokio/threads/Instant in engine).
Commit as you go with WHY-messages; never revert; report negative results.*

## Normative design — read BOTH first

1. /Users/amos/vixenware/vixen/docs/design/engine-demand-semantics.md
   (the constitution, INCLUDING its Amendments section A1–A3)
2. vix/src/engine.rs as it exists (chunk 1: non-exec surface, arena,
   Demand::{Identity,Path} signature) and vix/src/oracle.rs (`command_block`,
   `force_and_note`, `note_scheduled`, `observe`, the PendingTree binop
   projection branch, `deep_force_value`).

## Scope

Bring the exec seam into the engine so lua.vix (and types.vix's toolchain
entry) evaluate lazily end-to-end, and extend the differential harness to
the FULL equivalence contract (constitution §5).

## Design points (decided)

1. **Command blocks are Thunks.** `let x = cc! { … }` allocates a Thunk;
   demanding it (any kind) evaluates splices (Identity, jointly — mounts
   are whole-tree uses), assigns roles, dispatches through the engine's OWN
   `crate::exec::ExecCache`, registers the run in the GLOBAL runs registry
   (same `runs::register`/`LocalRun` as the oracle — expose oracle items
   pub(crate) rather than duplicating: `splice_into`, `assign_roles`,
   `tool_for`, `subtree`, `LocalRun`, the runs module), and emits
   `Event::Created` THEN. This is the engine-lazier divergence: an
   undemanded cmd! never registers a run, never logs Created.
2. **Projection**: `pending / p"x"` becomes a Project node. Demanding it
   emits the engine's Scheduled (first-touch latch per run, mirror
   `note_scheduled`), calls `demand_path` on the live run, falls back to
   flush-and-subtree exactly like the oracle's branch. Identity demand on
   a PendingTree (hash/cmp/splice/memo-arg/edge) flushes: Scheduled latch,
   `force_run`, `Event::Finished` with outputs — mirror `force_and_note`.
3. **Observations**: the engine gets its own journal (BTreeMap) and
   `observe` mirroring the oracle's (fetch/extract/acquire/toolchain —
   whatever eval.vix/types.vix/lua.vix touch). Same keys, same
   Observation events. Toolchain/acquire entries from types.vix that
   chunk 1 skipped are now IN scope for the harness.
4. **The edge deep-forces with logging** — engine analog of
   `deep_force_value` (every PendingTree in the result → Scheduled latch +
   Finished).
5. **A3 lockstep fix (constitution amendment): duplicate named-argument
   bindings are an ERROR** at the call boundary in BOTH oracle and engine
   (the oracle currently lets memo-hash and body-lookup diverge). Add the
   check to both + one test each proving the error fires. Existing tests
   must stay green (none exercise duplicates).

## Harness extension (vix/tests/engine.rs)

For lua.vix (entry `lua`, canned Target like vix/tests/compose.rs) and the
types.vix toolchain entry, run BOTH evaluators and assert:

- Values EXACTLY equal (deep-forced structural equality).
- `Finished` multiset of (command, ExecEvent-class) EQUAL.
- `Scheduled` run-count EQUAL on this corpus (5 for lua.vix — every run is
  demanded: three by identity, two by projection).
- `Created`: engine set ⊆ oracle set; on lua.vix additionally EQUAL (all
  five thunks are demanded).
- Observation key sets EQUAL.
- Engine `Miss` func-multiset ⊆ oracle's.
- NEW laziness proof: an inline program where a cmd! block is let-bound
  but never used — oracle logs Created (eager body evaluation), engine
  logs NOTHING for it (no Created, no run registered); values still equal.
  This is the constitution §3.5 divergence made into a test.

Warm-engine sanity (cheap, valuable): call `lua` twice on one Engine; the
second call's events are exactly one Hit (mirrors the oracle's warm test).

## Acceptance

All existing vix tests green; new harness tests green; clippy clean;
wasm build green. Do NOT touch vix-daemon/vix-wire in this chunk.

## Report

Commit hashes; test summary; any place the oracle's exec-seam semantics
were ambiguous while mirroring (report, don't resolve); and whether the
Scheduled/Finished latching needed any state the constitution's node
vocabulary doesn't name (that answer feeds the weavy lowering).
