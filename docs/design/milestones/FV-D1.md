# FV-D1 design packet: observation is not fetch

Status: revised committee candidate, 2026-07-15.

Controlling roadmap: [Vix full-vision milestones](../vix-full-vision-milestones.md),
milestone `FV-D1` only. Normative runtime authority lives in the Facet Vix
machine specification at `vix/docs/content/spec/machine/_index.md`, especially
`identity.md`, `primitive.md`, `receipt.md`, `persistence.md`, `scheduler.md`,
and `lifecycle.md`.

This revision incorporates the FV-D1 architecture and adversarial committee's
adjudication of the first immutable packet. In particular, it makes three
load-bearing corrections:

- schema identity is repaired before registered primitives are introduced;
- effect tickets are demand-owned and wait through actual Weavy suspension;
- the name-recognized `MiniSolve` host stub is deleted, not registered.

## Assignment and exact bases

- **Facet home:** create `vix/full-vision-d1` from the remotely verified FV-00
  tip `8db42feb6ebfb8ed921bba41f9d3b81f43d980e8` only after this revised
  packet is approved.
- **Vixen documentation:** `rodin-workspace-scope`; the first candidate is
  `cbf8b398553104d90882e527ef9accbf56cad3c9`. Vixen receives design and
  evidence records only; no Vix runtime implementation moves here.
- **Stop condition:** close schema authority where required by the primitive
  boundary, registered primitive dispatch, fetch/observation identity,
  persistence/replay, duplicate-authority deletion, and the gates in this
  packet. Do not begin `FV-D2` registry modeling, real sparse-path lookup, or
  Rodin lazy row demand.

No FV-D1 implementation branch exists at the time of this revision.

## Greenfield authority

Vix is pre-user and greenfield. FV-D1 has no compatibility, migration-reader,
adapter, deprecation-period, wire-version-preservation, or old-journal-reading
obligation. When the current shape conflicts with the settled machine laws,
replace it and document why.

This is not permission to delete proof. Before deleting an obsolete authority,
port every still-valid semantic assertion, negative control, oracle comparison,
and production-path certificate to the replacement. Itemize tests whose old
claim was invalid and the new certificate that replaces each one. No legacy
surface remains merely to keep an old test green.

## Outcome

There is one acquisition law:

- `fetch` is `Pinned`. Its request carries the final semantic `ValueId` of a
  `Blob` before execution. Origin coordinates and upstream SHA-256 are
  provenance/integrity facts, never identity.
- `observe` is `Observed`. Its result identity becomes known only through an
  explicitly refreshed, receipted observation claim.
- both are registered typed primitives using the normal scheduler, Store,
  memo, receipt, and persistent journal authority;
- a pinned fetch resolves local Store, configured value providers, then origin;
  every candidate is self-verified against the requested `ValueId` before
  admission;
- no Rust API, Vix surface call, wire contract, or fixture can select between
  pinned and observed behavior by omitting an identity;
- fetch returns `Blob`; extraction is a separate pure demand returning `Tree`;
- primitive waits yield a Weavy frame and resume from typed completion; the
  scheduler thread never drains an external effect synchronously;
- the compiler, VIR, and runtime contain no `MiniSolve` special case.

## Current-source audit

### Duplicate compiler/runtime acquisition authority

Facet `vix/src/lib.rs:3-6` names `surface -> vir -> lowering -> runtime` as the
authoritative compiler path. The crate-root parser and `vix::machine` still own
a competing acquisition law. FV-D1 ports useful evidence to the authoritative
path and deletes that duplicate acquisition authority; it does not preserve a
"migration" surface.

The rejected shape is concrete:

- `vix/src/fetch.rs:13-15` defines
  `FetchBackend::fetch(url, Option<expected_sha256>)` and returns a `Tree` plus
  actual SHA-256.
- `vix/src/machine/lower.rs:7392-7438` accepts `fetch(url: ...)`, optionally
  accepts `sha256`, encodes absence as `-1`, and declares the result `Tree`.
- `vix/src/machine/driver.rs:2534` owns a private `BTreeMap<String, i64>`
  journal. `prepare_fetch_request` chooses `fetch:<url>:sha256:<digest>` or
  `fetch:<url>:observed`; `finish_fetch_run` stores the observed digest and
  allocates the backend-provided `Tree`.
