# lessons from eval and eval2

Status: planning draft.

This note records what we should preserve from the older evaluators even if
their code stops being the path forward.

The point is to keep lessons and stop carrying mistaken architecture forward by
accident.

## Core rule

Do not preserve old evaluator code just because it exists.

Preserve only:

- semantic lessons
- fixture strategies
- debugging/tracing practices
- useful frontend seams

## What eval2 proved correctly

`eval2` proved important semantic truths:

- Vixen is genuinely lazy and demand-driven
- sharing matters
- keyed/shared runtime nodes are real semantics
- branch-local assumptions and replayable conflicts are real semantics
- parallel execution belongs in the same model, not as a separate bolt-on

These should remain part of `eval3` thinking.

## What eval2 got wrong architecturally

These are the big things we should not carry forward:

- oversized runtime values
- conflating function identity, closure environment, and activation state
- promoting too much local churn into global coordinated runtime state
- execution machinery that is too interpreter-shaped to get out of the way
- backend/codegen concerns arriving before the frontend analysis boundary is
  explicit

## What eval2 taught about workflow

These lessons are worth keeping even if the implementation is replaced.

### Result-only tests are weak

If a lazy system only checks final values, it is easy to accidentally become
eager while still passing tests.

Keep:

- event traces
- structural assertions
- "this did not happen" tests

### Fixtures were useful

Tiny named examples are much better than vague prose.

Keep the discipline of:

- canonical examples
- explicit demand
- explicit expected events / non-events

### Frontend reuse matters

Re-parsing or inventing a second fake source language wastes time and creates
translation bugs.

`eval3` should reuse the actual frontend from the beginning.

## What to preserve from old code

Preserve these ideas, whether or not the old crates survive:

- event/tracing discipline
- fixture style
- frontend-derived source identity
- stable IDs for source-level entities
- lessons about where runtime identity became too large

## What not to preserve from old code

Do not preserve these just because they already exist:

- current evaluator object model
- old bytecode or execution-plan formats
- old runtime admin/scheduler structure
- old function/closure representation
- old "evaluate until done" control shape where analysis should replace it

## Deletion policy

Deleting code is good when it reduces confusion.

But delete by category, not by mood.

### Category 1: live product/runtime dependencies

If the workspace still depends on a crate for current behavior, deleting it is a
product/runtime decision, not just an architecture cleanup.

That should happen only with explicit replacement or retirement.

### Category 2: prototype paths that claim to be the future but are not

These should either be:

- clearly marked prototype-only, or
- deleted once their useful lessons are written down

### Category 3: design notes

These should be kept and improved, because they are the thing that survives
context loss and stops future drift.

## Current recommendation

Right now:

- keep documenting `eval3`
- treat `vx-eval3-core` and `vx-eval3-oracle` as prototype/reference-harness
  code, not the target architecture
- do not extend them as if they were the real compiler path
- do not delete `vx-eval` / `vx-eval2` blindly if the workspace still uses them

The correct next move is to make the new path clear enough that old code can be
retired deliberately instead of emotionally.
