# Vix full-vision milestones: from certified demos to no-cheat builds

Status: committee-reviewed execution roadmap, 2026-07-15.

This document turns the current Vix/Rodin/Rust-build direction into named,
assignable goals. It sequences existing design authority; it does not replace
the language or machine specifications. When this roadmap conflicts with a
settled invariant, the invariant wins and the roadmap must be corrected.

Companion authority, in precedence order where they overlap:

- The repository-level [Rodin production directive](../../CLAUDE.md): the only
  production solver kernel is written in Vix, and Cargo is the behavioral
  oracle for resolution correctness.
- [Vix synthesis](vix-synthesis.md) for vocabulary and invariants.
- The active Facet `rodin/EXECUTION-PLAN.md` for Rodin implementation order,
  gates, and stop conditions.
- [Rodin in Vix](rodin-in-vix.md) for the semantic-cutoff and fact-lifecycle
  concepts. Its older Rust-production-kernel topology is historical and does
  not override the repository directive or active execution plan.
- [Vix substrate: VFS + sandbox](vix-substrate-vfs-sandbox.md) and its
  [addendum](vix-substrate-vfs-sandbox-addendum.md) for the enforcing executor.
- [Capabilities: ambient vs materialized](capabilities-ambient-vs-materialized.md)
  for toolchain and host-fact identity.
- [Package manager and provenance](package-manager-and-provenance.md) for
  resolution requests, recipes, upgrades, and artifact-carried provenance.
- [Open/proprietary division](open-proprietary-division.md) for repository and
  ownership boundaries.

Repository names in this roadmap are architectural homes, not an instruction
to move code as part of an unrelated milestone:

- **Facet** is the open monorepo containing Vix, Rodin, Weavy, the public runner
  protocol, recipe schemas, and language-owned memo/receipt semantics.
- **Host runtime** is the separate open, reproducibly buildable release repo
  described by the open/proprietary division. Its VFS, sandbox, and runner
  substrate currently lives in `vixenware/vixen`; extracting it is part of the
  relevant execution milestones, not a reason to maintain two implementations.
- **Vixen** is this private product repo: fleet, shared services, registry
  operation, provenance service, and product integration. It may consume the
  open protocol and host runtime; it does not own Vix semantics.

## The terminal claim

The full-vision Rust build path has one production story:

1. A workspace enters as a VFS/CAS grant, not as a Rust test harness copying a
   known subtree.
2. Vix parses manifests and constructs the resolution request.
3. Rodin resolves against a demand-driven package universe. Registry rows are
   typed observations, fetched only when demanded.
4. Resolution/update produces a typed recipe containing the original request,
   the resolved graph, source identities, artifact identities, target facts,
   features, and capability requirements.
5. A locked build consumes only pins. Registry observation is not part of a
   locked build.
6. Vix derives the complete build graph and typed command invocations.
7. The open host runtime executes through the Vox runner protocol. Inputs are
   CAS grants mounted by `vx-vfsd`; seatbelt/Landlock/Windows confinement
   enforces the ceiling; actual file, directory, and lookup-miss observations
   return in the receipt. Vixen may schedule that runtime but does not redefine
   its execution semantics.
8. Materializable toolchains are immutable CAS inputs. Non-materializable
   ambient resources are identity-bearing, continuously watched capabilities.
   Neither may enter through implicit `PATH`, host-triple, or filesystem reads.
9. Cargo is an executable oracle only. No Cargo-selected package set, unit
   graph, generated Vix source, or recorded answer enters the production path.
10. Identical locked builds are offline memo hits. Irrelevant edits do not
    execute tools. Relevant changes invalidate exactly the work whose verified
    read sets intersect the change.
11. Produced artifacts carry their replayable recipe. Publication, overlays,
    and advisory claims have independently verifiable provenance rather than
    relying on a mutable service answer.

```text
workspace grant
      |
      v
manifest adapter ------> registry universe row(source, package)
      |                              |
      |                              +-- observe sparse row
      |                              +-- typed decode
      v
    Rodin
      |
      v
solve request + resolved graph + artifact coordinates
      |
      +-- update/lock: acquire, verify upstream digest, record BLAKE3 pin
      v
Vix build graph
      |
      v
Vox Runner RPC --> CAS-backed VFS --> sandbox --> rustc/build.rs
                         |                           |
                         +-- observed read set <----+
                         +-- output tree identities
```

## Goal assignment and committee gate

A milestone ID in this document is a goal handle. An assignment must name the
ID, every repository it will touch, the exact branch/base in each repository,
and any narrower boundary. For example:

> Implement `FV-D1` in Facet from `<exact commit>`. Stop after its listed gate;
> do not start `FV-D2`.

No milestone enters implementation merely because its paragraph looks clear.
Before implementation, its owner prepares a design packet containing:

- the exact current code paths and current failing or missing production proof;
- the proposed data types and authority boundaries;
- the invariant-preserving execution path;
- migrations or deletions required to prevent two competing paths;
- the Cargo/live-system oracle and negative controls;
- explicit non-goals and known residual boundaries;
- the exact focused and broad verification ladder.

When the packet is ready, summon a two-member Paseo committee of independent,
high-reasoning reviewers. Committee members receive the same packet, have no
edit authority, and review from contrasting roles: one architecture/causality
review and one adversarial correctness/no-shortcut review. The owner must:

1. collect both reviews;
2. verify their factual assumptions against current source;
3. adjudicate convergent findings into the packet;
4. bring material disagreement to Amos rather than choosing silently;
5. obtain committee confirmation on the adjudicated plan;
6. only then start implementation.

After implementation, the same committee reviews the diff and evidence against
the adjudicated plan. A milestone is complete only after implementation review
findings are resolved or explicitly accepted by Amos.

Committee review is a design/correctness gate, not implementation delegation.
The implementation owner remains responsible for the code and evidence.

This roadmap itself was reviewed and adjudicated before publication by Paseo
architecture reviewer `dd39d15b-aaaf-4a03-9698-1c8afcdc0a91` and adversarial
reviewer `a7a0f72d-5e13-410d-acae-f4accedb98fe`. Both confirmed the final
combined adjudication with no remaining material objection. That review does
not replace the fresh, current-source committee gate required for each
milestone.

## Current baseline and cheat ledger

The Taxon and facet-core demonstrations establish that Vix can parse the
relevant manifests, run Rodin, derive a narrow Rust unit graph, fetch/extract
real crate archives through the effect surface, execute build scripts, invoke a
real `rustc`, and retain artifacts and receipts. They are valuable production-
path evidence, but not yet the terminal claim above.

The following shortcuts must have named owners rather than fading into demo
folklore:

| Current shortcut | Why it is not the terminal path | Removed by |
| --- | --- | --- |
| Rust constructs a known workspace `Tree` | Bypasses workspace discovery and lazy VFS reads | `FV-E2` |
| Rust supplies a tiny, preselected sparse-index JSONL slice | Rodin never chooses against the real available universe | `FV-D2`, `FV-D3` |
| Sparse-index paths are hardcoded for known crate names | Cannot address arbitrary registry packages | `FV-D2` |
| `FakeFetchBackend` serves archives from Cargo's local cache | Does not prove registry observation or CAS acquisition | `FV-D1`, `FV-D2` |
| URL-only `fetch` becomes a journal-pinned observation | Conflates unknown identity with pinned retrieval | `FV-D1` |
| Vix-source `mini_solve(fixture_index())` consumes a typed universe assembled by the Rust harness | The solver is real Vix, but the available registry universe is still preselected outside it | `FV-D3` |
| `RealProcessBackend` stages files but trusts the host | Cannot enforce the mount ceiling or observe arbitrary reads | `FV-E1`, `FV-E2` |
| Demo `Rustc::acquire(Target::host())`, host target, and `PATH` resolution are ambient | Executor state can enter an artifact without an input identity | `FV-E3` |
| The Cargo adapter covers a narrow lib/build-script graph | Does not yet cover Cargo's complete package and unit identity model | `FV-C2` |
| Demo assertions sample graph shape | A sampled shape is weaker than exact Cargo differential evidence | `FV-C1`, `FV-C2` |

No milestone may close its row by replacing one shortcut with another private
cache, static expected table, generated source, Cargo-fed graph, fixture-only
backend, or special-cased package/function name.

## Dependency graph

The dependency plane and execution plane advance independently. Logical target,
policy, and capability requirements belong to the dependency plane; resolved
runner advertisements and machine identities belong to the execution plane.
They first join at the no-cheat facet-core build.

```text
FV-00
  |
  +-- FV-D1 -> FV-D2 -> FV-D3 -> FV-D3F -> FV-D4 --+
  |                                                  +-> FV-C1 -> FV-C2 -> FV-C3 -> FV-P1 -> FV-C4
  +-- FV-E1 -> FV-E2 -> FV-E3 -----------------------+
       |
       +-> FV-E2W -------------------------------------------------------------------------> FV-C4
```