- `vix-fetch/src/lib.rs:39-91` repeats the optional-SHA contract, performs HTTP,
  and couples transfer to gzip/tar extraction.
- `vix-fetch/tests/fetch.rs` allows crates.io connectivity failure to turn a
  network smoke into success. That is not a production certificate.

Production/demo callers still choose the wrong law:

- `vix/corpus-next/cargo.vix` performs a SHA-only archive fetch;
- `vix/corpus-next/index.vix` performs a URL-only fetch and expects `Tree`;
- Taxon, facet-core, Lua, daemon, wire, and wasm tests inject
  `FakeFetchBackend`, whose archive bytes and returned tree are independent.

All such in-repository call sites are ported or deleted before FV-D1 closes.
There is no temporary identity-optional adapter and no compatibility exception.

### Useful authoritative substrate, not yet the foundation

- `vix/src/runtime/identity.rs` owns a framed BLAKE3 writer and a typed
  `ValueId`.
- `vix/src/runtime/store.rs` stores by `ValueId`, self-verifies journal bodies,
  and persists values separately from memo claims.
- `vix/src/vir.rs` models `RegistryUrl -> PinnedUrl` and `Fetch -> Blob`.
- `vix/src/runtime/scheduler.rs` verifies fixture bytes against a pinned Vix
  digest, records a read witness, and memoizes through the scheduler. Rungs
  075-077 prove fixture pinning, same-run dedup, and separate extraction.

But `Op::Fetch` is still a scheduler match arm, the only origin is the fixture
store, and there is no `PrimitiveId`, descriptor, object-safe primitive trait,
`EffectCtx`, demand-owned `EffectTicket`, or typed observation-claim journal.

### Schema identity is currently unsound

`vix/src/lowering.rs:685-686` and
`vix/src/runtime/scheduler.rs:5036-5037` derive a runtime schema identity from
`"vix.semantic.v1:" + Type::name()`. Record and enum names are nominal; generic
arguments and full structure do not participate. Distinct semantic schemas can
therefore collide.

This cannot be deferred to FV-D2 or hidden behind built-in-only primitive
schemas: typed document decode already accepts arbitrary authored target types
and emits a host boundary carrying this identity. `machine.identity.schema-ref`
settles that every identity-bearing API carries an interned, parameterized,
Taxon-derived `SchemaRef`.

Taxon already owns the content/structure-derived parameterized schema
algorithm. FV-D1 reuses that authority; it does not create a parallel Vix
`canonical_type` algorithm or retain `SchemaId::named(Type::name())` for
identity-bearing values.

### Current exec parking is not suspension

`vix/src/runtime/scheduler.rs:3663-3690` marks an exec task parked and then
calls `child.wait_with_output()` synchronously on the scheduler thread. The
state label says parked, but the scheduler is blocked until the process exits.
That violates `machine.scheduler.block-on-event`,
`machine.scheduler.no-shadow-scheduler`, and `machine.scheduler.effect-overlap`.

"Real Weavy suspension" in this packet has a precise meaning: the executing
Weavy task yields with its frame retained, control returns to the runtime, a
demand-owned completion registration remains live outside that frame, and a
typed completion later resumes the task directly. There is no scheduler-thread
wait, `block_on`, poll loop, or clock-driven harvest. The existing typed decode
yield demonstrates frame yield/resume, but external I/O must use the same
substrate rather than a synchronous host drain.

### `MiniSolve` is a fabricated-result bypass

The first packet described `Op::MiniSolve` as unrelated drift that might be
wrapped temporarily. Current-source inspection disproves that disposition:

- `compiler.rs:8137-8225` recognizes the literal callee name `mini_solve` plus
  a fixture-only signature, lowers the universe, then emits `Op::MiniSolve`
  with zero value inputs. The authored universe and requirements are discarded.
- `compiler.rs:8231-8256` extracts literal package names or a
  `FixtureWorkspace` marker instead of executing authored Vix solver code.
- `scheduler.rs:717-773` selects rows through hardcoded string switches such as
  `row.contains("-> libb")` and `text.contains("libd")`.
- `scheduler.rs:5239-5255` discards selected packages and returns
  `Some(OrderedMap(Vec::new()))` unconditionally.

