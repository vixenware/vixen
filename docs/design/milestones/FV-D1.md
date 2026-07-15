# FV-D1 design packet: observation is not fetch

Status: committee candidate, 2026-07-15.

Controlling roadmap: [Vix full-vision milestones](../vix-full-vision-milestones.md),
milestone `FV-D1` only. Normative runtime authority lives in the Facet Vix
machine specification at `vix/docs/content/spec/machine/_index.md`, especially
`primitive.md`, `receipt.md`, `persistence.md`, and `identity.md`.

## Assignment and exact bases

- **Facet home:** create `vix/full-vision-d1` from the remotely verified FV-00
  tip `8db42feb6ebfb8ed921bba41f9d3b81f43d980e8` only after this packet's
  committee approval.
- **Vixen documentation:** `rodin-workspace-scope`, packet parent
  `85e4eda40fc288070f9e626c42baf5ae477344cc`. Vixen receives design and
  evidence records only; no Vix semantics or fetch implementation moves here.
- **Stop condition:** close the fetch/observation identity law, its registered
  primitive substrate, persistence/replay, legacy migration, and the gate in
  this packet. Do not begin `FV-D2` registry modeling, real sparse-path lookup,
  or Rodin row demand.

No implementation branch has been created and no Facet code has been changed
for FV-D1 at the time of committee review.

## Outcome

There is one acquisition law:

- `fetch` is `Pinned`. Its request carries the final semantic `ValueId` of a
  `Blob` before execution. URL and upstream SHA-256 are provenance/integrity
  coordinates, never identity.
- `observe` is `Observed`. Its result identity becomes known only through an
  explicitly refreshed, receipted observation claim.
- both are registered typed primitives using the normal scheduler, Store,
  memo, receipt, and persistent journal authority;
- a pinned fetch resolves local Store, configured value providers, then origin;
  every body is self-verified against the requested `ValueId` before admission;
- no production API or Vix call can select between pinned and observed behavior
  by omitting a hash;
- fetch returns `Blob`; extraction is a separate demand returning `Tree`.

## Current-source audit

### Two compiler/runtime paths exist

Facet `vix/src/lib.rs:3-6` names `surface → vir → lowering → runtime` as the
authoritative compiler path. The crate-root parser and `vix::machine` remain
migration support for existing consumers. FV-D1 must advance the authoritative
runtime and make the migration surface obey the same identity law; it must not
promote the legacy driver into a second authority.

### The legacy fetch seam has the exact rejected shape

- `vix/src/fetch.rs:13-15` defines
  `FetchBackend::fetch(url, Option<expected_sha256>)` and returns a `Tree` plus
  actual SHA-256.
- `vix/src/machine/lower.rs:7392-7438` accepts `fetch(url: ...)`, optionally
  accepts `sha256`, encodes absence as `-1`, and declares the result `Tree`.
- `vix/src/machine/driver.rs:2534` owns a private
  `BTreeMap<String, i64>` journal. `prepare_fetch_request` at `:7915` chooses
  `fetch:<url>:sha256:<digest>` or `fetch:<url>:observed` based on optional
  input. `finish_fetch_run` at `:7956` stores an observed SHA-256 in that table
  and allocates the backend-provided `Tree`.
- the same private table also records legacy capability acquisition. It is not
  the typed, durable observation-claim authority required by
  `machine.receipt.journal`.
- `vix-fetch/src/lib.rs:39-91` repeats the optional-SHA contract, performs HTTP,
  and couples transfer to gzip/tar extraction.
- `vix-fetch/tests/fetch.rs:83-96` and `:99-143` silently return success when
  crates.io is unreachable. Those tests are useful smoke probes, not a
  production certificate.

### Production/demo call sites still choose the wrong law

- `vix/corpus-next/cargo.vix:184` performs a SHA-only archive fetch.
- `vix/corpus-next/index.vix:95` performs a URL-only fetch and expects `Tree`.
- Taxon, facet-core, Lua, daemon, wire, and wasm tests inject
  `FakeFetchBackend`, whose authored archive bytes and returned tree are
  independent fields. A caller can therefore receive a tree whose identity is
  unrelated to the downloaded bytes.

### The authoritative runtime has useful partial substrate