`FV-E2W` is the Windows enforcement completion. It can proceed after `FV-E1`
and is required before `FV-C4` can claim the production default on Windows.

## FV-00 — Preserve and integrate the proven baseline

**Home:** Facet, plus authority corrections in Vixen documentation.

**Status:** complete. The committee-approved Facet and Vixen objects were
published and verified at their named remote refs on 2026-07-15.

**Outcome:** the completed Vix runtime/Rodin/Taxon/facet-core work is reachable
from one clean, pushed integration lineage, with its proof boundaries recorded.

**Required work:**

- Integrate the completed facet-core branch after the existing Taxon and Vix
  runtime/Rodin work.
- Preserve every forward checkpoint and reconcile the local Facet main-line
  commit rather than rewriting history.
- Reconcile stale Rodin documents with the repository directive and active
  `rodin/EXECUTION-PLAN.md`; do not preserve the retired Rust-production-kernel
  topology as a second authority.
- Record exact branch tips and the authoritative full-suite results.
- Keep `RealProcessBackend` and fixture fetch clearly labelled as demo/evidence
  backends rather than production defaults.

**Gate:** focused Taxon + facet-core ladders in native and `WEAVY_JIT=0`; full
Vix/Weavy suites in both lanes; Cargo/Rodin oracle bundle; workspace check,
format, Clippy with warnings denied, diff check, clean status, and pushed
lineage.

**Forbidden shortcuts:** dropping inherited commits, squashing away negative
checkpoints, weakening a negative test, or calling the current demo terminal.

## FV-D1 — Observation is not fetch

**Home:** Facet.

**Status:** partial demo substrate; production identity semantics not closed.

**Outcome:** acquisition obeys one identity law. A pinned fetch knows the Vix
`ContentHash` before execution; a moving external read is an observation with a
receipt.

**Required design:**

- `fetch` requires a BLAKE3 content identity. URL and upstream SHA-256 are
  provenance/integrity coordinates, not value identity.
- `observe` is a separate primitive for registry rows and other ambient reads
  whose identity becomes known only after execution.
- Observation pins survive journal persistence with explicit refresh policy.
- Pinned fetch resolution tries local Store/CAS before peer/shared Store and
  origin transfer.
- Existing URL-only and SHA-only call sites migrate or become typed errors.
- The concrete migration seam is Facet `vix/src/fetch.rs`:
  `FetchBackend::fetch(url, Option<sha256>)`. The optional checksum must not
  continue selecting two different identity laws behind one operation.

**Gate:** production-path positive and negative certificates for pinned fetch,
observation, replay, refresh, corruption, unavailable origin with a local CAS
hit, and persistence reload. Both interpreter/JIT lanes must agree. No
production call site can invoke an identity-optional `fetch`.

**Forbidden shortcuts:** one function whose semantics change when a hash is
omitted; URL as identity; SHA-256 as the Vix identity; a fetch-only private
journal outside normal memo/receipt authority.

## FV-D2 — The registry is a real universe

**Home:** Facet.

**Status:** partial registry/test substrate; blocked by `FV-D1`.

**Outcome:** crates.io is queried through a typed registry capability. No
prepared sparse JSONL or package-name switch enters resolution.

**Required design:**

- Treat Facet `vix/tests/sparse_index.rs`, existing sparse-index adapters, and
  the completed typed-decode/fetch rungs as partial substrate to audit and
  promote, not as evidence that the production registry boundary is already
  closed.
- Observe the real sparse-registry `config.json` and retain registry/source
  identity, download template, and authentication policy.
- Implement the generic crates.io sparse path algorithm for every legal crate
  name.
- Model `Registry`, `PackageId`, `PackageRowSet`, `PackageRow`, dependencies,
  features, yanked state, upstream digest, and source identity as typed values.
- Decode each JSONL row directly into typed values through the document host.
- Expose row lookup as a memoized projection keyed by registry identity and
  package identity.
- Implement real HTTP observation and CAS-backed artifact acquisition without
  invoking Cargo.

**Gate:** from an empty Store, demand arbitrary real package rows including the
one-, two-, three-, and four-plus-character sparse path cases; prove exact row
receipts, typed decode failures, HTTP/protocol failures, replay behavior, and no
access to unrelated rows. Compare decoded rows with the same pinned sparse
responses consumed by the Cargo oracle.

**Forbidden shortcuts:** fixture-only transport, local Cargo registry as the
production origin, hardcoded package/version lists, `Doc` or newline text as a
kernel interface, or downloading the entire index before a row is demanded.