This is not a rough solver. It is laziness scaffolding for rungs 094/099 that
bypasses the Vix function and fabricates the weakest answer their assertions
accept. Registering it would launder that answer through the new primitive
authority.

The genuine solver evidence is separate:

- rungs 089-093 execute a Vix-authored mini kernel over a rows-bearing universe
  and do not match the special signature;
- `rodin/kernel.vix` is the production Vix solver target and is checked against
  Cargo through `rodin_fixtures`.

FV-D1 first ports the valid ticket/read-receipt/persistence mechanics behind
094/099 to generic fetch/observe certificates, then deletes
`MiniSolveRequirements`, `Op::MiniSolve`, compiler recognition, scheduler
helpers, fabricated-result construction, and all special memo/receipt cases.
The 094/099 solver claim is explicitly invalid evidence. FV-D3 later connects
`rodin/kernel.vix` to FV-D2's typed lazy registry projections; it does not
revive a host solver or fixture-preselected universe.

### Measured specification gap

At the FV-00 base, Dodeca reports:

```text
ddc coverage status --source vix .
Referenced 52/232 (22.4%), Implemented 50/232 (21.6%), Verified 28/232 (12.1%)
```

The report is evidence, not a completion claim. The governing rules for the
registered primitive, journal, and persistence boundary have no complete
implementation/verification mapping. `vix/tests/ratchet/FOUNDATION.md`
requires those structures past rung 082; green behavior through hardcoded
branches is not the named foundation.

## Governing invariants

FV-D1 carries and verifies at least these rules:

- identity: `machine.identity.single-module`, `machine.identity.blake3`,
  `machine.identity.value-identity-pair`, `machine.identity.framed-encoding`,
  `machine.identity.hash-at-construction`, and
  `machine.identity.schema-ref`;
- primitive: `machine.primitive.trait`, `machine.primitive.registered`,
  `machine.primitive.requests-are-values`, `machine.primitive.memo-policy`,
  `machine.primitive.effectctx-witness-only`,
  `machine.primitive.fetch-is-an-invocation`,
  `machine.primitive.fetch-is-pinned`,
  `machine.primitive.fetch-returns-a-blob`,
  `machine.primitive.fetch-integrity-vs-identity`, and
  `machine.primitive.typed-deserialization`;
- scheduler: `machine.scheduler.passive-no-loop`,
  `machine.scheduler.block-on-event`,
  `machine.scheduler.completion-resumes-direct`,
  `machine.scheduler.join-atomic`, `machine.scheduler.no-shadow-scheduler`,
  `machine.scheduler.replay-is-semantics`,
  `machine.scheduler.tickets-outlive-tasks`,
  `machine.scheduler.chaos-kill-oracle`, and
  `machine.scheduler.effect-overlap`;
- lifecycle: `machine.lifecycle.effect-failure-is-a-result`,
  `machine.lifecycle.failure-carries-receipt`,
  `machine.lifecycle.ticket-liveness`,
  `machine.lifecycle.cancellation-poisons-not-memoizes`,
  `machine.lifecycle.freeze-transactional`, and
  `machine.lifecycle.stable-snapshot`;
- receipt: `machine.receipt.witness-reads`,
  `machine.receipt.complete-by-construction`,
  `machine.receipt.journal`, and
  `machine.receipt.fetch-observation-pin`;
- persistence: `machine.persistence.trait-boundary`,
  `machine.persistence.value-vs-claim`,
  `machine.persistence.reverify-on-load`,
  `machine.persistence.lookup-order`,
  `machine.persistence.four-lifetimes`, and
  `machine.persistence.value-and-claim-interfaces`;
- cache: `machine.cache.three-kinds`, `machine.cache.no-private-caches`, and
  `machine.cache.effects-are-memo-entries`.

False negatives are permitted. A body or claim that cannot be verified is a
miss or transient unavailability. A mismatched body, stale claim admitted
without policy, URL-keyed value, SHA-named value, fabricated solver result, or
transient failure persisted as a semantic negative is a forbidden false
positive.

## D1A0: one semantic schema authority

Identity-bearing runtime structures use the Taxon semantic schema model:

```text
SchemaRef = Concrete { id: TaxonSchemaId, args: [SchemaRef] } | Var(...)
ValueId = { schema: SchemaRef, content: ContentHash }
```

The concrete Rust representation may use interned handles, but equality and
serialization preserve the parameterized semantic structure. Facet metadata
is the ingress; the compiler constructs or reuses the Taxon schema closure;
the runtime consumes the resulting interned `SchemaRef`. Schema strings remain
debug/ingest data only.

The same authority flows through Store entries, demand preimages, primitive
descriptors, request/response values, receipts, value bodies, observation
claims, and persistence journals. Delete identity-bearing
`semantic_schema_id(Type::name())`, `SchemaId::named`, and any competing
canonical-type hash.

This changes value identity. Bump the value/store/journal identity epoch and
reject old-epoch bodies and claims. There is no migration reader. Authored
fixtures recompute expected identities through the production schema and
framed-value writer; generated files are regenerated from their source.

## D1A: registered primitive foundation

### Descriptor and dispatch

All protocol and persistence types derive `Facet`; serialization uses
`facet-json`, never serde or hand-written JSON.

```text
PrimitiveId { namespace: String, name: String, version: u32 }

PrimitiveMemoPolicy = Hermetic | Pinned | Observed | Volatile

PrimitiveDescriptor {
    id: PrimitiveId,
    request_schema: SchemaRef,
    response_schema: SchemaRef,
    failure_schema: SchemaRef,
    memo_policy: PrimitiveMemoPolicy,
    protocol_version: u32,
    capability_schemas: [SchemaRef],
}

Primitive::begin(request: ValueId, ctx: EffectCtx) -> EffectTicket
```

`Primitive` is object-safe and registered at machine construction. Lowering
emits one generic invocation containing descriptor identity and a typed request
value. Adding a primitive changes registration/package code, not compiler,
VIR, scheduler, receipt, memo, or persistence enums. The scheduler owns no
per-primitive result table, channel table, match arm, receipt variant, or
ontology string.

Pure operations remain ordinary Vix/VIR evaluation. Tree projection, tree
glob, blob length, and other computation over already-realized values are not
host primitives merely because Rust implements them. Archive extraction is
kept as a separate demand; if its current host implementation enters the
initial registered set, its request is content-addressed and its behavior is
Hermetic, never part of transport.

The initial migration required to prove the generic authority covers the
currently reachable external boundaries: typed document decode, exec dispatch,
pinned fetch, observe, and fixture origin adapters. Exec is `Volatile` and
nonpersistent until FV-E supplies confinement and complete external witnesses.
This milestone makes no E-plane hermeticity claim.

### Witness-only `EffectCtx`

`EffectCtx` is the primitive's only machine window. It exposes:

- witness-typed value and projection reads;
- typed result interning and verified candidate admission;
- progressive projection/codata publication where the response schema permits;
- typed causal event emission;
- mount-grant minting for later execution work;
- typed mandatory journal-observation emission;
- one completion/publication transaction.

It exposes no raw Store, memo, scheduler, filesystem path, network client,
executor, process-local capability, or generic "push receipt fact" handle.
Every Store-backed read automatically records its witness. Mandatory journal
observations are typed operations with their own schema and lifetime, not an
opt-in receipt loophole or a transient event.

### Demand-owned tickets and actual suspension

`begin` returns without blocking the scheduler. `EffectTicket` owns a typed
completion registration plus a renewable demand lease. The ticket belongs to
the `DemandKey`, not the task frame.

- Killing or evicting a requesting task leaves the ticket alive. Replay joins
  that same in-flight ticket atomically and observes exactly one backend
  operation and one publication.
- Completion publishes once, drains registered waiters, and resumes their
  Weavy frames directly.
- If the last waiter leaves, policy may cancel the demand/backend ticket.
  Cancellation is a transient control-plane completion: it publishes no memo,
  claim, receipt root, or partial value.
- A semantic timeout must be an explicit typed request field. Scheduler policy
  deadlines never enter Vix result identity.

The synchronous scheduler-thread exec `wait_with_output()` path is deleted.
External completion arrives from the primitive runtime through the typed
ticket delivery path. No replacement poll loop or shadow async executor is
introduced.

### Completion taxonomy

Every completion is exactly one of:

```text
Ok(ValueId)
Failed(FailureValueId)
MachineError(TypedMachineError)
```

`Failed` is a deterministic semantic negative, carries a complete receipt, and
memoizes under the primitive policy. A command-grammar termination or a
self-contradictory typed request may qualify.

Transport loss, current replica/origin unavailability, cancellation, exhausted
executor resources, policy rejection, refresh-head conflict, or corrupt
persistence are `MachineError` or candidate rejection and never memoize. A
bad body/claim/provider is rejected and the next admissible source is tried;
only exhaustion yields transient unavailability. Rendered strings never decide
the class.

## D1B: pinned Blob retrieval

```text
UpstreamDigest = Sha256([u8; 32])
BlobId(ValueId)

OriginHint {
    capability: ValueId,
    coordinate: String,
}

PinnedBlobRef {
    value: BlobId,
    origins: [OriginHint],
    upstream: Option<UpstreamDigest>,
}

PinnedFetchRequest { pin: PinnedBlobRef }
```

`BlobId` wraps the canonical `ValueId` of a `Blob` and can only be constructed
with the canonical Blob `SchemaRef` through the single runtime identity writer.
A bare digest is not a semantic pin. `OriginHint` and upstream SHA-256 do not
enter result identity.

Fetch execution is:

1. validate the requested Blob schema and descriptor;
2. resolve the exact `ValueId` in the resident Store;
3. query configured `ValuePersistence` providers in policy order;
4. query admissible origins through capability adapters;
5. hash every candidate through the canonical framed writer, verify the
   requested `ValueId`, then verify optional upstream provenance;
6. only after verification, intern the Blob, offer it to persistence, publish
   the generic memo result, and record typed provenance through the journal.

There is no fetch table. A resident hit performs zero transfer. Upstream
provenance is checked regardless of serving tier, so a contradictory pin does
not become acceptable merely because the Blob identity matches.

The normative interface remains generic:

```text
ValuePersistence::get(ValueId) -> Option<ValueBodyCandidate>
ValuePersistence::put(ValueId, ValueBody)
ClaimPersistence::get_observation(ObservationLookup) -> Option<ClaimCandidate>
ClaimPersistence::append_observation(ObservationClaim)
ClaimPersistence::compare_exchange_head(key, expected, next)
```

FV-D1's concrete body codec/provider is explicitly
`CanonicalBlobPersistence`/`BlobBodyCodec`; it does not claim codecs for every
schema. Passing a non-Blob schema to that provider is a typed rejection. The
generic interface remains because the settled persistence law is not
Blob-specific. Every returned candidate is untrusted until open Vix verifies
it.

Generic receipts do not grow a `PinnedFetchReceipt` enum variant. Serving tier,
origin, and upstream verification are typed provenance/journal values referenced
by the ordinary receipt.

## D1C: observation claims and refresh history

```text
ObservationKey {
    primitive: PrimitiveId,
    capability: ValueId,
    request: ValueId,
}

RefreshGeneration(ValueId)

RefreshDirective =
    ReusePinned |
    Refresh {
        generation: RefreshGeneration,
        expected_predecessor: Option<ObservationClaimId>,
    }

ObservationClaim {
    id: ObservationClaimId,
    key: ObservationKey,
    generation: RefreshGeneration,
    predecessor: Option<ObservationClaimId>,
    result: ValueId,
    receipt: Receipt,
}
```

`RefreshGeneration` is a caller-authored typed value, not an ambient runtime
counter or timestamp. The explicit predecessor makes the mutation precondition
part of the request. Claims are immutable content-addressed values. Persistence
keeps an append-only generation index `(key, generation) -> claim` and a mutable
current-head pointer `key -> claim` updated only by compare-and-exchange.

Refresh publication is transactional by reachability:

1. observe and verify the candidate body;
2. write the immutable body;
3. append the immutable claim and generation index with put-if-absent/CAS;
4. compare-exchange the head from `expected_predecessor` to the claim;
5. only then publish the observation demand result.

A crash may leave unreachable self-verifying body/claim objects, never a
trusted dangling head. Concurrent same-generation/different-result claims are
a transient conflict. Different generations racing from one predecessor yield
one head-CAS winner; losers are transient and nonmemoizing.

