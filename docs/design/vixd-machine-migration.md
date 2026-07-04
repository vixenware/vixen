# vixd machine migration recon

Purpose: keep the vix evaluator funeral from breaking vixenware consumers when
`vix::oracle` and `vix::engine` are deleted in favor of
`vix::machine::{lower::Machine, driver::DriveEvent}`.

Scope for this pass:

- Editable repo: `/Users/amos/vixenware/vixen`.
- Read-only reference: `/Users/amos/oss/facet-cc` on
  `snark-playground-rebased`.
- No migration was started in this pass.

## Search boundary

Positive source consumers in this repo are the playground generated Vox client
and the TypeScript UI that consumes it. The literal Rust frozen surfaces
`vix::engine`, `vix::oracle`, `vix::graph`, and `vix::exec` have no source
imports in this repo outside docs and the sample `.vix` source text; the only
literal `use vix::...` match in non-doc source is the evaluated sample module at
`apps/playground/src/samples/lua.vix:3`.

There is no `apps/console` tree and no `wtf` tool in this checkout. The local
Vixen daemon service is host introspection only: `crates/vx-services/src/daemon.rs:1`
through `crates/vx-services/src/daemon.rs:4` say orchestration/evaluation belong
elsewhere, and its RPC trait is only `status`/`shutdown` at
`crates/vx-services/src/daemon.rs:28` through `crates/vx-services/src/daemon.rs:32`.
The `vx`/`vixen` CLI is still a husk: `crates/vx/src/cli.rs:4` through
`crates/vx/src/cli.rs:8` and `crates/vx/src/lib.rs:1` through
`crates/vx/src/lib.rs:6` both say the language runtime and daemon are being
rebuilt and no command surface is available yet.

The mac app has a planned consumer of a future run firehose, but not a current
vixd/Vox consumer: `apps/mac/xcode/Vixen/Shell.swift:103` through
`apps/mac/xcode/Vixen/Shell.swift:109` describe build history arriving once the
daemon persists its run firehose, and `apps/mac/xcode/Vixen/Shell.swift:120`
through `apps/mac/xcode/Vixen/Shell.swift:125` describe future capability data
from the daemon status RPC.

## Current producer: vix-daemon in facet-cc

The playground's generated client is produced from the read-only `vix-daemon`
crate, not from Vixen Rust code: `apps/playground/package.json:6` through
`apps/playground/package.json:10` define `pnpm gen` as a `cargo run` of
`/Users/amos/oss/facet-cc`'s `vix-daemon` codegen binary.

That daemon is still an oracle consumer:

| Surface | Current use | Evidence | Machine equivalent |
| --- | --- | --- | --- |
| `vix::oracle::{Event, Oracle}` | Main evaluator and event source. | `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:34` through `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:37` | Partial. `Machine::load`, `reload`, `demand_i64`, `demand_f64`, `trace`, and `fn_hashes` exist, but no oracle-shaped event sink or value enum. |
| `vix::exec::ExecEvent` | Wire `Serving` derives from exec cache class. | `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:69` through `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:90` and `/Users/amos/oss/facet-cc/vix/src/exec.rs:426` through `/Users/amos/oss/facet-cc/vix/src/exec.rs:437` | Partial. `DriveEvent::RunStarted`/`RunCompleted` exist, but they carry hashed `command`/`output` only, not `Ran`/`Tier1Hit`/`Tier2Cutoff`/`Joined`. |
| `vix::support::Span` | Converted to wire `Span` for editor links. | `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:92` through `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:107` | Missing on `DriveEvent`. |
| `vix::oracle::EventSink` | The actor installs a live event sink per eval. | `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:310` through `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:315`, `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:362` through `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:379` | Missing. `Machine::trace()` exposes accumulated trace after/while owned by caller, but no callback/gated sink. |
| `Oracle::load`, `reload`, `with_fetch_backend_arc`, `set_sink`, `call` | Warm actor owns one oracle, reloads it, installs sink, calls entry. | `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:369` through `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:379` | Partial. `Machine::load`/`reload`/`demand_*` exist; fetch backend, sink, and general `Value` result are missing. |
| `Oracle::fn_param_is_target` and `vix::oracle::Value::Struct` | Auto-supplies canned Linux `Target` when the entry expects it. | `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:529` through `/Users/amos/oss/facet-cc/vix-daemon/src/lib.rs:543` | Partial. `Machine::linux_target_handle()` exists, but no public entry-param metadata. |