## FV-D3 — Rodin demands package rows

**Home:** Facet.

**Status:** Vix kernel and fixture certificates exist; blocked by `FV-D2`.

**Outcome:** the production Vix Rodin kernel in `rodin/kernel.vix` resolves
against the lazy registry universe. Package-row effects are adapter projections
whose receipts flow into the solve; Rodin itself remains representation-neutral.
Facet `rodin/EXECUTION-PLAN.md` governs kernel semantics, ordering, and gates.

**Required design:**

- Replace the fully materialized demo `Index` boundary with a typed universe
  whose row/version projections are demanded by propagation and search.
- Retire the demo `mini_solve(fixture_index())` path whose Vix-source solver
  consumes a Rust-harness-assembled `FixtureIndex`/preselected universe after
  equivalent lazy production behavior is proven. There is no `Op::MiniSolve`
  or solver-specific compiler/runtime primitive to delete.
- Preserve Rodin's learned-region and structural-reuse semantics on the normal
  scheduler/Store authority.
- Ensure dependency discovery recursively nominates new package rows without a
  Rust-precomputed transitive closure.

**Gate:** resolve facet-core from its real manifest against real sparse rows;
compare exact package/source/version/features results with Cargo; prove an
unvisited package row is never observed; prove a changed relevant row
invalidates, an unchanged row reloads as a verified hit, and changed root
requirements reuse untouched row work. Run native and `WEAVY_JIT=0` plus the
full Rodin oracle corpus.

**Forbidden shortcuts:** `fixture_index()` or another harness-preselected
universe on the production path, solver-specific scheduler traversal, string
inspection of dependency rows, precomputed transitive row sets, a Rodin-private
cache, a Rust production kernel, or Cargo metadata as input.

## FV-D3F — Semantic cutoff reaches production Rodin

**Home:** Facet.

**Status:** generic Vix comparator substrate exists; blocked by `FV-D3` for the
real registry proof.

**Outcome:** Rodin marks the VersionSet premises whose narrowing admits semantic
memo reuse, and the Vix machine's existing comparator seam reuses the prior
result exactly when the new premise entails the old one.

**Required design:**

- Bind the production Rodin demand arguments to the existing Vix semantic
  comparator convention, with `new.subset(old)` for VersionSet narrowing.
- Version and identity the comparator as part of the computation contract.
- Persist and reverify semantic candidates through the normal Store/journal
  path; a loaded claim is never trusted merely because a comparator once
  accepted it.

**Gate:** a real-registry narrowing produces a semantic hit with no kernel
spawn; widening and a changed relevant row miss; an irrelevant row edit remains
warm; persistence drop/reload re-verifies the same behavior. The final typed
`SolveResult` and dependency graph agree with Cargo in native and
`WEAVY_JIT=0` lanes. Comparative cold-versus-warm traces prove the cutoff rather
than inferring it from equal outputs.

**Correctness basis:** Vix false-positive-never and replay invariants, Cargo as
the behavioral oracle for externally visible resolution, and cold-versus-warm
edit differentials. Cargo is not claimed to expose an internal fact trace, and
retired Rust resolver scaffolding is not revived as an oracle or production
dependency.

**Forbidden shortcuts:** result comparison after recomputation, a Rodin-private
memo table, trusting a loaded fact without replay, treating widening as
entailment, or routing fact lifecycle through Rust.

## FV-D4 — Resolution becomes a replayable recipe

**Home:** Facet.

**Status:** partial lock/recipe substrate; blocked by `FV-D3F`.

**Outcome:** update/resolve may observe a moving registry; build consumes a
typed, content-addressed resolution recipe and performs no registry observation.

**Required design:**

- The recipe contains both the original solve request and the complete resolved
  graph: exact source/package/version identities, enabled features, target facts,
  dependency kinds, artifact coordinates, upstream digests, Vix BLAKE3 pins,
  and capability requirements.
- Logical `Target`, `Policy`, and capability requirements are dependency-plane
  recipe types. The recipe contains no host-derived `Target::host()` result and
  no selected machine/toolchain identity; `FV-E3` performs that binding at
  execution closure.
- Acquiring a selected archive verifies its upstream digest, computes/stores the
  Vix content identity, and closes the recipe before a locked build begins.
- Cargo.lock participates as a source-of-truth journal backend where Cargo
  compatibility requires it; it is not merely a rendered report.
- Refresh/upgrade explicitly ignores selected observation pins and produces a
  new recipe with predecessor linkage.