- `vix/src/runtime/identity.rs` owns the closed framed BLAKE3 writer and typed
  `ValueId { schema, content }` identity.
- `vix/src/runtime/store.rs` stores by `ValueId`, self-verifies journal bodies,
  and persists values separately from memo claims.
- `vix/src/vir.rs:1151-1161` models `RegistryUrl → PinnedUrl` and `Fetch → Blob`.
- `vix/src/runtime/scheduler.rs:2344-2377` verifies fixture bytes against the
  pinned Vix content digest, records a read witness, and memoizes through the
  scheduler. Rungs 075-077 prove fixture pinning, same-run dedup, and separate
  extraction.

That path is not yet FV-D1:

- `Op::Fetch` is a scheduler match arm, and the origin is only
  `FixtureStore::fetch_url`;
- no `observe` primitive or observation claim exists;
- no `PrimitiveId`, typed primitive descriptor, object-safe primitive trait,
  `EffectCtx`, or `EffectTicket` exists anywhere in `vix/src`;
- `PersistentRuntimeJournal` has Store values and memo claims, but no typed
  observation claims or explicit refresh generation;
- the authoritative and migration stores use different identity code. A pin
  must use `runtime::ValueId`; a new raw BLAKE3/SHA/legacy
  `machine::ContentHash` namespace is forbidden.

### Measured specification gap

The current Dodeca report is evidence, not a completion claim:

```text
ddc coverage status --source vix .
Referenced 52/232 (22.4%), Implemented 50/232 (21.6%), Verified 28/232 (12.1%)
```

The following governing rules currently report no implementation and no
verification references:

- `machine.primitive.trait`;
- `machine.primitive.fetch-is-an-invocation`;
- `machine.primitive.fetch-is-pinned`;
- `machine.receipt.journal`;
- `machine.persistence.value-vs-claim`.

This matters because `vix/tests/ratchet/FOUNDATION.md` requires the primitive
trait, `EffectCtx`, journal, persistence, and receipt discipline past rung 082.
Green post-082 behavior without those structures is not sufficient foundation
evidence.

### Unrelated roadmap drift found during the audit

The integrated source now contains `Op::MiniSolve`, although the roadmap's
FV-D3 text says there is no such operation. FV-D1 will route any authoritative
external primitive through the generic registry without changing solver
semantics, but it will not delete, redesign, or bless `MiniSolve`. The roadmap
and D3 packet must reconcile this current-source fact before FV-D3 begins.

## Governing invariants

The implementation must carry and verify the corresponding Dodeca rules:

- `machine.identity.blake3`, `machine.identity.value-identity-pair`,
  `machine.identity.single-module`, and `machine.identity.framed-encoding`;
- `machine.cache.three-kinds`, `machine.cache.no-private-caches`, and
  `machine.cache.effects-are-memo-entries`;
- `machine.primitive.trait`, `machine.primitive.registered`,
  `machine.primitive.requests-are-values`,
  `machine.primitive.memo-policy`, and
  `machine.primitive.effectctx-witness-only`;
- `machine.primitive.fetch-is-an-invocation`,
  `machine.primitive.fetch-is-pinned`,
  `machine.primitive.fetch-returns-a-blob`, and
  `machine.primitive.fetch-integrity-vs-identity`;
- `machine.receipt.journal` and the renamed observation-pin rule at
  `machine.receipt.fetch-observation-pin`;
- `machine.persistence.trait-boundary`,
  `machine.persistence.value-vs-claim`,
  `machine.persistence.reverify-on-load`, and
  `machine.persistence.value-and-claim-interfaces`.

False negatives are permitted. A body or claim that cannot be verified is a
miss/unavailable result. A mismatched body, stale claim accepted without its
policy, URL-keyed value, or SHA-named value is a forbidden false positive.

## Proposed types and authority

All protocol/persistence types derive `Facet`; serialization uses
`facet-json`. No serde or hand-written JSON is introduced.

### Registered primitive boundary

Add the authoritative runtime types in a dedicated primitive module:

```text
PrimitiveId {
    namespace: String,
    name: String,
    version: u32,
}

PrimitiveMemoPolicy = Hermetic | Pinned | Observed | Volatile

PrimitiveDescriptor {
    id: PrimitiveId,
    request_schema: SchemaId,
    response_schema: SchemaId,
    memo_policy: PrimitiveMemoPolicy,
    protocol_version: u32,
    capability_schemas: [SchemaId],
}

Primitive::begin(request: ValueId, ctx: EffectCtx) -> EffectTicket
```

`Primitive` is object-safe and registered at runtime construction. Lowering
emits one generic primitive invocation carrying descriptor identity and a
typed request value. The scheduler dispatches by `PrimitiveId`; it has no
fetch/observe-specific result cache, receipt branch, channel table, or string
key.

`EffectCtx` is the primitive's only machine window. It exposes witnessed typed
value reads, verified value resolution/publication, typed receipt facts,
progress events, and completion. It exposes no raw Store, memo, scheduler,
filesystem, network client, or process-local capability handle.

`begin` never blocks the scheduler. `EffectTicket` is either completed or owns
a completion registration that wakes the parked task. There is no scheduler
poll loop and no blocking network read on the scheduler thread. Task discard
drops the ticket; a late completion cannot publish into a discarded demand.

The first migration covers every external primitive reachable from the
authoritative runtime so the generic path is an authority, not a fetch-only
exception: exec, typed document decode, pinned fetch, observe, archive
extraction, and the existing fixture adapters. Existing pure effect-island
construction remains ordinary VIR evaluation. `MiniSolve` may be wrapped as a
temporary registered compatibility primitive, but its semantics and later
retirement belong to FV-D3.

### Pinned fetch values

```text
UpstreamDigest = Sha256([u8; 32])

BlobId(ValueId)

OriginHint {
    capability: ValueId,
    coordinate: String,
}

FetchPin {
    value: BlobId,
    origins: [OriginHint],
    upstream: Option<UpstreamDigest>,
}

PinnedFetchRequest {
    pin: FetchPin,
}

PinnedFetchReceipt {
    request: ValueId,
    result: ValueId,
    serving: LocalStore | ValueProvider | Origin,
    origin: Option<OriginHint>,
    upstream_verified: Option<UpstreamDigest>,
}
```

`BlobId` is a typed wrapper around the canonical framed `ValueId` of a `Blob`,
constructed only with the canonical Blob schema through the single runtime
identity writer. The Vix surface accepts the required BLAKE3 `ContentHash` and
the typed constructor supplies that known response schema; callers never author
a raw `SchemaId`. A bare 32-byte digest without the known Blob schema is not a
semantic pin. The URL and SHA-256 do not enter the result identity.

### Observation values and refresh

```text
ObservationKey {
    primitive: PrimitiveId,
    capability: ValueId,
    request: ValueId,
}

RefreshGeneration(ValueId)

RefreshDirective = ReusePinned | Refresh(RefreshGeneration)

ObservationRequest {
    key: ObservationKey,
    refresh: RefreshDirective,
}

ObservationClaim {
    key: ObservationKey,
    generation: Option<RefreshGeneration>,
    result: ValueId,
    receipt: Receipt,
    prior_result: Option<ValueId>,
}

ObservedBlob {
    value: Blob,
    fetch_pin: FetchPin,
}
```

The capability and typed request decide what is being observed; core Vix does
not expose an untyped `observe(url)` escape hatch. FV-D1 supplies an HTTP-blob
test/adapter capability. FV-D2 will define the crates.io `Registry` capability
and typed row constructors on top of the same primitive.

`ReusePinned` consults the durable observation claim. A previously unseen
`Refresh(generation)` is an explicit new input and performs one new observation
even when an older pin exists. An exact re-demand of the same generation follows
ordinary memo/claim reuse; refresh is not an ambient bypass around memoization.
The old claim remains immutable. Every refresh claim records the prior result.
If bytes change, the runtime also emits a typed
`ObservationChanged { prior, current }` event; the change is never hidden behind
an unchanged demand key. Equal refresh bytes produce the same `ValueId` and a
new refresh receipt without duplicating the Store value.

The observed response provides both the resident `Blob` and its derived
`FetchPin`. Lock/update code may persist the pin; later locked builds use only
`fetch(pin)` and do not re-observe.

### Typed failures

Fetch and observation failures are typed language failures, not
`Result<_, String>` contracts:

- unavailable value after all admissible providers/origins;
- origin transport/protocol failure;
- requested Vix identity versus observed body mismatch;
- upstream-digest mismatch;
- missing or inadmissible observation capability;
- observation-claim authority/policy rejection;
- corrupt persisted value or claim;
- refresh-generation conflict.

Diagnostic source/demand context remains reconstructed at report time and does
not enter failure identity.

## Execution paths

### Pinned fetch

1. Lower `fetch(pin)` to a generic primitive invocation whose demand preimage
   contains the pinned-fetch `PrimitiveId` and request `ValueId`.
2. Validate that `pin.value.schema` is the canonical Blob schema.
3. Ask the runtime Store for that exact `ValueId`. A resident hit completes
   without calling any provider or origin.
4. Ask configured open `ValuePersistence` providers in policy order. Hash each
   returned body through the canonical framed writer and reject any mismatch
   before Store admission.
5. Only then try admissible origin hints through their capability adapters.
   Verify the mandatory Vix `ValueId`, then intern the Blob and offer it to
   configured value persistence.
6. Publish an ordinary memo result and typed receipt. Re-demand follows normal
   exact memo/Store behavior; there is no fetch table.

If `FetchPin.upstream` is present, verify that digest against the candidate Blob
regardless of whether it came from the resident Store, a provider, or an origin.
It remains provenance rather than identity, but a contradictory pin cannot be
accepted merely because another identity check passed.

The open persistence interface is semantic:

```text
ValuePersistence::get(ValueId) -> Option<ValueBodyCandidate>
ValuePersistence::put(ValueId, ValueBody)
ClaimPersistence::get_observation(ObservationKey, refresh) -> Option<claim>
ClaimPersistence::put_observation(claim)
```

`ValueBodyCandidate` is deliberately not trusted by its name or provider.
Open Vix verifies every returned body and claim. FV-D1's concrete provider path
must support canonical Blob bodies; unsupported realized schemas are permitted
false-negative misses until their canonical persistence encoding exists.
Product tenancy, placement, replication, and authorization remain outside this
milestone.

### Observation

1. Lower the capability-specific observation constructor to a generic
   `Observed` primitive request. Capability identity and refresh directive are
   ordinary request-value fields.
2. For `ReusePinned`, load the observation claim from the normal journal/claim
   interface. Verify its authority metadata, request/key equality, receipt
   framing, result body identity, and policy. Any uncertainty is a miss.
3. If no acceptable claim exists, or refresh is explicit, begin the registered
   observation adapter and park the task.
4. Completion publishes the typed result, Store value, receipt, and immutable
   observation claim together. A crash before completion publishes none of
   them.
5. Serialize Store values separately from observation/memo claims in
   `PersistentRuntimeJournal`. On load, bodies self-verify and claims undergo
   their distinct policy/reverification path.

Observation pins remain available when the event sink is disabled. The
transient event log is observability, never authority.

## Legacy and call-site migration

The legacy path remains migration-only and cannot preserve a competing law:

- remove `FetchBackend::fetch(url, Option<sha256>)`;
- replace `FetchOutput { tree, actual_sha256 }` with raw Blob transfer and
  typed provenance receipts;
- delete the fetch-key entries and SHA pins from `Driver.journal`;
- make legacy source spelling require a `FetchPin`, or a temporary adapter
  requiring both Vix `ValueId` and optional upstream digest. URL-only and
  SHA-only calls are compile-time typed errors;
- make legacy fetch return `Blob`; keep `crate_archive`/`untar` as a separate
  demand;
- rename/rebuild `HttpArchiveFetchBackend` as an HTTP Blob
  origin/observation adapter. HTTP transport never extracts archives;
- migrate FakeFetchBackend, daemon, wire, wasm, Lua, Taxon, and facet-core
  consumers to authored bytes plus canonical pins. Fake transport remains
  labelled test/demo evidence and is not a production origin;
- migrate `cargo.vix` update behavior to explicit observation producing a
  `FetchPin`, followed by pinned fetch for the build demand. This retains the
  existing demo boundary while removing SHA-only identity;
- remove the URL-only function from `corpus-next/index.vix`. FV-D1 may accept an
  already-observed Blob/claim for parsing; the typed registry observation that
  replaces it belongs to FV-D2.