An exact re-demand of `(key, generation)` resolves its historical immutable
claim even if the current head later changes. `ReusePinned` witnesses and
resolves the current head separately, so a head change invalidates that demand.
A successful refresh receipt witnesses the explicit predecessor input, not the
post-write mutable head in a way that invalidates itself immediately.

Claims and bodies have distinct load trust paths. On load:

- bodies self-verify semantic schema, framed content, identity epoch, and
  `ValueId`;
- claims reverify key/request equality, predecessor/generation linkage,
  receipt framing and witnesses, result-body availability/identity, current
  policy, and identity epoch;
- uncertain/corrupt candidates are rejected and another source may be tried;
  they never become memoized semantic failures.

The complete change history is reconstructable from immutable predecessor
links and generation index without the event sink. A typed
`ObservationChanged { predecessor, prior_result, current_result }` event is
still emitted for observability, but is not authority.

An observed HTTP Blob result contains the resident Blob plus a derived
`PinnedBlobRef`. Lock/update code may persist that pin; locked execution later
uses only pinned fetch. FV-D2 defines crates.io-specific typed observation
constructors over this substrate.

## D1D: duplicate-authority deletion and proof port

After replacement certificates exist:

- delete `FetchBackend::fetch(url, Option<_>)` and all equivalent optional
  identity APIs;
- delete the URL/SHA keyed `Driver.journal` acquisition entries;
- make HTTP transport return raw Blob bytes/provenance, never extract archives;
- delete URL-only and SHA-only Vix calls; use typed observe for discovery and
  `fetch(PinnedBlobRef)` for locked retrieval;
- port or delete the old `vix-fetch`, daemon, wire, wasm, Lua, Taxon,
  facet-core, and fake-backend contracts so authored bytes are verified by the
  production identity writer;
- delete the duplicate compiler/driver acquisition path once its still-valid
  assertions run through `surface -> VIR -> lowering -> runtime`;
- delete all `MiniSolve` compiler/VIR/runtime symbols and replace the valid
  094/099 mechanics with generic primitive/fetch/observe evidence.

No adapter, deprecated legacy entry point, alternate epoch reader, or second
journal remains. The broader `vix::machine` module may remain only for unrelated
semantics that no longer own a competing acquisition, identity, or effect
authority.

Test preservation is explicit in the implementation evidence:

- each deleted test is listed with the obsolete claim it froze;
- each still-valid assertion names its replacement certificate;
- Cargo/Rodin external-oracle coverage and protected negative controls are not
  reduced;
- rungs 089-093 and `rodin/kernel.vix` evidence remain genuine Vix execution;
- rungs 094/099 are no longer described or counted as solver correctness.

## Observability

Typed events/counters prove serving and scheduling behavior without becoming
authority:

- primitive registered/started/completed/joined/cancelled by `PrimitiveId` and
  demand key;
- Weavy task yielded/resumed/killed and ticket waiter count;
- pinned fetch served from Store/provider/origin;
- observation claim loaded/rejected, refresh unchanged/changed/conflicted;
- bodies and bytes transferred per serving tier;
- value/claim persistence writes and verified/rejected reads;
- typed semantic failure versus transient machine error.

The existing `fetched(n)` view counts pinned-origin transfers, not demands.
Store/provider hits and ticket joins are distinguishable from origin work.
Both execution lanes emit the same semantic facts.

## Implementation sequence

1. **FV-D1A0 — Schema authority.** Introduce Taxon-derived parameterized
   `SchemaRef` across identity-bearing runtime APIs; remove nominal-name
   identity; bump/reject the old epoch; port fixtures through the production
   writer.
2. **FV-D1A — Primitive foundation.** Add descriptors, generic
   lowering/dispatch, witness-only `EffectCtx`, demand-owned tickets, typed
   completion taxonomy, uniform memo policy, and real Weavy yield/resume.
   Replace the scheduler-thread exec drain. Commit a compiling red checkpoint
   if the settled invariant exposes a deeper runtime contract.
3. **FV-D1B — Pinned Blob retrieval.** Add `PinnedBlobRef`, lookup order,
   mandatory `ValueId` verification, optional upstream verification, generic
   persistence interfaces, concrete Blob codec, and raw-Blob HTTP transport.