- Treat Facet `vix/tests/crate_lock.rs` and existing lock adapters as partial
  substrate whose semantics must be promoted and audited, not a new design from
  zero.

**Gate:** a fresh update observes the registry, closes the recipe, and
materializes every selected source pin; the same locked recipe reloads and
materializes all selected sources offline from Store/CAS. Corruption and
source-identity substitution fail; an upgrade re-observes and produces a
structural resolution diff; serialization/drop/reload preserves only reverified
claims. Compiling that recipe belongs to `FV-C1`, not this gate.

**Forbidden shortcuts:** silently trusting the first observed bytes forever,
building from SHA-only coordinates, hidden mutable lock state, or re-running
Rodin during an unchanged locked build.

## FV-E1 — One publishable runner protocol

**Home:** Facet for the open protocol; host runtime for the real executor;
current development substrate in Vixen must migrate rather than fork.

**Status:** dual protocol/client substrate exists; public authority not yet
reconciled.

**Outcome:** Vix is a client of one publishable Vox exec protocol. The open host
runtime owns the auditable local executor implementation and never depends on
Vix. Vixen's private fleet implementation speaks the same protocol.

**Required design:**

- Reconcile the richer `exec-protocol` read-set/control surface with
  `vx-services::Runner` and choose one versioned public authority.
- Mounts cross as CAS tree identity plus grant, not inline tree contents.
- Requests carry normalized typed command roles, environment, declared output
  roots, toolchain/capability identities, and execution-platform requirements.
- Results carry typed failure, exit outcome, stdout/stderr streams, output tree
  identities, file/directory/miss read observations, observation scopes, and
  progressive tree events.
- `RpcRunnerBackend` maps the protocol into Vix's language-owned memo/receipt
  model without inventing a second semantic cache authority.

**Gate:** protocol round trips, version/capability negotiation, every typed
failure mapping, CAS grant rejection, read-set fidelity, output identity, and
native/interpreter client agreement against a fake protocol server and a real
open host-runtime runner.

**Forbidden shortcuts:** a Vixen dependency on Vix, private types on the wire,
inline tree transfer as the production mount model, stringly errors, or treating
declared roles as the observed read set.

## FV-E2 — Mount, confine, observe

**Home:** open host runtime; current development substrate in Vixen.

**Status:** VFS and sandbox primitives exist; their composed exec leaf is
blocked by `FV-E1`.

**Outcome:** the open host-runtime runner composes existing CAS VFS and sandbox
primitives into the enforcing exec leaf on macOS and Linux.

**Required design:**

- Create one action prefix in `vx-vfsd`; mount input grants; create writable
  output roots; resolve the selected toolchain capability.
- Feed the mount/output/toolchain ceiling into the existing macOS
  `seatbelt-exec` deny-default policy compiler, which already imports
  `system.sb`, and into Landlock/seccomp on Linux. Do not build a second policy
  generator or resurrect historical code over the current implementation.
- Deny network for build tools. Registry observation is a separate capability,
  not ambient network access granted to build scripts.
- Launch, stream output/tree events, enforce bounded cancellation, flush output
  trees to CAS, and drain `tracked_observations` for the action scope.
- Return file content, directory listing, and lookup-miss evidence sufficient
  for Vix's tier-2 verification.
- Add a typed `Poisoned` substrate outcome for a wedged FSKit run. A poisoned
  attempt never mints a deterministic process failure, miss, or cache entry;
  the runner kills the child, performs bounded sacrificial mount recovery, and
  requires a recovered or fresh mount before policy-controlled rescheduling.

**Gate:** real subprocess integration tests prove declared reads, directory
enumeration, negative lookup, declared writes, output flushing, and progressive
events. Adversarial tests prove reads from HOME, `/tmp`, sibling action prefixes,
undeclared toolchains, and network all fail because of enforcement—not because
the resource happened to be absent. A forced FSKit wedge returns `Poisoned`,
cannot enter memo state, recovers or abandons the mount without wedging the
orchestrator, and never retries against the same unhealthy mount.

**Forbidden shortcuts:** temp-directory staging as confinement, declared-input
read sets, host-trusting fallback on sandbox setup failure, unbounded FSKit
join/unmount, or a success path that omits negative observations.

## FV-E2W — Enforced exec on Windows

**Home:** open host runtime; current development substrate in Vixen.

**Status:** ProjFS materialization substrate exists; blocked by `FV-E1` and an
unselected complete-enumeration mechanism. Required for Windows completion of
`FV-C4`.