No generated Vix source or fixture answer table supplies pins. A test fixture
may carry a declared canonical pin beside authored bytes, and its certificate
must recompute and compare the pin through the production identity writer.

## Observability and counters

Add typed events/counters sufficient to prove serving behavior without making
events authoritative:

- primitive started/completed/discarded by `PrimitiveId` and demand key;
- pinned fetch served from local Store, value provider, or origin;
- observation claim loaded/rejected, observation performed, refresh unchanged,
  refresh changed;
- bodies transferred and bytes transferred per serving tier;
- value/claim persistence puts and verified/rejected gets.

The existing `fetched(n)` check becomes a view over pinned-origin transfers,
not every fetch demand. Store/provider hits must be distinguishable from origin
work. Both execution lanes emit the same semantic facts.

## Implementation sequence

1. **FV-D1A — Foundation repair.** Add registered primitive descriptors,
   generic invocation lowering/dispatch, `EffectCtx`, non-blocking tickets,
   generic receipt/memo policy, typed failures, and structural certificates.
   Migrate authoritative external primitive adapters without changing their
   domain semantics. Commit the first compiling red checkpoint if the
   foundation gate exposes a deeper runtime contract.
2. **FV-D1B — Pinned Blob retrieval.** Add `FetchPin`, semantic persistence
   interfaces, Store/provider/origin lookup order, mandatory `ValueId`
   verification, optional upstream verification, and raw-Blob HTTP transport.
3. **FV-D1C — Observation claims.** Add typed observation requests, explicit
   refresh generations, durable claims, change events, persistence
   serialize/drop/load/reverify, and corruption controls.
4. **FV-D1D — Legacy migration.** Delete the optional fetch API/private fetch
   journal, migrate all Rust/Vix call sites, separate extraction, and keep demo
   backends visibly non-production.
5. **FV-D1E — Certificates and broad gates.** Land production-path
   integration certificates, Dodeca references, dual-lane evidence, broad
   suites, static gates, and an evidence section in this packet. Then return
   the exact immutable Facet/Vixen objects to the same committee before push.

Each coherent phase is committed forward. A red phase is preserved with typed
traces and returned to committee if it contradicts this design; it is never
backed out or narrowed into a fixture-only success.

## Verification ladder

Every filtered run is listed before execution with the identical selector.

### Focused production certificates

Add one integration binary, provisionally `fetch_observe`, whose tests enter
through Vix lowering, the scheduler, registered primitive dispatch, Store,
memo, receipt, and persistence—not by calling backend helpers directly:

- `pinned_fetch_origin_returns_blob_and_separate_extraction`;
- `pinned_fetch_local_store_hit_never_contacts_provider_or_origin`;
- `pinned_fetch_provider_hit_precedes_origin`;
- `pinned_fetch_rejects_vix_identity_mismatch`;
- `pinned_fetch_rejects_upstream_digest_mismatch`;
- `observation_records_and_replays_durable_pin`;
- `observation_refresh_unchanged_reuses_value_identity`;
- `observation_same_refresh_generation_dedupes`;
- `observation_refresh_change_is_explicit_and_prior_result_linked`;
- `observation_corrupt_body_or_claim_is_rejected`;
- `persistent_fetch_and_observation_reload_in_fresh_runtime`;
- `discarded_primitive_completion_publishes_no_value_or_claim`;
- `legacy_fetch_rejects_missing_vix_identity`.

Run the exact selection natively and with `WEAVY_JIT=0`. Each certificate
asserts lane agreement, task/primitive counts, Store/memo verdicts, receipt
contents, provider/origin call counts, and transferred bytes.

### Existing regression bundles

List and run in both lanes:

- rungs 066-082 and 101-110, including 075-077 fetch/extract and persistence;
- the persistence-journal binary;
- Taxon and facet-core production-demo ladders;
- Cargo/Rodin/manifest oracle bundle;
- full `vix --features real-process` and full `weavy`;
- `vix-fetch`, `vix-daemon`, `vix-wire`, and `snark-wasm` consumers affected by
  the public backend migration.

The HTTP production certificate uses an in-process real TCP server with
deterministic bytes, status, and headers. A live crates.io smoke may be
separately ignored/opt-in, but connectivity failure cannot turn the
authoritative certificate green.