4. **FV-D1C — Observation.** Add typed requests, explicit generation and
   predecessor, immutable claim history, generation index/head CAS,
   serialize/drop/load/reverify, and conflict/corruption controls.
5. **FV-D1D — Delete duplicate authorities.** Port proof first, then remove
   optional fetch APIs, private journals, coupled extraction, obsolete callers,
   and every `MiniSolve` special case.
6. **FV-D1E — Certificates and broad gates.** Land production-path
   integration certificates, Dodeca references, dual-lane evidence, full
   suites, structural gates, and the evidence record below. Return exact
   immutable Facet/Vixen objects to the same two committee seats before push.

Each coherent phase is committed forward. A red phase is preserved with typed
traces and returned to committee if it contradicts the design; it is not backed
out or narrowed into fixture-only success.

## Verification ladder

Every filtered Rust run is listed first with the identical `cargo nextest list`
selector, then run with `cargo nextest run`, natively and with `WEAVY_JIT=0`.

### Foundation certificates

- arbitrary parameterized schemas produce distinct Taxon `SchemaRef`s and
  distinct `ValueId`s in lowering, Store, receipt, and journal;
- typed document decode agrees across lanes using the same schema authority;
- old-epoch body and claim journals are rejected rather than migrated;
- registered primitive dispatch adds no primitive-specific machine branch;
- a primitive read through `EffectCtx` appears in its receipt without an
  explicit recording call;
- task kill during HTTP observation replays, joins the same demand-owned
  ticket, and produces exactly one origin transfer and one publication;
- last-waiter demand cancellation cancels policy work and publishes zero memo,
  claim, receipt root, or partial Blob;
- transient origin loss/corrupt candidate does not memoize; after recovery or
  a second admissible source, the same demand succeeds;
- two independent effects overlap while their Weavy frames are suspended;
- exec dispatch contains no synchronous scheduler-thread
  `wait_with_output`/`wait`/`block_on` drain.

### Fetch and observation production certificates

Add one integration binary, provisionally `fetch_observe`, whose tests enter
through Vix lowering, scheduler, generic primitive dispatch, Store, memo,
receipt, and persistence:

- `pinned_fetch_origin_returns_blob_and_separate_extraction`;
- `pinned_fetch_local_store_hit_never_contacts_provider_or_origin`;
- `pinned_fetch_provider_hit_precedes_origin`;
- `pinned_fetch_rejects_vix_identity_mismatch`;
- `pinned_fetch_rejects_upstream_digest_mismatch_at_every_tier`;
- `observation_records_and_replays_durable_pin`;
- `observation_refresh_unchanged_reuses_value_identity`;
- `observation_same_generation_replays_historical_claim`;
- `observation_refresh_change_links_predecessor_and_updates_head`;
- `observation_concurrent_refresh_conflict_is_transient`;
- `observation_corrupt_body_or_claim_is_rejected`;
- `persistent_fetch_and_observation_reload_in_fresh_runtime`;
- `disabled_event_sink_preserves_complete_observation_history`.

The HTTP certificate uses an in-process real TCP server with deterministic
bytes, status, and headers. Live crates.io access may be an ignored smoke only;
connectivity failure cannot turn an authoritative certificate green.

### Existing regression and oracle bundles

List and run in both lanes:

- rungs 066-082 and 101-110, with the 094/099 replacement certificates listed
  separately and not called solver evidence;
- persistence-journal and source-revision binaries;
- Taxon and facet-core production-demo ladders;
- Cargo/Rodin/manifest bundle, including rungs 089-093 and
  `rodin_fixtures` Cargo differential;
- full `vix --features real-process` and full `weavy`;
- affected `vix-fetch`, `vix-daemon`, `vix-wire`, and `snark-wasm` consumers.

Record selection-count changes and itemize intentionally removed obsolete-path
tests. A lower count is never silently accepted.

### Structural and specification gates

The final tree must prove zero occurrences of:

- `FetchBackend::fetch(_, Option<_>)` or equivalent identity-optional fetch;
- URL-only or SHA-only production fetch calls;
- fetch returning `Tree` or transport-time archive extraction;
- URL/SHA-keyed private acquisition journal/cache;
- per-primitive compiler/VIR/scheduler/receipt/persistence variants for the
  registered external boundary, including a scheduler `Op::Fetch` arm;