The current oracle event shape is richer than `DriveEvent`: `Event::Miss` and
`Event::Hit` carry function name, span, caller, and rendered args; `Event::Created`
adds command, run id, span, containing function, argv, and describe; scheduled
and finished events reuse run ids and spans; finished events include exec serving
class and output path/hash pairs; observations carry key/replayed. That surface
is in `/Users/amos/oss/facet-cc/vix/src/oracle.rs:440` through
`/Users/amos/oss/facet-cc/vix/src/oracle.rs:492`.

## Current machine facade

The read-only machine facade currently provides:

| Surface | Evidence |
| --- | --- |
| `Machine::load` and `Machine::load_with_lane` | `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:49` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:55` |
| Warm `Machine::reload` returning `ReloadDiff { changed }` | `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:44` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:47` and `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:229` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:332` |
| Scalar entry demand | `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:334` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:346` |
| Canned Linux target handle | `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:348` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:350` |
| Accumulated trace and trace clearing | `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:352` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:358` |
| Value-store size and tree output rendering | `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:360` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:369` |
| Function hash lookup and hash map | `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:376` through `/Users/amos/oss/facet-cc/vix/src/machine/lower.rs:387` |
| Lane choice, including `Interp` and cfg-gated `Jit` | `/Users/amos/oss/facet-cc/vix/src/machine/driver.rs:66` through `/Users/amos/oss/facet-cc/vix/src/machine/driver.rs:72` |

`DriveEvent` currently provides hashed, driver-level facts:
`Demanded`, `MemoHit`, `Spawned`, `ParkedOn`, `Completed`,
`SpawnedInvocation`, `StoreAlloc`, `RunRequested`, `RunStarted`, and
`RunCompleted`. The enum is in
`/Users/amos/oss/facet-cc/vix/src/machine/driver.rs:107` through
`/Users/amos/oss/facet-cc/vix/src/machine/driver.rs:155`.

## Vixenware consumers

### 1. Generated daemon Vox client

The generated client is the direct TypeScript contract consumed by the UI:

| Consumed type/API | Evidence | Machine equivalent |
| --- | --- | --- |
| `StepMode::{Run, Step}` | `apps/playground/src/generated/daemon.ts:10` through `apps/playground/src/generated/daemon.ts:12` | Adapter-side protocol can keep this, but machine needs per-event gating for true step mode. |
| `EvalRequest { source, entry, mode }` | `apps/playground/src/generated/daemon.ts:14` through `apps/playground/src/generated/daemon.ts:18` | Adapter-side. Source/entry map to `Machine::load`/`reload` and `demand_*`; `mode` needs streaming/gating support. |
| `StepCommand::{Step, Resume}` | `apps/playground/src/generated/daemon.ts:20` through `apps/playground/src/generated/daemon.ts:22` | Adapter-side wire type, facade-side execution hook. |
| `Span { start, end }` | `apps/playground/src/generated/daemon.ts:24` through `apps/playground/src/generated/daemon.ts:27` | Missing on `DriveEvent`. |
| `Serving::{Ran, Tier1Hit, Tier2Cutoff, Joined}` | `apps/playground/src/generated/daemon.ts:29` through `apps/playground/src/generated/daemon.ts:33` | Missing from machine run events. |
| `EvalSummary` counters | `apps/playground/src/generated/daemon.ts:35` through `apps/playground/src/generated/daemon.ts:42` | Adapter-side if events are available; not meaningful from hashed trace alone for created/scheduled/finished parity. |
| `DemandEvent` variants and fields | `apps/playground/src/generated/daemon.ts:44` through `apps/playground/src/generated/daemon.ts:95` | Partial. Machine has hashed demand/run/store trace, but not wire-ready names, spans, args, run ids, serving, outputs, observations, summary, done/failed result strings. |
| `Daemon.eval(req, control, events)` streaming RPC | `apps/playground/src/generated/daemon.ts:162` through `apps/playground/src/generated/daemon.ts:170` and `apps/playground/src/generated/daemon.ts:185` through `apps/playground/src/generated/daemon.ts:200` | Adapter-side Vox shape can remain; facade-side event hook is needed for faithful stream/step. |
| WebSocket client factory | `apps/playground/src/generated/daemon.ts:204` through `apps/playground/src/generated/daemon.ts:217` | Adapter-side. |

### 2. Playground session glue

`apps/playground/src/daemon.ts` does not know about `vix::oracle` or
`vix::engine`; it assumes the generated Vox contract. It opens a control channel
and an event channel, calls `client.eval`, then exposes the event `Rx` and
control `Tx` to React. Evidence: `apps/playground/src/daemon.ts:1` through
`apps/playground/src/daemon.ts:12` and `apps/playground/src/daemon.ts:14`
through `apps/playground/src/daemon.ts:29`.

Machine equivalent: adapter-side for Vox plumbing, facade-side for live event
production and step gating.

### 3. Playground app shell

The app sends `{ source, entry, mode }` to `startEval`, then iterates the event
stream until terminal `Done`/`Failed`. Evidence: `apps/playground/src/App.tsx:40`
through `apps/playground/src/App.tsx:54`, `apps/playground/src/App.tsx:58`
through `apps/playground/src/App.tsx:84`, and the step/resume control calls at
`apps/playground/src/App.tsx:95` through `apps/playground/src/App.tsx:101`.

It also treats spans as byte offsets into the exact evaluated source and maps
them back into the editor: `apps/playground/src/App.tsx:21` through
`apps/playground/src/App.tsx:33`, `apps/playground/src/App.tsx:198` through
`apps/playground/src/App.tsx:213`, and `apps/playground/src/App.tsx:218`
through `apps/playground/src/App.tsx:233`.

Machine equivalent: adapter-side for terminal `Summary`/`Done`/`Failed` once a
result renderer exists; facade-side for spans and streaming/gating.

### 4. Demand graph view

The graph view consumes the full `DemandEvent` union:

| UI dependency | Evidence | Machine equivalent |
| --- | --- | --- |
| `Serving` labels for finished runs | `apps/playground/src/graph.tsx:13` through `apps/playground/src/graph.tsx:24` | Missing from machine run events. |
| Variant-specific row classes and labels | `apps/playground/src/graph.tsx:26` through `apps/playground/src/graph.tsx:47` and `apps/playground/src/graph.tsx:55` through `apps/playground/src/graph.tsx:79` | Partial. Demand/memo/spawn/run concepts exist, but labels need names, argv/describe, outputs, observations, and result state. |
| Timing via `at` | `apps/playground/src/graph.tsx:105` through `apps/playground/src/graph.tsx:114` | Missing from `DriveEvent`; oracle sink had eval-relative timing outside pure event. |
| Source span linking | `apps/playground/src/graph.tsx:116` through `apps/playground/src/graph.tsx:127` and `apps/playground/src/graph.tsx:275` through `apps/playground/src/graph.tsx:300` | Missing from `DriveEvent`. |
| Demand ancestry by `caller`/`in_fn` | `apps/playground/src/graph.tsx:159` through `apps/playground/src/graph.tsx:200` | Missing from `DriveEvent` except recoverable function names from hashes for some events. |
| Exact spawn-to-exec arcs by `run` id | `apps/playground/src/graph.tsx:232` through `apps/playground/src/graph.tsx:241` | Missing. Machine run events use `command`/`output` hashes, not an explicit run id shared with created/scheduled/finished events. |
| Work-only filtering of hits, replayed observations, and unscheduled thunks | `apps/playground/src/graph.tsx:210` through `apps/playground/src/graph.tsx:229` | Partial. Hits exist. Replayed observations and created-but-never-scheduled thunks are not currently exposed by machine trace. |

### 5. Lanes view

The lanes view builds a profiler-style timeline from run ids and timestamps.
It groups `Created`, `Scheduled`, and `Finished` by `run` id at
`apps/playground/src/lanes.tsx:59` through `apps/playground/src/lanes.tsx:67`,
derives lanes from command strings at `apps/playground/src/lanes.tsx:69`
through `apps/playground/src/lanes.tsx:84`, scales all timed events by `at` at
`apps/playground/src/lanes.tsx:86` through `apps/playground/src/lanes.tsx:100`,
draws scheduled-to-finished bars at `apps/playground/src/lanes.tsx:110`
through `apps/playground/src/lanes.tsx:155`, and draws unscheduled created-run
ticks at `apps/playground/src/lanes.tsx:157` through
`apps/playground/src/lanes.tsx:180`.

Machine equivalent: missing for current UI parity. `DriveEvent::RunRequested`,
`RunStarted`, and `RunCompleted` are close in lifecycle but lack run id, span,
command string, serving class, output tree, and timestamps.

### 6. Inspector

The inspector pairs run events by `run`, displays command descriptions, argv,
function args, outputs, and projection notes. Pairing is at
`apps/playground/src/inspector.tsx:9` through
`apps/playground/src/inspector.tsx:14`; event-specific extraction is at
`apps/playground/src/inspector.tsx:23` through
`apps/playground/src/inspector.tsx:61`; args/argv/output rendering is at
`apps/playground/src/inspector.tsx:80` through
`apps/playground/src/inspector.tsx:103`; projection notes depend on whether a
run has `Scheduled`/`Finished` at `apps/playground/src/inspector.tsx:104`
through `apps/playground/src/inspector.tsx:110`.

Machine equivalent: mostly missing. The adapter can render scalar/tree final
values from `demand_*` plus `tree_entries`, but cannot reconstruct command
descriptions, argv, per-run output path/hash pairs, or projection notes from the
current hashed trace.

### 7. Span offset conversion

The frontend assumes demand-event spans are UTF-8 byte offsets and converts them
to/from CodeMirror UTF-16 offsets in `apps/playground/src/offsets.ts:1` through
`apps/playground/src/offsets.ts:4`, `apps/playground/src/offsets.ts:13`
through `apps/playground/src/offsets.ts:23`, and
`apps/playground/src/offsets.ts:25` through `apps/playground/src/offsets.ts:35`.

Machine equivalent: missing until machine events expose byte spans.

### 8. Vix sample source

The playground sample imports vix language prelude types, not Rust APIs:
`apps/playground/src/samples/lua.vix:1` through
`apps/playground/src/samples/lua.vix:4`. It exercises fetch/extract, target
matching, acquire, glob/filter/map/collect, `cc!`, and `ar!` across
`apps/playground/src/samples/lua.vix:6` through
`apps/playground/src/samples/lua.vix:50`.

Machine equivalent: not a Vixenware adapter problem. If this sample remains the
default playground source, full parity depends on the machine lane supporting
the same vix language and exec/fetch surface.

## Gap report

### Adapter-side gaps

These can be handled in `vix-daemon`/Vox adapter code once the machine exposes
enough raw facts. They should not block the machine facade design unless the
required raw fact is absent.

| Gap | Why adapter-side | Required machine input |
| --- | --- | --- |
| Preserve the generated Vox method shape: `Daemon.eval(req, control, events)`. | The UI already speaks this contract via generated TS; the adapter can keep the service and regenerate the client. | A machine-backed evaluator callable from the daemon. |
| Warm actor lifecycle. | The current oracle actor serializes jobs through one warm evaluator; the adapter can own one warm `Machine` and call `reload`. | `Machine::load`/`reload` already exist. |
| Generation numbers, summaries, terminal `Done`/`Failed`. | These are daemon/session concerns. | Streamed events plus a final machine result/error. |
| Hash-to-function-name decoration for basic demand events. | The adapter can invert `Machine::fn_hashes()` for `Demanded`/`MemoHit`/`Spawned`/`Completed`. | Existing `fn_hashes`; still insufficient for caller, args, and spans. |
| Final scalar/tree rendering for narrow demos. | The adapter can turn `demand_i64`/`demand_f64` results into strings and call `tree_entries` for tree handles. | Existing scalar demand and `tree_entries`; general value rendering remains facade-side. |
| Lane selection policy. | The adapter can choose `load_with_lane(source, Lane::Interp)` or a configured lane. | Existing `load_with_lane`; production JIT availability depends on cfg/features. |

### Facade-side gaps queued to the machine lane

These are gaps in the vix machine/facade surface. The Vixenware migration should
not fake them in the adapter from lossy hashes.

| Gap | Required for | Current state |
| --- | --- | --- |
| Span-enriched demand events. | Editor linking in App/DemandGraph and every `Span` field in `DemandEvent`. | `DriveEvent` has no span fields. |
| Function names, caller/in_fn ancestry, and rendered args. | Demand tree layout, inspector args, row labels. | `DriveEvent` has `fn_hash`; adapter can recover names for known hashes, but not caller/in_fn or args. |
| Stable run ids shared by created/scheduled/finished events. | Graph arcs, lanes grouping, inspector pairing. | `DriveEvent::RunRequested/RunStarted/RunCompleted` carry hashed command/output, not a run id. |
| Command string, argv, describe levels, and command span. | Graph labels, inspector, clickable command source. | Not exposed by `DriveEvent`; oracle `Event::Created` had them. |
| Exec serving class. | `Serving::{Ran,Tier1Hit,Tier2Cutoff,Joined}` UI coloring and labels. | Machine run completion does not expose the `ExecEvent` class. |
| Output tree per finished run. | Inspector output path/hash list and final build artifact visibility. | Machine can render a final tree handle via `tree_entries`, but run events do not carry output path/hash pairs. |
| Observation events and replay state. | Graph filtering and cold/replayed observation glyphs. | No `DriveEvent` observation variant. |
| Eval-relative timestamps. | Graph `+ms` labels and lanes scaling. | `DriveEvent` is timeless. Current oracle sink timestamp is outside the pure event. |
| Live event hook with gating. | `StepMode::Step`, `StepCommand::{Step,Resume}`, and streaming UI behavior. | `Machine::trace()` is an accumulated slice; there is no event callback or incremental driver step API. |
| Entry parameter metadata. | Auto-injecting Linux `Target` when the entry expects a `Target`. | `Machine::linux_target_handle()` exists, but entry parameter inspection is not public. |
| General value renderer/schema-aware result. | `Done.result` parity with oracle `Debug` values beyond scalar/tree handles. | `demand_i64`, `demand_f64`, and `tree_entries` are narrow; no public value enum/renderer. |
| Fetch/backend injection and full lua sample lowering. | Default playground source and vixd parity with current `Oracle::with_fetch_backend_arc`. | Not exposed through `Machine`; the current machine parity ledger still says full `lua.vix` lowering needs fetch/extract/glob/filter, arrays, path/tree joins, `ar!`, multi-input `cc!`, exec observations/journal, and projection-preserving run accounting at `/Users/amos/oss/facet-cc/vix/src/machine/PARITY.md:48`, and engine/oracle parity for `lua.vix` waits on the same surface at `/Users/amos/oss/facet-cc/vix/src/machine/PARITY.md:62`. |

## Migration shape once facade gaps land

1. Keep the Vox service boundary and generated TS client stable where possible.
   The playground already isolates daemon contact in `apps/playground/src/daemon.ts`.
2. Replace the oracle actor with a machine actor that owns a warm `Machine`,
   calls `reload`, and uses a machine event sink to emit the existing
   `DemandEvent` wire shape.
3. Build a small event adapter in `vix-daemon`:
   machine event -> wire `DemandEvent`, using function hash/name tables and
   machine-provided spans/run records/value renderers.
4. Regenerate `apps/playground/src/generated/daemon.ts` from the updated
   `vix-daemon`; do not hand-edit the generated file.
5. Keep the first migration test as a production-path Vox/playground slice:
   one eval request, streamed events, terminal result, and run-id/span
   invariants. Unit tests against handcrafted event arrays would not protect the
   funeral boundary.

## Funeral gate conclusion

The current Vixenware repo does not import `vix::oracle`, `vix::engine`,
`vix::graph`, or `vix::exec` directly. The breakage risk is the generated
`vix-daemon` wire contract consumed by the playground and, later, the app's run
firehose surfaces.

`Machine` already covers warm load/reload, scalar demand, function hashes,
trace access, lane choice, a Linux target handle, and final tree-entry rendering.
It does not yet cover the UI contract that made the oracle useful to vixd:
span-enriched demand events, run-id correlation, command descriptions, serving
class, observations, timestamps, live step gating, entry param metadata, and
general value rendering.

Therefore the migration should wait for those facade-side machine gaps or an
explicit machine-lane event adapter surface. Vixenware-side work should be
limited to the daemon adapter and generated Vox client once that surface exists.

## Adjudication (accepted 2026-07-04)

Recon accepted. Consequences banked:
1. The facade-gap table above IS the Innards-mode trace spec,
   concretized — folded into the machine lane's B4 brief (exec depth
   carries run ids/serving/observations/command metadata; sink+step
   gating and span/name enrichment ride the same slice).
2. THE FUNERAL GATE GROWS ONE CLAUSE: parity ledger 100% AND
   vix-daemon migrated to the Machine facade (playground client
   regenerated; production-path vox slice test as specified above).
   A funeral that strands vix-daemon on the oracle is forbidden.
3. Migration itself waits for the facade gaps; vixenware-side work
   stays limited to the daemon adapter + regeneration.
