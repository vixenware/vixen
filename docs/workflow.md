# Vixen Development Workflow

## Roles

- **Coordinator**: the Codex session on `main`. Owns issue triage, writes worker prompts, reviews commits, cherry-picks approved work onto `main`, and resets the worker lane between passes.
- **Worker agent**: another agent working in the persistent worker worktree on the `worker` branch.

## Worktree layout

| Path | Branch | Purpose |
|---|---|---|
| `~/bearcove/vixen` | `main` | Integration point. Coordinator works here. |
| `~/bearcove/vixen-worker` | `worker` | Persistent worker lane for implementation tasks. |

Worktrees are **persistent**. Do not remove the worker worktree between passes.

## Dispatch cycle

1. Coordinator makes sure `main` is the current integration tip.
2. Coordinator refreshes the worker lane to match `main` before sending a new prompt:
   ```sh
   cd ~/bearcove/vixen-worker
   git fetch --all --prune
   git checkout worker
   git reset --hard main
   ```
3. Coordinator writes a scoped prompt for the worker agent.
4. Worker agent makes changes in `~/bearcove/vixen-worker` and commits on `worker`.
5. Coordinator reviews the worker commit from `main`.
6. If approved, coordinator cherry-picks the reviewed commit(s) onto `main`:
   ```sh
   cd ~/bearcove/vixen
   git cherry-pick -x <commit>
   ```
7. After the cherry-pick lands, coordinator resets `worker` back to `main` before the next pass.

## Prompt scoping

- Give the worker one coherent task per pass.
- Prompts should define the target files, intended outcome, and required verification.
- If the task is architectural or exploratory, the prompt should say whether the worker is expected to write code, docs, or both.
- If the task depends on coordinator-side docs or issue reshaping, land that on `main` first and only then refresh the worker lane.

## Review rules

- Coordinator reviews commits before any cherry-pick.
- Cherry-pick only reviewed commits.
- If a worker pass is not acceptable, do not cherry-pick partial work. Send a follow-up prompt against the same worker lane or reset it and start over.
- Coordinator git mutations happen sequentially, never in parallel.

## Main branch rules

- `main` is the only integration branch.
- While the worker is active, avoid editing overlapping source files on `main`.
- Coordinator-side docs, issue notes, and workflow files are safe to edit on `main` between worker passes.

## Recovery

If the worker branch stops being a clean source of reviewed commits:

1. Do not merge it directly.
2. Cherry-pick only the reviewed commit(s) that should land.
3. Reset the worker lane back to `main`:
   ```sh
   cd ~/bearcove/vixen-worker
   git checkout -B worker main
   ```
4. Start the next pass from that refreshed state.
