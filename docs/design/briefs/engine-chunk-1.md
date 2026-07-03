# Brief: engine chunk 1 — the demand-driven evaluator, non-exec surface

*Self-contained delegation prompt. Repo: ~/oss/facet-cc, branch
snark-playground-rebased (shared checkout — other agents' unrelated changes
are normal, leave them alone). Verify: `cargo nextest run -p vix`;
`cargo clippy -p vix --all-targets` warning-free (never suppress). Commit
as you go with WHY-messages; never revert; if blocked, commit and report
the negative result. No serde, no println (tracing only), and vix must
stay WASM-CLEAN: no tokio, no threads, no Instant in the new module.*

## Normative design — read it FIRST

/Users/amos/vixenware/vixen/docs/design/engine-demand-semantics.md

That document is the constitution. This brief implements its §1–§3 for the
NON-EXEC language surface only (no command blocks, no PendingTree, no
fetch/acquire — that is chunk 2). Where the constitution and this brief
seem to disagree, the constitution wins; report the discrepancy.

## What exists that you build on

- vix/src/oracle.rs — the eager reference evaluator. You are writing its
  lazy twin. Read `call_fn`, `block`, `eval`, `call_value`, `pattern`,
  and the memo key construction (`fn_hashes[func]` × args `hash_into`).
  Mirror SEMANTICS exactly; do not mirror eagerness.
- vix/src/graph.rs — the toy substrate (i64). Do not modify it; it stays
  as the invariant spec. Your module is new: vix/src/engine.rs.
- Samples: playgrounds/snark/src/bundled/vix/samples/{eval.vix,types.vix}.
  The oracle tests (vix/tests/oracle.rs) show which entry points each
  sample exposes and what they return.

## Step 1 — shared module loading (refactor, zero behavior change)

`Oracle::load`/`reload` build fns/fn_hashes/enums/structs via the private
`load_module_tables` + `ModuleTables` (plus `EnumInfo`, `StructInfo`,
`VariantShape`). Move these into a new `pub(crate) mod module` (file
vix/src/module.rs) so oracle and engine share them. Pure move: all
existing tests stay green after this step alone — commit it separately.

## Step 2 — vix/src/engine.rs

Public surface (keep it this small):

```rust
pub struct Engine { /* module tables, arena, memo, events */ }
impl Engine {
    pub fn load(source: &str) -> Result<Engine, String>;
    /// The EDGE: demand enters here; result is deep (chunk 1 has no
    /// pending trees, so "deep" is trivial — keep the call site anyway).
    pub fn call(&mut self, func: &str, args: &[(&str, Value)]) -> Result<Value, String>;
    pub fn events(&self) -> Vec<Event>;
}
```

Reuse `vix::oracle::Value` and `vix::oracle::Event` (make items pub(crate)
where needed rather than duplicating types).

Internals per the constitution:

- Arena: `Vec<NodeState>`, `NodeId = usize`. Node states: `Thunk { expr,
  env }`, `Forced(Value)`, `InProgress` (cycle detection: demanding an
  InProgress node is an error "demand cycle", with the fn name if known).
  Expressions: store `Rc<Expr>` clones or an owned expr — pick the
  simplest thing that borrows cleanly; nodes must be PLAIN DATA (no
  closures — constitution §6, load-bearing for the later weavy lowering).
- `Env`: persistent map name → NodeId (`Rc`-linked frames or im-style;
  simplest correct wins). A `let` allocates a Thunk node — it does NOT
  evaluate. Using a binding twice demands the same NodeId once (assert
  via test: shared binding's fn Miss appears ONCE).
- `Demand` enum with `Identity` and `Path(String)` — chunk 1 only ever
  passes Identity, but the signature `fn demand(&mut self, node: NodeId,
  kind: Demand) -> Result<Value, String>` must exist as specified, so
  chunk 2 doesn't refactor.
- Strictness EXACTLY per constitution §3: scrutinees Identity-only, arms
  lazy (a match arm that would error must not error when unselected —
  test it); operators demand operands; fn application creates a Call
  boundary: demand each arg Identity, compute memo key EXACTLY like the
  oracle (same fn_hashes, same `hash_into`), consult the engine's own
  memo, emit Hit (memoized) or Miss (expand body thunk with params bound
  to the ARG NODES, not forced values — binding nodes preserves sharing).
  v1 choice (record in a comment): struct/enum construction is strict in
  field values (no trees in chunk 1); partial application mirrors the
  oracle's `Value::Partial` behavior.
- Events: emit `Event::Miss`/`Event::Hit` with func, span, args (same
  short-rendering as the oracle — reuse `Value::short`). `caller`: track
  the currently-expanding Call's fn name with a plain stack field
  (mirroring the oracle's fn_stack). No sink in chunk 1; events collect
  in a Vec.

## Step 3 — the differential harness: vix/tests/engine.rs

- `engine_matches_oracle_on_eval_vix` and `…_on_types_vix`: for each entry
  point the oracle tests exercise on that sample, run BOTH evaluators with
  the same args and assert value equality. Also assert: the engine's Miss
  func-name MULTISET ⊆ the oracle's (lazier, never eagerer).
- `unused_binding_is_never_demanded`: an inline source where fn main
  let-binds a call to fn expensive() but returns a constant — oracle
  Misses `expensive`, engine must NOT (assert both directions).
- `shared_binding_computes_once`: let x = f(…); use x twice; engine Miss
  multiset contains f exactly once; second use is NOT a Hit (same node,
  not a memo lookup — assert no Hit for f either).
- `unselected_match_arm_never_evaluates`: a match whose unselected arm
  calls a fn that would error (e.g. divides by zero) — engine returns the
  selected arm's value cleanly. (Check the oracle's behavior on the same
  program and note it in a comment: if the oracle ALSO doesn't evaluate
  unselected arms, this is parity; if it does, this is an allowed
  engine-lazier divergence — report which.)
- `memo_hits_across_calls`: call the same fn with equal args from two
  places; exactly one Miss, at least one Hit.

## Acceptance

- All existing vix tests green (`cargo nextest run -p vix`) — the module
  refactor must not disturb the oracle.
- All new engine tests green.
- clippy warning-free on `-p vix --all-targets`.
- vix still compiles for wasm: `cargo build -p snark-wasm --target
  wasm32-unknown-unknown --release` succeeds.

## Report back

Commit hashes; test summary; the oracle-parity note from the match-arm
test; and: list every place where you found the oracle's semantics
AMBIGUOUS while mirroring it (things the constitution doesn't pin and the
oracle code decides implicitly). Do not resolve ambiguities creatively —
mirror the oracle and report them.