**Outcome:** a Windows VFS with complete directory-enumeration observation and a
Windows process sandbox enforce the same ceiling and report the same
observation model as macOS/Linux.

**Required design:** bank and implement the complete-observation VFS choice—an
NT driver or paid FUSE for Windows—before claiming the platform. ProjFS may
remain an interim lazy materializer or independent file-open signal, but it
cannot be the terminal read-set authority because it cannot observe directory
enumeration completely. Choose and implement the explicit Windows confinement
mechanism; preserve CAS-level observations; define process/job cancellation and
network denial.

**Gate:** the `FV-E2` positive and adversarial matrix passes on Windows with
equivalent typed protocol evidence.

**Forbidden shortcuts:** claiming ProjFS provides a complete read set, claiming
VFS visibility is process confinement, or silently weakening enumeration,
network, or host-filesystem negatives on Windows.

## FV-E3 — Capabilities, not PATH

**Home:** Facet for open requirement/advertisement schemas and capability
packages; host runtime for probes, materialization, watch, and enforcement.

**Status:** partial schemas and demo capabilities exist; blocked by `FV-E2`.

**Outcome:** every external resource affecting a build is an identity-bearing
input selected before execution.

**Required design:**

- The demand root supplies target, registry, Rust toolchain, C toolchain,
  linker, and other admitted capability values.
- Runner capability discovery returns stable identity, execution-platform
  contract, toolchain roots, and matching command grammar identity.
- `Rustc::acquire`, `rustc -vV`, host triple discovery, and PATH lookup disappear
  from recipe evaluation.
- Materializable toolchains—including Rust and first-party-distributed
  gcc/clang—are content-addressed inputs mounted immutably from CAS. They are
  never watched; mutation would violate the Store identity contract rather than
  describe a capability update.
- Ambient residue—including Xcode, MSVC, platform SDKs, and legally or
  technically non-materializable integration—is daemon-fingerprinted and
  continuously watched. Mutation poisons every in-flight execution that used
  the advertised fingerprint.
- Build scripts receive only explicitly declared capabilities and trap on every
  undeclared ambient tool.

**Gate:** identical materialized inputs or ambient capability fingerprints share
work; changed identity invalidates; ambient mutation during execution poisons;
materialized CAS roots require no watch; wrong platform fails before spawn;
missing/wrong first-party `cc`, ambient platform integration, linker, and rustc
requirements are typed negatives; the receipt names every external input used
and its trust class.

**Forbidden shortcuts:** process-local handles as identity, PATH probing inside
the recipe, host-derived `Target::host()`, capability fingerprints computed
after execution starts, or ambient fallback when a capability is unavailable.

## FV-C1 — facet-core, reality edition

**Home:** Facet plus the open host runtime; Vixen may provide the selected
runner but no build semantics.

**Status:** certified host-trusting demo exists; blocked by `FV-D4` and
`FV-E3`.

**Outcome:** `facet-core --no-default-features` builds from a normal checkout
through the entire no-cheat production path.

**Gate conditions:**

- Start with an empty Vix Store/CAS.
- Root inputs are only the workspace VFS grant, registry capability, target,
  toolchain requirements, and build selection.
- Observe real crates.io sparse rows lazily; select against all available rows;
  acquire real `.crate` blobs; emit a closed recipe.
- Derive every Rust/build-script unit in Vix and execute only through the
  sandboxed VFS runner.
- Produce and receipt `libfacet_core.rlib` and `libfacet_core.rmeta`.
- Compare the exact resolved package graph, enabled features, dependency edges,
  semantic unit demands, and relevant artifact properties with Cargo invoked
  separately as oracle. Normalize away Cargo's internal duplication of
  check/build modes; separately prove Vix reuses one `rmeta` demand node across
  check/build/clippy selections.
- Repeat the locked build offline with zero registry observations and zero
  process executions.
- Add an irrelevant workspace file and prove zero process executions.
- Preserve relevant-source, manifest, registry-row, checksum, missing/wrong
  capability, undeclared read, and nondeterministic-output negatives.
- Pass native and `WEAVY_JIT=0`, focused and broad suites, strict Clippy, format,
  diff, and clean-status gates.

**Forbidden shortcuts:** prepared sparse text, fake fetch, Cargo cache as origin,
Cargo-fed graph, hand-authored expected unit graph, generated Vix source,
`RealProcessBackend`, or skipping the first empty-CAS network acquisition.

## FV-C2 — Cargo-domain and unit-graph completeness

**Home:** Facet plus the open host runtime.