- `MiniSolveRequirements`, `Op::MiniSolve`, name/signature recognition,
  hardcoded fixture row switches, or fabricated solver result construction;
- nominal `Type::name()` schema identity or identity-bearing
  `SchemaId::named`;
- synchronous effect drain on the scheduler thread for migrated primitives;
- string-rendered failure classification;
- old-epoch compatibility readers or greenfield adapters.

The blocking structural gate is scoped to scheduler-thread effect drains; it
does not ban legitimate blocking at an isolated worker/process boundary that
delivers completion through a ticket.

Targeted Dodeca rules above must report implementation and verification
references. Invalid reference count must not increase. Record the complete
coverage report without claiming unrelated uncovered rules.

Then run:

```sh
cargo check --workspace --all-targets --all-features
cargo fmt --all -- --check
just clippy-workspace-all
git diff --check
git status --short --branch
```

No warning is suppressed. Facet and Vixen worktrees finish clean.

## Negative controls

- URL and SHA-256 stay identical while requested Vix `ValueId` is wrong:
  candidate is rejected before publication.
- Vix `ValueId` is correct while upstream SHA-256 is wrong: every serving tier,
  including resident Store, rejects the contradictory request.
- origin/provider are unavailable while Store has the exact Blob: zero
  external calls.
- provider returns corrupt bytes: reject it, try the next admissible source,
  and admit only verified bytes.
- persisted body claims a wrong schema/content identity or old epoch: reject.
- observation claim names a missing/corrupt body, mismatched request, broken
  predecessor, or old epoch: reject and follow typed policy.
- `ReusePinned` does no network work for an accepted current claim; a new
  generation performs one observation; exact generation re-demand reuses its
  immutable historical claim.
- two generations race from one predecessor: one head winner, one transient
  loser, no memoized conflict.
- task kill during a live effect joins and publishes once; demand cancellation
  after the last waiter publishes nothing. These are separate tests.
- transport unavailability produces no memoized Vix failure; a later retry
  after recovery succeeds.
- archive bytes with different compression but equivalent extracted content
  remain distinct Blob identities and may produce one Tree identity.
- event sink disabled: durable observation history remains complete.

## Non-goals

- No crates.io `Registry`, generic sparse path algorithm, row typing, auth
  policy, or real package-universe lookup (`FV-D2`).
- No Rodin lazy row demand (`FV-D3`). `MiniSolve` deletion removes a false
  authority; it does not implement that later integration.
- No closed resolution recipe or lock/update UX (`FV-D4`).
- No product vx-store tenancy, placement, replication, peer discovery, or
  authorization policy.
- No claim that exec is hermetic before FV-E confinement/capability work;
  registered exec is `Volatile` here.
- No VFS/sandbox runner or toolchain capability integration.
- No claim that fake transports, local HTTP fixtures, Taxon, or facet-core are
  terminal production registry evidence.

## Committee review requests

Reviewers should attack:

1. whether D1A0 removes the current schema false-positive without inventing a
   second canonical schema algorithm;
2. whether the generic primitive path is the sole authority and `EffectCtx`
   makes receipts complete by construction;
3. whether demand-owned tickets, join/cancel distinction, and actual Weavy
   yield/resume satisfy replay and effect overlap;
4. whether semantic `Failed` versus transient `MachineError` prevents stale
   negative memoization;
5. whether `PinnedBlobRef`, value/claim persistence, and refresh CAS history
   preserve identity and concurrency laws;
6. whether proof-port-then-delete removes every optional-fetch/private-journal
   authority without weakening oracle evidence;
7. whether zero `MiniSolve` symbols and reclassification of 094/099 correctly
   remove fabricated solver evidence while preserving genuine Vix/Cargo proof.

## Evidence record

Populate after revised committee approval and implementation. Until then,
`FV-D1` remains open.

- Revised plan review: pending.
- Facet implementation tip: pending.
- Vixen evidence parent: pending.
- Focused native/interpreter: pending.
- Regression/oracle bundles: pending.
- Full suites and static gates: pending.
- Targeted Dodeca coverage: pending.
- Post-diff committee verdict: pending.
- Publication refs and remote verification: pending.
