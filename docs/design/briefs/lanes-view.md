# Brief: the lanes view (left-to-right timing) in the vixen playground

*Self-contained delegation prompt. Repo: ~/vixenware/vixen, app:
apps/playground (vite+react+TS; `pnpm install` then `pnpm run check` to
typecheck, `pnpm dev --port 4180` to serve). The daemon: build and run
`./target/release/vixd` from ~/oss/facet-cc (`cargo build --release -p
vix-daemon --bin vixd`). Commit early with WHY-messages; never revert; if
stuck, commit and report.*

## The design (decided — implement, don't re-litigate)

A run has three moments, already on the wire (src/generated/daemon.ts):
`Created` (thunk born — a value, nothing owed), `Scheduled` (first demand
touched the run — paying starts), `Finished` (outputs + serving class).
Every event carries `at` (bigint, eval-relative microseconds).

The existing top-to-bottom trace (src/graph.tsx) is the LOG view. This
brief adds the LANES view (a profiler-like left-to-right timeline — NOT a
flamegraph; we want to see the pipeline):

- X axis: time, from 0 to the max `at` across events (label a few ticks in
  ms). Scale to container width; horizontal scroll beyond a minimum scale.
- One lane per command kind for now (cc, ar, rustc… — executor lanes come
  with the fleet backend; leave a comment saying so). Lane label on the
  left gutter.
- One RECTANGLE per run that has a `Scheduled` event: from Scheduled.at to
  Finished.at (fill = serving class, same colors as the trace: ran=--err,
  tier-1=--ok, tier-2=--teal, joined=--violet). Runs scheduled but never
  finished: open-ended hatch/ghost to the end of the timeline. Enforce a
  minimum visual width (~3px) — locally execution is synchronous, so
  rectangles are near-zero-width; the view earns its keep on the fleet,
  but must be correct now.
- `Created` is NOT a rectangle: render it as a small tick (dashed, --info)
  at Created.at in the run's lane — thunk birth is a point of interest,
  not an extent.
- Fn Miss/Hit and Observations do not appear in lanes (v1).
- CLICK a rectangle or tick → same behavior as clicking the run's row in
  the trace: open the Inspector for that run AND select the Created span
  in the editor (reuse the existing onSelect plumbing in App.tsx — pass
  the run's Created event if available, else the clicked event).
- Hover → tooltip (SVG <title>): describe[0] from the Created event,
  serving status, and the Scheduled→Finished duration in ms.

View toggle in the toolbar: `trace | lanes` (segmented control or two
small buttons; trace remains the default). The `all` checkbox only affects
the trace; lanes always show scheduled runs only (that IS the paid-for
filter).

## Files

- NEW src/lanes.tsx — the component. Follow the style of src/graph.tsx
  (pure SVG, CSS classes from src/styles.css variables, no chart library).
- src/App.tsx — view state + toggle; render <Lanes …> or <DemandGraph …>.
- src/styles.css — lane styles; reuse the existing --ok/--err/--teal/
  --violet/--info tokens; no new colors; NO italics anywhere.
- Do not touch src/generated/daemon.ts (generated) or the Rust side.

## Acceptance

- `pnpm run check` clean.
- Manual flow (describe it in your report, screenshots if you have a
  browser tool): run vixd, `pnpm dev`, demand `lua` — the lanes view shows
  a cc lane with 3 finished rectangles + 2 created-only ticks (ar lane: 1
  tick, 1 open-ended scheduled bar — the archive is scheduled by
  projection but never finishes), timeline labels in ms, clicking a
  rectangle opens the inspector on that run and highlights the cc! block
  in the editor.
- The trace view is unchanged and still default.