**Status:** partial unit-graph adapter exists; blocked by `FV-C1`.

**Outcome:** Vix/Rodin represents the Cargo package identities and build units
required by real Rust workspaces without falling back to Cargo.

**Required scope:**

- Multiple simultaneously selected versions and exact source identity.
- Resolver-v2 feature behavior, optional dependencies, weak feature forwarding,
  and build/normal/dev feature separation.
- Target-specific dependencies and typed cfg facts.
- Proc-macro and build-dependency host units versus target units.
- `links` uniqueness and build-script metadata propagation.
- Registry, path, git, alternate-registry, replacement, and patch sources.
- Library, proc-macro, binary, test, example, and custom build targets as
  demanded selections.
- Rust-version, yanked, and prerelease policy.
- Complete rustc/linker/native-tool invocation roles needed by those units.

**Gate:** every supported Cargo-domain behavior has a minimized live-Cargo
differential fixture; every unsupported source or unit shape is a typed failure;
the accepted corpus compares exact selection, features, dependency edges,
commands, and outputs under a normalized semantic unit model—not sampled names
or one-to-one Cargo-internal node identity. Real representative packages
exercise proc macros, target cfg, native build scripts, and multiple-version
graphs through the sandbox runner. Check/build/clippy selections demonstrably
reuse one Vix `rmeta` demand node.

Byte-identical check-versus-build `rmeta` is the separate RDR/temporary-rustc-
fork track. It is not a condition for this graph-reuse milestone.

**Forbidden shortcuts:** collapsing Cargo package identity to name, selecting
one version per name when Cargo admits several, invoking Cargo for unsupported
subgraphs, recorded answers, or tests that accept a package-name superset.

## FV-C3 — The artifact carries its recipe

**Home:** Facet for recipe/envelope schemas and build stamping; host runtime for
replay; Vixen only for later publication.

**Status:** blocked by `FV-C2`.

**Outcome:** a produced artifact contains the complete typed recipe needed to
replay or upgrade it, and produces a detached content-addressed publication
envelope without assuming that a transparency service already exists.

**Required design:**

- Embed the original solve request, closed resolution, input/capability
  identities, command-grammar identities, and predecessor linkage.
- Define signature-region exclusion and a detached content-addressed
  publication envelope suitable for `FV-P1`.
- `vx rebuild <artifact>` extracts and replays the pinned recipe without a
  source checkout.
- `vx upgrade <artifact>` refreshes admitted observations, re-runs Rodin, shows
  a structural resolution diff, and emits a successor recipe.
- Restricted inputs remain named by identity and access scope, making partial
  reproducibility explicit.

**Gate:** rebuild from artifact plus reachable pins is bit-identical; offline
rebuild succeeds when all pins are local; missing authorization is explicit;
tampered recipe/signature/artifact fails; upgrade preserves predecessor
lineage and rebuilds only the changed graph region.

**Forbidden shortcuts:** embedding only a pointer to a mutable service,
embedding only package names/versions, making Cargo.lock the sole recipe, or
signing a self-referential byte range.

## FV-P1 — Provenance registry and overlay authority

**Home:** Facet for open schemas and verification; Vixen for the hosted
registry, transparency log, and organization policy.

**Status:** blocked by `FV-C3`.

**Outcome:** recipe publication, overlay composition, trusted publishing, and
advisory projection have one typed, auditable authority rather than being
implicit product promises.

**Required design:**

- Append artifact publication envelopes to a content-addressed transparency
  log and return inclusion evidence.
- Bind publishing identity through OIDC or an equally explicit trusted-
  publisher contract; distinguish author, builder, signer, and operator claims.
- Give overlays stable identity, authorization scope, predecessor/patch lineage,
  and explicit precedence in the Rodin-composed package universe.
- Project RustSec/OSV/GHSA-style advisories onto content identities while
  preserving version-range provenance and explicit backport/not-affected
  decisions.
- Keep public proof schemas and verification independent of Vixen's hosted
  implementation.

**Gate:** publish, retrieve, and independently verify a recipe envelope and log
inclusion; reject unauthorized publishers and overlays; resolve the same roots
under two authorized overlay compositions and show the structural provenance
diff; demonstrate an advisory range, its affected upstream content identity,
and the same package version with a distinct authorized patched/backported
identity marked unaffected without false equivalence.

**Forbidden shortcuts:** a mutable package table without append evidence,
overlay selection outside Rodin, organization policy hidden from receipts,
version-only advisory identity, or a verifier that must trust Vixen's database.