### Structural and specification gates

The final tree must prove:

- no `FetchBackend::fetch(_, Option<_>)` or equivalent identity-optional
  production entry point;
- no `fetch(url: ...)` or SHA-only Vix production call;
- no fetch result `Tree` and no transfer-time archive extraction;
- no URL/SHA-keyed private fetch journal/cache;
- no per-primitive scheduler branch or scheduler-owned result field for the
  external primitives migrated to the registered boundary, including no
  scheduler `Op::Fetch` branch;
- no stringly fetch/observation failure boundary;
- targeted Dodeca rules above each report implemented and verified, invalid
  reference count does not increase, and the complete `ddc coverage status`
  is recorded without claiming unrelated uncovered rules.

Then run:

```sh
cargo check --workspace --all-targets --all-features
cargo fmt --all -- --check
just clippy-workspace-all
git diff --check
git status --short --branch
```

No warning is suppressed. Both Facet and Vixen worktrees finish clean.

## Negative controls

- URL and SHA-256 stay identical while the requested Vix `ValueId` is wrong:
  fetch fails before publication.
- Vix `ValueId` is correct while upstream SHA-256 is wrong: every serving tier,
  including a resident local body, fails provenance verification without
  publishing a result.
- origin and provider are unavailable while local Store has the exact Blob:
  fetch succeeds with zero external calls.
- provider returns corrupt bytes for the requested identity: it is rejected and
  origin may be tried; corrupt bytes never enter Store.
- persisted body claims the wrong `ValueId`: journal load rejects it.
- observation claim names a missing/corrupt body or mismatched request: it is a
  rejected claim and the observation runs again under policy.
- `ReusePinned` never performs network work when an accepted claim/body exists;
  a fresh `Refresh(generation)` performs one authored observation, while an
  exact re-demand of that generation deduplicates.
- changed refresh content produces an explicit prior-result-linked change event
  and new claim; the older demand remains reproducible.
- killing an in-flight HTTP primitive publishes no memo entry, observation
  claim, or partial Blob.
- archive bytes with different compression but equivalent extracted content
  remain distinct Blob identities and may yield one Tree identity after
  extraction.
- the event sink can be disabled without losing durable observation claims.

## Non-goals

- No crates.io `Registry`, generic sparse path algorithm, row typing, auth
  policy, or real package-universe lookup (`FV-D2`).
- No Rodin lazy row demand or `MiniSolve` semantic retirement (`FV-D3`).
- No closed resolution recipe or lock/update UX (`FV-D4`); FV-D1 supplies the
  typed pin and observation facts those later recipes carry.
- No product vx-store implementation, cross-tenant policy, peer discovery, or
  replication protocol. FV-D1 defines and verifies the open semantic
  persistence interface and lookup order.
- No VFS/sandbox runner integration or toolchain capability work.
- No claim that FakeFetchBackend, local HTTP fixtures, Taxon, or facet-core are
  terminal production registry evidence.

## Committee review requests

Reviewers should attack these specific causal claims:

1. whether FV-D1 must own the registered-primitive foundation repair rather
   than adding a hardcoded observe branch;
2. whether `FetchPin.value: BlobId`, backed by the runtime `ValueId`, correctly
   preserves the semantic `(SchemaRef, ContentHash)` identity law without
   making callers author schemas or exposing storage fields;
3. whether explicit refresh generations make observation changes visible and
   pure enough for later lock/update semantics;
4. whether value bodies and observation claims have distinct trust/reload
   paths, with no private cache disguised as a journal;
5. whether the authoritative-runtime/legacy-adapter migration closes every
   identity-optional production call without pretending FV-D2 is done;
6. whether the test matrix proves lookup order, crash/discard safety,
   persistence, and both execution lanes through the real scheduler path.

## Evidence record

Populate only after committee adjudication and implementation. Until then,
`FV-D1` remains open.

- Plan review: pending.
- Facet implementation tip: pending.
- Vixen evidence parent: pending.
- Focused native/interpreter: pending.
- Regression bundles: pending.
- Full suites and static gates: pending.
- Targeted Dodeca coverage: pending.
- Post-diff committee verdict: pending.
- Publication refs and remote verification: pending.
