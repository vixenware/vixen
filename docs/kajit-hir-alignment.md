# Vixen and Kajit HIR Alignment

This note records the current alignment between Vixen's planned rule-language
frontend and the Kajit team's intended high-level IR.

It is a companion to [rule-language-design.md](./rule-language-design.md).
That document describes the Vixen-facing language direction. This document
defines the narrower integration question: what IR shape should Vixen target so
that an interpreter can exist now and Kajit lowering can exist later without a
semantic rewrite.

## Current Kajit HIR Direction

Current Kajit HIR assumptions:

- HIR sits above RVSDG.
- HIR is the semantic, source-readable layer.
- RVSDG remains the optimization layer.
- CFG-MIR remains the backend and debug-codegen layer.
- HIR is typed and structured.
- HIR has first-class structs and Rust-shaped enums.
- HIR uses semantic types, not layout-defined types.
- Layout resolves during HIR -> RVSDG lowering.
- HIR keeps structured control flow: `if`, `loop`, `break`, `continue`,
  `match`.
- `match` is first-class in HIR and lowers to RVSDG gamma structure.
- HIR does not expose RVSDG-style state tokens.
- HIR operations carry coarse effects: `pure`, `reads`, `mutates`, `barrier`.
- HIR owns source-facing metadata: names, scopes, spans, comments, statement
  IDs, local IDs.
- HIR is broader than any one frontend. Vixen may use a restricted subset.

The main downstream RVSDG contract is also taking shape:

- RVSDG should generalize by replacing hardcoded cursor/output state with named
  state domains.
- Cursor and output are just the first named domains.
- Domain ops may remain domain ops. Kajit does not need to collapse everything
  into one tiny generic opcode set before work can begin.
- Initial lowering may conservatively map HIR reads onto the same state chain
  as mutates for a given domain.

## Recommended Repo-Level Interpretation

My recommendation for this repo is:

- Keep a separate surface AST for the Vixen parser.
- Add a shared typed HIR that is Kajit-facing, not parser-facing.
- Have Vixen type checking lower into that shared HIR.
- Run the interpreter over shared HIR, not over a frontend-specific runtime AST.
- Lower shared HIR into Kajit later.
- Keep `RulePlan` as the output contract below the interpreter/JIT boundary.

In short:

```text
Vixen source
  -> surface AST
  -> name resolution + type checking
  -> shared HIR
  -> interpreter now
  -> Kajit lowering later
  -> RulePlan
  -> ActionGraph
```

This is the important design choice:

- Vixen should not parse directly into Kajit's HIR.
- Vixen should not keep a separate long-term "typed runtime AST" if Kajit needs
  a different semantic IR.
- The shared HIR should be the semantic contract consumed by both the
  interpreter and Kajit.

That keeps one semantic core in the system instead of two.

## Relation to `rule-language-design.md`

The current rule-language design stays mostly valid:

- interpreter first
- typed IR before execution
- Rust-shaped domain structs and enums
- capability/policy model
- `RulePlan` as the emitted contract
- frontend restrictions such as "no closures in v0"

The main refinement is terminology and layering:

- the "typed IR" described there should now be read as a Kajit-compatible
  shared HIR, or be split into "typed frontend AST" and "shared HIR" if that
  makes implementation clearer
- Vixen's restricted surface must not define HIR's full semantics
- debug and source metadata must live on the shared HIR, not be deferred until
  later lowering stages
- builtin typing and effect information must align with Kajit's coarse
  effect model

If there is a conflict between "what is easiest for the first Vixen parser" and
"what keeps one stable semantic IR for interpreter and Kajit", prefer the
latter.

## Recommended Shared HIR Shape

At minimum, the shared HIR should have this structure:

```text
Function
  - id
  - name
  - params
  - return type
  - locals
  - body: Block
  - metadata

Block
  - id
  - statements: Vec<Stmt>
  - scope_id

Stmt
  - let
  - assign
  - expr statement
  - if
  - loop
  - break
  - continue
  - match
  - return

Expr
  - literal
  - local ref
  - field access
  - enum variant construction
  - struct construction
  - call
  - builtin/intrinsic call
  - unary op
  - binary op
  - index / lookup (if the frontend allows it)
```

Required type families:

- scalars
- structs
- enums
- `List<T>`
- `Map<K, V>`
- `Set<T>`
- `Option<T>`
- frontend/domain nominal types

Explicit non-goal:

- a dynamic `PlannerValue`-style bag may exist as a temporary escape hatch at a
  host boundary, but it must not become the center of gravity for the new HIR