## FV-C4 — The no-cheat path is the product default

**Home:** Facet, open host runtime, and Vixen product integration.

**Status:** blocked by `FV-P1` and, per platform, its enforcement milestone.

**Outcome:** a normal user entrypoint builds the Facet workspace and other real
Rust workspaces through the production path by default. Demo backends cannot be
mistaken for product support.

**Required work:**

- Build the full Facet workspace, including proc macros and representative
  tests/check/clippy selections, from a VFS grant and real registry.
- Make check/build/clippy distinct demands over one graph rather than separate
  mutable target-directory modes.
- Expose receipts, structural resolution/build diffs, and cache explanations
  through the normal CLI/API observability surface.
- Delete obsolete solver-specific runtime hooks and generated/demo bridge code.
- Feature-gate or test-label `RealProcessBackend`, fixture registry/fetch, and
  Cargo oracle harnesses so they remain useful evidence without being eligible
  production backends.
- Require sandbox and VFS capability negotiation before a production exec.
- Bank a committee-reviewed cold-solve and warm-edit performance budget on real
  registry/workspace workloads. Measure both execution lanes and close any
  product-default violation in the Vix machine or kernel; a Rust solver escape
  hatch is not an admissible remedy.

**Terminal gate:** from a clean checkout and empty Store, the default command
performs real resolution/update, locks, builds Facet through sandboxed runners,
and emits recipe-carrying artifacts. A second machine can rebuild from the
artifact recipe and pins. Cargo agrees as oracle but is absent from every
production trace. Native/interpreter, macOS/Linux, and `FV-E2W` Windows evidence
are explicit; unsupported platforms fail before work starts. The banked cold
and warm performance budget passes on its named workloads, and the evidence
distinguishes memo/cutoff wins from recomputation that happens to return the
same result.

**Forbidden shortcuts:** a hidden demo flag required for the real path, silent
fallback to host-trusting exec or Cargo, fixture roots in production config,
claiming platform support without enforcement, or retaining two authorities for
resolution, memo validity, or receipts.

## Completion ledger

Update this table only after the milestone's committee plan review,
implementation gate, and committee diff review are all closed.

| ID | Name | State | Plan review | Implementation evidence | Diff review |
| --- | --- | --- | --- | --- | --- |
| `FV-00` | Preserve and integrate the proven baseline | complete | [`FV-00.md`](milestones/FV-00.md); architecture `50dd61c5-df0c-4bcb-8739-47f2b5f91ff7` and adversary `167c2441-b5e8-40a4-90ef-06a4392292fe` approved | Facet `8db42feb6ebfb8ed921bba41f9d3b81f43d980e8`; Vixen evidence `e8dce9fd63c6a72cf799471bc5c6be391a31f2c0`; exact remote tips verified | Same committee approved both immutable objects; no blocking findings |
| `FV-D1` | Observation is not fetch | partial substrate | — | — | — |
| `FV-D2` | The registry is a real universe | partial; blocked | — | — | — |
| `FV-D3` | Rodin demands package rows | partial; blocked | — | — | — |
| `FV-D3F` | Semantic cutoff reaches production Rodin | partial; blocked | — | — | — |
| `FV-D4` | Resolution becomes a replayable recipe | partial; blocked | — | — | — |
| `FV-E1` | One publishable runner protocol | partial | — | — | — |
| `FV-E2` | Mount, confine, observe | partial; blocked | — | — | — |
| `FV-E2W` | Enforced exec on Windows | partial; blocked | — | — | — |
| `FV-E3` | Capabilities, not PATH | partial; blocked | — | — | — |
| `FV-C1` | facet-core, reality edition | demo; blocked | — | — | — |
| `FV-C2` | Cargo-domain and unit-graph completeness | partial; blocked | — | — | — |
| `FV-C3` | The artifact carries its recipe | blocked | — | — | — |
| `FV-P1` | Provenance registry and overlay authority | blocked | — | — | — |
| `FV-C4` | The no-cheat path is the product default | blocked | — | — | — |

## Definition of a useful negative result

A milestone may end red without being failed work. A useful negative result is
a committed forward checkpoint containing:

- the exact production-path command and error;
- structured traces or mechanically readable evidence;
- the violated or missing type/runtime contract;
- proof that no fallback, fixture weakening, or bypass was introduced;
- the smallest next design question suitable for committee review.

What is not acceptable is converting a missing capability into a green demo by
moving the decision into Rust, Cargo, generated Vix source, a fake backend, or a
test-only cache.
