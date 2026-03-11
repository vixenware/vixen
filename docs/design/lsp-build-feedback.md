# LSP Build Feedback

This note captures the intended shape of build-driven editor feedback in
`vx-lsp`.

There are two closely related goals:

1. show the last build's evaluation failures in the editor
2. show the last build's timings in the editor

These should not be treated as separate systems. They come from the same build
invocation, the same daemon-owned build state, and the same source spans.

## Goals

1. Surface `build.vx` evaluation failures in LSP as normal editor diagnostics.
2. Surface last-build timing data in LSP in a lightweight, glanceable way.
3. Keep the data model shared between CLI, daemon, and LSP instead of inventing
   an LSP-only format.
4. Make "last build" explicit. The editor should never pretend these are live
   static-analysis diagnostics.
5. Preserve source-backed spans so both failures and timings land on the right
   expressions.

## Product Shape

The editor should show two kinds of build feedback:

1. build diagnostics
   - shown as LSP diagnostics
   - sourced from the most recent build run
   - clearly marked as build/runtime failures, not parser/typechecker failures

2. build timings
   - shown as CodeLens first
   - optionally later as hovers or inlay hints
   - sourced from the most recent successful or failed build run

The user experience should answer:

1. what failed in the last build?
2. what was slow in the last build?

## Current State

Today the pieces are split:

1. `vx-lsp` publishes parser / resolution / capability / type diagnostics from
   the current open document.
2. `vxd` persists `BuildReport` for graph builds under `.vx/runs/`.
3. `build.vx` evaluation failures now have a structured `BuildFailure` model for
   CLI rendering, but that model is not yet persisted as part of "last build"
   editor state.
4. `BuildReport` already contains total and per-node timing data.

So timings already have durable storage for graph builds, while evaluation
failures currently stop at CLI rendering. The missing piece is a daemon query
surface that lets `vx-lsp` ask for the latest normalized build feedback
directly.

## Design Principle

The editor should query `vxd` for the latest build feedback, not scrape terminal
output and not re-run the build inside the language server.

`vxd` should be the source of truth for:

1. last build status
2. last build diagnostics
3. last build timings
4. run metadata such as workspace root, target, profile, and run id

This keeps ownership in the daemon that already knows:

1. when the last build ran
2. which workspace it belongs to
3. whether it succeeded or failed
4. what timings and source spans were produced

## Proposed Data Model

Introduce a shared "last build feedback" model returned by `vxd`.

Example shape:

```text
LastBuildFeedback
  run_id
  workspace_root
  status
  started_at_unix_ms
  ended_at_unix_ms
  diagnostics: Vec<BuildDiagnosticReport>
  timings: Vec<BuildTimingSpan>
```

The important point is not the exact type names. The important point is:

1. failures and timings live together
2. spans are source-backed
3. `vxd` owns and serves the data
4. `vx-lsp` consumes one normalized shape

There are still two storage choices behind that API:

1. keep it in daemon memory and rebuild it from existing run data as needed
2. persist some or all of it on disk as an implementation detail

The architectural point is that storage is not the interface. `vx-lsp` should
not know or care whether `vxd` rebuilt the answer from memory, a run report on
disk, or a dedicated cache file.

## Feedback Contents

The feedback returned by `vxd` should contain:

1. overall build metadata
   - run id
   - success / failure
   - started / ended time
   - profile
   - target triple

2. persisted diagnostics
   - the same structured diagnostic model used by CLI rendering
   - enough source information to publish LSP diagnostics without reparsing any
     terminal output

3. timings
   - source span
   - label
   - duration
   - optional start offset
   - optional category / node kind

4. source identity
   - absolute file path or stable workspace-relative path
   - enough to map snapshot entries to open editor documents

## Evaluation Failures In LSP

### What To Publish

For the last build failure, `vx-lsp` should publish diagnostics derived from the
structured build diagnostics returned by `vxd`.

These diagnostics should:

1. target the actual source span that failed
2. include the primary failure message
3. include runtime facts in `relatedInformation` or in the message text when
   necessary
4. be tagged as build/runtime diagnostics, not static analysis

### Separation From Static Diagnostics

Build diagnostics should not be mixed conceptually with parser/typechecker
diagnostics, even if they share the same LSP transport.

The editor-facing distinction should be:

1. static diagnostics
   - parser
   - resolve
   - capability
   - type

2. build diagnostics
   - "from last build"
   - runtime/evaluation failures
   - potentially stale relative to current unsaved edits

That means the LSP message text should say "last build" somewhere, or the
diagnostic source should be named accordingly.

Example:

```text
source: "vx build"
message: "Last build failed: build input not found"
```

### Staleness Policy

This matters.

The editor buffer may have changed since the last build. So `vx-lsp` needs a
clear policy:

1. if the on-disk file still matches the last-build version, publish normally
2. if the document has unsaved changes, still publish but mark as "from last
   build"
3. if the file no longer exists or spans cannot be mapped, skip rather than
   guessing

The goal is honesty, not fake liveness.

## Timings In LSP

### First Presentation

The first implementation should use CodeLens.

Why:

1. timings are supplementary, not errors
2. CodeLens is visible but non-invasive
3. it avoids cluttering the source with fake diagnostics
4. it maps well to "last build: 37 ms"

Recommended surfaces:

1. top-of-file CodeLens
   - `Last build: 1.46s`

2. per-span CodeLens for important build expressions
   - `build: 37ms`
   - `compile: 220ms`
   - `archive: 90ms`

### What To Time

Not every internal action should become an editor annotation.

The LSP view should show timings only for source-backed spans that help explain
build performance.

Good candidates:

1. `build.vx` call sites and key expressions
2. top-level build helper calls
3. major graph nodes that have a clear source origin

Bad candidates:

1. infrastructure-only nodes with no source span
2. toolchain download internals
3. dozens of tiny near-identical spans

### Aggregation Rule

If multiple internal nodes map to the same source span, LSP should aggregate
them before display.

For example:

1. sum durations for identical span + label pairs
2. keep one CodeLens per visible span
3. prefer a stable human label over raw node ids

## Daemon Query Flow

The flow should be:

1. `vxd` completes a build
2. `vxd` stores or reconstructs the normalized last-build feedback internally
3. `vx-lsp` queries `vxd` for the current last-build feedback for the workspace
4. `vx-lsp` publishes diagnostics and CodeLens from that response

This keeps `vx-lsp` passive:

1. no build execution
2. no build ownership
3. no terminal scraping

## Refresh Strategy

There are two reasonable stages.

### Stage 1

`vx-lsp` queries `vxd` opportunistically:

1. on file open
2. on file save
3. on CodeLens refresh
4. on a periodic lightweight poll if needed

This is simple and good enough to start.

### Stage 2

Move to push-based updates:

1. `vxd` exposes a "last build changed" notification
2. `vx-lsp` subscribes
3. diagnostics and CodeLens refresh immediately after a build finishes

Do not start here. Pull-based queries are simpler and already fit the current
daemon boundary.

## Mapping To LSP

### Diagnostics

Map the returned build reports to:

1. `Diagnostic.range`
2. `Diagnostic.severity`
3. `Diagnostic.source = "vx build"`
4. `Diagnostic.message`
5. optional `relatedInformation` for secondary spans or runtime facts

### CodeLens

Map timing entries to:

1. `CodeLens.range`
2. `CodeLens.command.title = "37ms"` or `"last build: 1.46s"`
3. no command at first, or a simple "open build report" command later

## Deduplication Requirements

Both diagnostics and timings need aggressive deduplication before they reach
LSP.

For diagnostics:

1. collapse identical span + message entries
2. collapse repeated demand-chain frames on the same span
3. prefer one strongest message-bearing span per report cluster

For timings:

1. collapse identical span + label pairs
2. aggregate repeated internal nodes
3. keep the editor surface sparse

## Non-Goals

1. Do not re-run builds from `vx-lsp`.
2. Do not make build diagnostics pretend to be live static analysis.
3. Do not dump raw node-level timing internals directly into the editor.
4. Do not block LSP responsiveness on daemon calls.

## Optional Persistence

Persisting normalized editor-facing feedback can still be useful later:

1. for daemon restart recovery
2. for crash inspection
3. for offline tooling

But if we do it, it should remain an internal optimization or recovery path for
`vxd`, not the primary contract with `vx-lsp`.

## Implementation Plan

1. Define a shared `LastBuildFeedback` model in a shared crate.
2. Add a `vxd` query that returns the latest feedback for a workspace.
3. Populate that feedback for both successful and failed builds.
4. Include structured build diagnostics for `build.vx` evaluation failures.
5. Include normalized source-backed timing entries.
6. Teach `vx-lsp` to query `vxd` for the latest feedback.
7. Publish last-build diagnostics.
8. Add CodeLens for top-level and per-span timings.
9. Add last-build staleness labeling in the LSP messages.

## Product Standard

The finished feature should make the editor answer, at a glance:

1. what failed in my last build?
2. where did it fail in source?
3. what part of the build was slow last time?

It should feel like build-aware editor feedback, not like static analysis trying
to impersonate the runtime.