## Metadata the Shared HIR Must Carry

Kajit wants source/debug meaning early. Vixen should align with that from the
start.

Minimum metadata payload:

- statement IDs
- local IDs
- scope IDs
- source spans
- comments where available
- stable names for user-facing diagnostics
- coarse effect info on calls and ops

This is not optional decoration. It is part of the contract.

If the interpreter is built over shared HIR, it should preserve this metadata in
diagnostics and in `RulePlan` provenance.

## Effects and Capabilities

Kajit's current effect model is coarse:

- `pure`
- `reads`
- `mutates`
- `barrier`

Vixen already has a useful policy model:

- `EmitGraph`
- `Transform`
- `Env`
- `Time`
- `Random`
- `Network`
- `Filesystem`
- `Process`

These should coexist, not compete.

Recommended builtin signature model:

```text
BuiltinSig {
  name: BuiltinId,
  params: [...],
  ret: Type,
  effect: EffectKind,
  capability: PlannerCapability,
  state_domain: Option<StateDomain>,
}
```

Interpretation:

- `effect` answers "what kind of operation is this semantically?"
- `capability` answers "is this operation allowed under current policy?"
- `state_domain` answers "what named state chain does Kajit/RVSDG lowering use,
  if any?"

Examples:

- `str.cat(...)`: `pure`, `Transform`, no state domain
- `env.profile()`: `reads`, `Env`, maybe domain `env`
- `emit.node(...)`: `mutates`, `EmitGraph`, domain `ruleplan`
- future file reads: `reads`, `Filesystem`, domain `fs`

This keeps the current policy model useful while aligning with Kajit's broader
effect story.

## Named State Domains

Vixen should not expose RVSDG state tokens in the source language or in shared
HIR.

But shared HIR should be designed so lowering can attach named state domains to
effectful operations.

Working rule:

- HIR expresses effects coarsely and structurally.
- HIR may annotate an op with a named state domain.
- RVSDG lowering decides how that becomes ports, chains, and dependency edges.

This matches the Kajit direction:

- generalize RVSDG state handling
- keep HIR source-readable
- avoid leaking backend machinery upward

## Transition Guidance

### `PlannerValue`

`PlannerValue` should be treated as transitional runtime baggage from the
prototype, not as a model for the new semantic core.

Recommended rule:

- do not let new HIR nodes use `PlannerValue`
- if transition code needs a dynamic escape hatch, isolate it at host/builtin
  boundaries
- do not design new builtins around untyped maps/vectors

### Existing deserializer work

If another frontend is already targeting RVSDG directly, it does not have to
stop immediately just because Vixen is adding shared HIR.

Pragmatic transition stance:

- new Vixen language work should target shared HIR
- existing non-Vixen pipelines may keep targeting RVSDG during transition
- convergence can happen once the shared HIR is stable enough to be worth
  retargeting against

This avoids blocking progress while still steering the architecture in the right
direction.

## What Is Safe To Implement Now

Safe now:

- separate parser surface AST from runtime AST
- keep and extend lexer/parser scaffolding
- define shared HIR data types
- define source/span/comment tables
- define statement/local/scope IDs
- define domain structs and enums
- define builtin signature tables with effect and capability metadata
- define a lowering path from typed frontend constructs into shared HIR

Avoid until HIR is settled:

- interpreter semantics that assume the final HIR shape without documenting it
- Kajit lowering contracts
- committing to one dynamic/static bridging story too early
- frontend decisions that leak RVSDG-specific state machinery upward

## Main Open Questions

- Is the "typed IR" from `rule-language-design.md` simply the shared HIR, or
  should the repo model both a typed frontend AST and a shared HIR explicitly?
- What is the minimum temporary escape hatch, if any, for bridging from current
  `PlannerValue`-based builtins?
- When should non-Vixen Kajit frontends retarget through shared HIR instead of
  lowering straight to RVSDG?
- What is the first concrete builtin signature table, including effects,
  capabilities, and state domains?
- What exact named state-domain model does Kajit want RVSDG lowering to consume?

## Recommended Next Implementation Slice

The first implementation slice should not touch interpreter semantics yet.

It should do only this:

1. Introduce explicit surface AST types for the parser.
2. Introduce shared HIR types with metadata IDs and typed statements/expressions.
3. Introduce a builtin signature table with:
   - parameter types
   - return type
   - coarse effect
   - capability
   - optional state domain
4. Keep current `PlannerRuntime` and `RulePlanHost` untouched.

That gives the repo a concrete semantic spine to build against while leaving the
old runtime available as a reference during transition.
