# Capabilities - Recovered Design Notes

Status: recovered design archaeology, 2026-07-08.

This document records design material recovered from git history about Vixen
capabilities, ambient toolchains, toolchain fingerprints, runner advertisements,
watch/poison behavior, and the materialized-vs-ambient boundary. It is not a
new spec and does not resurrect deleted implementation code. Citations name the
commit, date, and path where the design evidence was found.

The search was read-only over all refs after refreshing remotes. It included
deleted files and deleted daemon/runner crates, commit messages and diffs for
`capability`, `fingerprint`, `toolchain`, `xcode`, `msvc`, `inotify`,
`fsevents`, `poison`, `watch`, `ambient`, `advertise`, and `daemon`.

The companion note for comparison is
`docs/design/capabilities-ambient-vs-materialized.md`, committed as `53cc6c86`
on 2026-07-08. That note is today's restatement, not the historical baseline.

## 1. Complete Map

### 1.1 Materializable toolchains came first

The oldest substantial thread is December 2025 hermetic toolchains. It treated
toolchains as CAS-addressed build inputs and made the build graph refer to a
toolchain identity instead of the host `PATH`.

The deleted `internal/HERMETIC_TOOLCHAINS.md` says:

> "Toolchains as first-class CAS artifacts." (`96e393a2`, 2025-12-21,
> `internal/HERMETIC_TOOLCHAINS.md`)

and immediately gives the identity rule:

> `ToolchainId = blake3(ToolchainManifest + all file blobs)` (`96e393a2`,
> 2025-12-21, `internal/HERMETIC_TOOLCHAINS.md`)

The same document then narrowed v0 C/C++ to Zig:

> `ToolchainId = hash(zig_binary + zig_lib_directory)` (`96e393a2`,
> 2025-12-21, `internal/HERMETIC_TOOLCHAINS.md`)

The deleted Rust/Zig toolchain crates encode the same idea as types and
doc-comments. `RustToolchainId` was derived from Rust's published manifest
SHA256s, with download verification separate from identity; Zig's ID was built
from the executable hash plus deterministic `lib/` tarball hash (`41339adc`,
2025-12-21, `crates/vx-toolchain/src/lib.rs`; `c9ac929c`, 2025-12-21,
`crates/vx-toolchain/src/zig.rs`).

Recovered intent: rustc-class and Zig-class toolchains were meant to be
content-addressed, acquired, cached, materialized locally, and mounted/run
without depending on ambient host state.

### 1.2 C/C++ design already distinguished declared tool identity from discovered deps

The C build design answers kept `toolchain_id` as a real query input and used
depfiles for header discovery:

> "Discovered inputs are an output." (`a533060a`, 2025-12-21,
> `internal/C_BUILD_DESIGN_ANSWERS.md`)

> "`toolchain_id` is a query input, not just cache key component"
> (`a533060a`, 2025-12-21, `internal/C_BUILD_DESIGN_ANSWERS.md`)

It also described a local materialization cache:

> "Perfectly consistent with CAS--just a local materialized view."
> (`a533060a`, 2025-12-21, `internal/C_BUILD_DESIGN_ANSWERS.md`)

Recovered intent: toolchain identity was not a cosmetic cache-key salt. It was
part of planning, execution, and materialization.

### 1.3 Ambient Apple support started as a weak version fingerprint

By February 2026, the system had a pragmatic macOS concession: discover the
installed CommandLineTools/Xcode version and include it in C/C++ cache keys. The
deleted module doc says it:

> "discovers the installed CommandLineTools/Xcode version on macOS"
> (`1a708c13`, 2026-02-10, `crates/vx-toolchain/src/macos.rs`)

and that the version was:

> "used as part of cache keys for C/C++ compilation" (`1a708c13`,
> 2026-02-10, `crates/vx-toolchain/src/macos.rs`)

The commit message also recorded the limitation: it trusted version strings
rather than hashing the entire toolchain, so a corrupted or patched install with
the same version could still slip through (`1a708c13`, 2026-02-10).

Recovered intent: this was a bridge from full hermeticity toward ambient
identity, but not the final ambient design. It was a best-effort fingerprint,
not a whole-tree watch or structured obligation model.

### 1.4 May 2026 introduced typed abilities, probes, obligations, and drift handling

The richest pre-deletion prose is the May 2026 `executor-abilities` and stdlib
spec material. Abilities were typed package-owned values:

> "The package owns the type. The type is its contribution."
> (`e59d4cad`, 2026-05-27, `wiki/content/executor-abilities.md`)

Executors ran package-owned probes and advertised the resulting typed values:

> "The executor publishes `{identity, [typed ability values]}` to the fleet registry over Vox."
> (`e59d4cad`, 2026-05-27, `wiki/content/executor-abilities.md`)

The spec later removed user-callable `discover` and made toolchain discovery a
daemon/probe concern:

> "Toolchains that cannot be acquired with a digest -- non-redistributable ones (MSVC, Xcode) or whatever is installed on an executor host -- are found by **capability probes**."
> (`1a5c2589`, 2026-05-28, `docs/content/spec/stdlib/build.md`)

The obligation model is the historical core of ambient fingerprinting:

> "Those observations become verification obligations attached to the values that flow out of the probe"
> (`1a5c2589`, 2026-05-28, `docs/content/spec/stdlib/build.md`)

and:

> "the runtime rechecks the relevant obligations on the actual runner at every `exec`"
> (`1a5c2589`, 2026-05-28, `docs/content/spec/stdlib/build.md`)

Recovered intent: capabilities were not mere labels on executors. They were
typed values with attached host-observation obligations, flowing through the
program and forcing routing and verification.

### 1.5 Runner/control-plane notes made advertisement the operational model

The runner/control-plane design note made the daemon loop explicit:

> "runners are **long-lived daemons**, not per-build spawns" (`7e3df8cd`,
> 2026-05-30, `docs/design/runners-and-control-plane.md`)

> "they **detect what their host has installed**, continuously" (`7e3df8cd`,
> 2026-05-30, `docs/design/runners-and-control-plane.md`)

> "they **advertise** those capabilities to a **control plane**" (`7e3df8cd`,
> 2026-05-30, `docs/design/runners-and-control-plane.md`)

That note also states the cache connection as a pipeline:

> "capability advertisement -> fingerprint -> exemption -> L1 cache key."
> (`7e3df8cd`, 2026-05-30, `docs/design/runners-and-control-plane.md`)

Recovered intent: runner capability advertisement was the replacement for inline
host discovery. Build placement matched typed requirements against a live
registry of advertised capabilities.

### 1.6 Runtime semantics split tracked mounts from fingerprinted mounts

`runtime-semantics.md` gives the clearest vocabulary for the two classes of
host input:

> "**tracked mount** -- the spec's `mounts` (`r[exec.arg]`): fine-grained VFS content (`Tree`/`Blob`)."
> (`fb68edab`, 2026-05-30, `docs/design/runtime-semantics.md`)

> "**fingerprinted mount** -- the spec's `coarse_mounts` (`r[exec.coarse-mounts]`): a **host-backed sandbox exemption**, *not* content-addressed and *outside* the VFS, so its reads cannot be tracked."
> (`fb68edab`, 2026-05-30, `docs/design/runtime-semantics.md`)

It also defined fingerprint:

> "a cheap, reliable identity for an input that is expensive to walk but rarely changes"
> (`fb68edab`, 2026-05-30, `docs/design/runtime-semantics.md`)

Recovered intent: ambient toolchains were a deliberate untracked mode, not a
failed tracked mount. They belonged in the L1 key by fingerprint/obligation;
they did not participate in read-set refinement.

### 1.7 July 2026 recorded the exact watch/poison phrase Amos remembered

The July Vix design notes include the all-caps daemon responsibility:

> "the daemon is RESPONSIBLE for advertised invariance -- e.g. fs-watches every file of an MSVC install and POISONS in-flight builds using that capability if anything changes"
> (`92b859ad`, 2026-07-07, `docs/design/vix-language-design.md`)

The synthesis vocabulary repeats it:

> "Advertises capabilities, routes jobs, and ENFORCES advertised invariance"
> (`92b859ad`, 2026-07-07, `docs/design/vix-synthesis.md`)

and defines poisoning:

> "daemon-initiated invalidation of in-flight work whose capability changed underneath it."
> (`92b859ad`, 2026-07-07, `docs/design/vix-synthesis.md`)

Today's short note makes the OS watcher names explicit:

> "(FSEvents/inotify) the entire toolchain tree, and POISONS ANY IN-FLIGHT BUILD"
> (`53cc6c86`, 2026-07-08,
> `docs/design/capabilities-ambient-vs-materialized.md`)

Recovered intent: the exact whole-install MSVC watch + poison invariant exists
in July design prose. Earlier history had filesystem-watch/poll mechanisms and
verification-race poison, but not the explicit FSEvents/inotify whole-tree
contract except in the 2026-07-08 restatement.

## 2. Deep Dives

### 2.1 Fingerprint and toolchain identity schema

The historical schema splits into four levels:

1. **Rust materialized identity.** `RustToolchainId` is derived from manifest
   SHA256 strings for `rustc` and `rust-std`, with a domain separator
   `rust-toolchain-v1`. The comments emphasize that identity is defined by what
   Rust publishes, while corrupted downloads fail verification rather than
   creating a new identity (`41339adc`, 2025-12-21,
   `crates/vx-toolchain/src/lib.rs`).

2. **Zig materialized identity.** `ZigToolchain` stores `version`, `host`,
   `zig_exe_hash`, and `lib_hash`; `ToolchainId::from_parts` hashes executable
   and lib hashes. The lib directory hash is generated from a deterministic tar:
   sorted entries, fixed mtime/uid/gid, and consistent permissions (`c9ac929c`,
   2025-12-21, `crates/vx-toolchain/src/zig.rs`).

3. **Apple weak ambient identity.** `CommandLineToolsVersion` carries a version
   and optional build string, formatting them into a cache-key string. This was
   explicitly weaker than full tree hashing (`1a708c13`, 2026-02-10,
   `crates/vx-toolchain/src/macos.rs`).

4. **Capability/obligation identity.** Probes record host facts such as
   `host.which`, `host.read`, `host.hash`, and `host.exec`; those observations
   attach to the returned capability value and are rechecked at every exec
   (`1a5c2589`, 2026-05-28, `docs/content/spec/stdlib/build.md`). Deleted runner
   protocol code exposed `verification_obligations: Vec<HostObservation>` and
   `HostObservation::Which { name, expected_path }` shapes (`110a97ed`,
   2026-03-29, `crates/vx-runner-proto/src/lib.rs`).

There is no single historical `ToolchainFingerprint` schema that settled all
toolchains. Instead, the design converged from concrete IDs for materializable
Rust/Zig toward typed capability values carrying observation obligations for
ambient toolchains.

### 2.2 Watch and poison contract

The contract has three historical layers:

1. **Verification races with work.** The build-and-resolution note says that
   reused obligations are a bet, then verification runs concurrently with the
   build. If verification disagrees before commit, the build is poisoned and
   redispatched under corrected obligations (`1a5c2589`, 2026-05-28,
   `wiki/content/build-and-resolution.md`).

2. **Daemon refresh discipline.** Executor abilities are refreshed at startup,
   on filesystem watches for probe-inspected paths or package-manager hooks, and
   on a periodic timer. Dispatch trusts last-published values; verification
   catches drift (`e59d4cad`, 2026-05-27,
   `wiki/content/executor-abilities.md`).

3. **Advertise -> watch -> poison.** The July synthesis states the invariant
   directly:

   > "Capability invariance is daemon-enforced (advertise => watch => poison)."
   > (`92b859ad`, 2026-07-07, `docs/design/vix-synthesis.md`)

The older notes talk about filesystem watches and polling, but they leave the
exact watcher mechanism open. Historical mentions of `FSEvents` and `inotify`
before July mostly concern source-file watching or generic old design docs, not
ambient toolchain whole-tree watching. The exact FSEvents/inotify tree-watch
language is in today's restatement (`53cc6c86`, 2026-07-08).

Recovered intent: poison is not "build failed." It is a correctness fence for
in-flight work whose advertised capability/fingerprint no longer describes the
host state it is using.

### 2.3 Materializability boundary: where history agrees and diverges

The history strongly confirms the rustc-class side of today's restatement.
Rust and Zig were repeatedly designed as content-addressed artifacts that can be
downloaded, verified, stored in CAS, materialized locally, and mounted/run by
executors (`96e393a2`, 2025-12-21, `internal/HERMETIC_TOOLCHAINS.md`;
`41339adc`, 2025-12-21, `crates/vx-toolchain/src/lib.rs`; `c9ac929c`,
2025-12-21, `crates/vx-toolchain/src/zig.rs`).

The proprietary side is more nuanced historically than today's short note.
`executor-abilities.md` says customer-local proprietary builds are forced to use
the customer's install:

> "For proprietary toolchains (Xcode, MSVC), Vixen cannot ship the bytes to them"
> (`e59d4cad`, 2026-05-27, `wiki/content/executor-abilities.md`)

But `cache-and-trust.md` also contemplated a Vixen-owned private store for
licensed proprietary toolchains:

> "They don't prohibit Vixen storing its own licensed copy in a private CAS and mounting via VFS on its own hardware"
> (`1a5c2589`, 2026-05-28, `wiki/content/cache-and-trust.md`)

and explicitly left the technical question open:

> "Whether Xcode and MSVC actually function correctly served from chunked CAS via VFS is empirically unknown"
> (`1a5c2589`, 2026-05-28, `wiki/content/cache-and-trust.md`)

That differs from today's sharper phrase "technically and LEGALLY impossible to
materialize" for Xcode/MSVC-class ambient toolchains (`53cc6c86`, 2026-07-08,
`docs/design/capabilities-ambient-vs-materialized.md`).

Recovered intent:

- For customer machines, history agrees with today's restatement: Xcode/MSVC
  bytes cannot be redistributed to the customer; the local licensed install is
  ambient and must be discovered, fingerprinted, watched, and poisoned on drift.
- For Vixen-owned fleet machines, history did not settle "legally impossible."
  It said redistribution to third parties was forbidden, private CAS under
  Vixen's own licenses looked legally acceptable, and technical viability of
  VFS-backed Xcode/MSVC was unknown.
- Today's restatement is therefore either a deliberate strengthening/superseding
  decision or a simplification that collapses the older customer-vs-fleet split.

### 2.4 Runner capability advertisement

The historical advertisement model is consistent across the May and July notes:

- A package owns an ability type and ships a probe (`e59d4cad`, 2026-05-27,
  `wiki/content/executor-abilities.md`).
- Executors run opted-in probes and publish typed ability values to a fleet
  registry over Vox (`e59d4cad`, 2026-05-27,
  `wiki/content/executor-abilities.md`).
- Build code requests requirements structurally, by type and predicate, not by
  string executor class (`e59d4cad`, 2026-05-27,
  `wiki/content/executor-abilities.md`; `7e3df8cd`, 2026-05-30,
  `docs/design/runners-and-control-plane.md`).
- Runner/control-plane notes replace inline `discover` with standing
  advertisement (`7e3df8cd`, 2026-05-30,
  `docs/design/runners-and-control-plane.md`).
- The 2026-07-08 package/provenance note refines ambient toolchains as runner
  advertised capabilities: the build declares its toolchain requirement, the
  runner advertises path plus probed identity, and the receipt records the
  capability identity (`3214c371`, 2026-07-08,
  `docs/design/package-manager-and-provenance.md`).

Deleted runner code briefly exposed this at the protocol boundary by reexporting
`RunnerCapabilities`, `SandboxCapability`, `ToolchainCapability`, and
`CapabilityProbe*` types (`4c2ad832`, 2026-07-07,
`crates/vx-services/src/runner.rs`). A later stage-1 protocol carried ambient
state as `toolchain_roots`:

> "Declared roots that may be executed or read as ambient toolchain state."
> (`17babfa4`, 2026-07-07, `crates/vx-services/src/runner.rs`)

Recovered intent: the runner advertises what it has; build artifacts and receipts
refer to the advertised capability identity; ambient reads are legitimate only
when routed through a declared capability.

### 2.5 Deleted implementation schemas that carry design knowledge

The deleted daemon/runner implementations were not the source of the strongest
legal prose, but they contain useful design-bearing schemas:

- `RunMountMode::{Tracked, Opaque}` separates fine-grained tracked VFS mounts
  from opaque ambient mounts (`110a97ed`, 2026-03-29,
  `crates/vx-runner-proto/src/lib.rs`).
- `RunEvalPayload` contains hidden discovery obligations to recheck on the
  executor (`110a97ed`, 2026-03-29,
  `crates/vx-runner-proto/src/lib.rs`).
- `RunTrackedObservation::{File, Directory, LookupMiss}` encodes file reads,
  directory listings, and negative lookups as first-class observations
  (`110a97ed`, 2026-03-29, `crates/vx-runner-proto/src/lib.rs`).
- Runner tests assert that different declared tool fingerprints do not alias and
  that changing executable content at one path does not alias (`52d58460`,
  2026-06-27, `crates/vx-runner/src/lib.rs`).

These are implementation-era evidence for the design, not files to resurrect.

## 3. Failure Modes / Dragons

- **Version-string fingerprints are too weak.** The February CommandLineTools
  cache-key fix was explicitly better than ignoring Apple toolchain version, but
  still vulnerable to a patched/corrupt install with the same version
  (`1a708c13`, 2026-02-10).

- **Ambient reads cannot be read-set refined.** Fingerprinted/coarse mounts are
  outside the VFS, so reads cannot be tracked. Cache correctness rests on
  fingerprint/obligation freshness, not L2 read-set validation (`fb68edab`,
  2026-05-30).

- **Stale advertisement creates routing lies.** The executor may have changed
  since its last probe. Historical design handles this by marking the specific
  `(executor, ability)` stale, forcing reprobe, throwing away affected in-flight
  work, and rerouting if possible (`e59d4cad`, 2026-05-27).

- **Within-predicate drift can still break coherence.** If a runner advertised
  Xcode 14.2 and actually has 14.3, a predicate `>= 14.0` still matches, but a
  multi-executor build could link units compiled against different SDK/toolchain
  details. History treats this as a coherence problem requiring stale-marking and
  discard/reroute for affected work (`e59d4cad`, 2026-05-27).

- **No eligible executor is a real failure.** If stale-marking removes the only
  runner satisfying an ability, the build fails rather than pretending a nearby
  capability is equivalent (`e59d4cad`, 2026-05-27).

- **Private CAS for Xcode/MSVC was technically unsettled.** The old fleet design
  lists code signing, `dyld`, framework lookup, and `mmap` on VFS-backed paths as
  empirical unknowns (`1a5c2589`, 2026-05-28,
  `wiki/content/cache-and-trust.md`).

- **Whole-tree watch mechanics were not fully designed.** Earlier notes allow
  watch/poll/hybrid and package-manager hooks. The exact "FSEvents/inotify the
  entire toolchain tree" contract appears in the 2026-07-08 restatement, while
  the July 7 notes only say `fs-watches` and the May notes leave mechanism open
  (`7e3df8cd`, 2026-05-30; `92b859ad`, 2026-07-07; `53cc6c86`,
  2026-07-08).

## 4. Tensions Against Today's Restatement

Today's note (`53cc6c86`, 2026-07-08,
`docs/design/capabilities-ambient-vs-materialized.md`) is mostly confirmed, but
the history adds nuance.

### Confirms

- Rust/Zig/rustc-class toolchains are materializable: content-addressed, stored
  in CAS, materialized locally, and mounted/run by executors.
- Customer-local Xcode/MSVC-class toolchains are ambient: Vixen cannot ship those
  proprietary bytes to the customer, so the customer's install must be discovered
  and verified.
- Runner-advertised capabilities are the right shape for ambient residues.
- Ambient identity is a posteriori: observed/probed, rechecked, and invalidated
  by drift.
- Poisoning in-flight work is the intended correctness fence when a capability
  mutates under a build.

### Extends

- Historical design had a typed ability model owned by packages, not just runner
  metadata.
- Capability values carry host-observation obligations, which flow through
  values and are rechecked at exec time.
- The runtime vocabulary distinguishes tracked mounts from fingerprinted mounts;
  only tracked mounts get read-set refinement.
- Command grammars were expected to ride with capability identity in July
  synthesis (`92b859ad`, 2026-07-07, `docs/design/vix-synthesis.md`).
- Receipts/lockfiles record resolved typed abilities, not just version strings
  (`e59d4cad`, 2026-05-27).

### Diverges

- Today's note says Xcode/MSVC-class toolchains are technically and legally
  impossible to materialize. Historical May design agreed for redistribution to
  customers, but left Vixen-owned private-CAS materialization legally acceptable
  and technically open. If today's note supersedes that, it should be treated as
  a new stronger decision, not something history had already settled.

- Today's note says entire toolchain trees are FS-watched with FSEvents/inotify.
  History has the invariant "advertise => watch => poison" and an MSVC whole-install
  example, but older runner/control-plane notes left watch vs poll vs hybrid open.
  FSEvents/inotify specifically appear in today's note and in older unrelated
  source-watching docs, not as a settled ambient-toolchain implementation plan.

### Simplifies or drops

- The weaker CommandLineTools version-fingerprint bridge is omitted. That is fine
  if it is obsolete, but it is useful history for why version-only fingerprints
  are insufficient.
- The customer-facing vs Vixen-fleet infrastructure split is collapsed.
- The typed package-owned probe/ability model is compressed into "runner
  advertises capabilities."
- The L1/L2 cache split and "fingerprinted mounts have no read-set" distinction
  are not visible in the short restatement.

## 5. Verbatim Quotes Worth Preserving

> "Toolchains as first-class CAS artifacts." (`96e393a2`, 2025-12-21,
> `internal/HERMETIC_TOOLCHAINS.md`)

> `ToolchainId = blake3(ToolchainManifest + all file blobs)` (`96e393a2`,
> 2025-12-21, `internal/HERMETIC_TOOLCHAINS.md`)

> Build nodes reference `ToolchainId`, not "whatever is on PATH".
> (`96e393a2`, 2025-12-21, `internal/HERMETIC_TOOLCHAINS.md`)

> "Discovered inputs are an output." (`a533060a`, 2025-12-21,
> `internal/C_BUILD_DESIGN_ANSWERS.md`)

> "toolchain_id is a query input, not just cache key component"
> (`a533060a`, 2025-12-21, `internal/C_BUILD_DESIGN_ANSWERS.md`)

> "The toolchain ID is deterministic and derived from the *manifest-declared*
> SHA256 hashes, NOT from the downloaded bytes." (`41339adc`, 2025-12-21,
> `crates/vx-toolchain/src/lib.rs`)

> "This replaces install.sh execution with direct file copying for hermeticity."
> (`41339adc`, 2025-12-21, `crates/vx-toolchain/src/lib.rs`)

> "discovers the installed CommandLineTools/Xcode version on macOS"
> (`1a708c13`, 2026-02-10, `crates/vx-toolchain/src/macos.rs`)

> "Discovery produces *obligations*, not fingerprints" (`1a5c2589`,
> 2026-05-28, `wiki/content/build-and-resolution.md`)

> "Verification races with the work. Whoever finishes first either commits or
> kills. There is no quiet wrong-bytes case." (`1a5c2589`, 2026-05-28,
> `wiki/content/build-and-resolution.md`)

> "The package owns the type. The type is its contribution."
> (`e59d4cad`, 2026-05-27, `wiki/content/executor-abilities.md`)

> "There is **no `ExecutorClass`-style stringly-typed selector**."
> (`e59d4cad`, 2026-05-27, `wiki/content/executor-abilities.md`)

> "For proprietary toolchains (Xcode, MSVC), Vixen cannot ship the bytes to
> them" (`e59d4cad`, 2026-05-27, `wiki/content/executor-abilities.md`)

> "The advertised value is therefore a hypothesis until verified."
> (`e59d4cad`, 2026-05-27, `wiki/content/executor-abilities.md`)

> "Only the specific `(E, A)` is invalidated." (`e59d4cad`, 2026-05-27,
> `wiki/content/executor-abilities.md`)

> "They don't prohibit Vixen storing its own licensed copy in a private CAS and
> mounting via VFS on its own hardware" (`1a5c2589`, 2026-05-28,
> `wiki/content/cache-and-trust.md`)

> "Whether Xcode and MSVC actually function correctly served from chunked CAS
> via VFS is empirically unknown" (`1a5c2589`, 2026-05-28,
> `wiki/content/cache-and-trust.md`)

> "capability advertisement -> fingerprint -> exemption -> L1 cache key."
> (`7e3df8cd`, 2026-05-30, `docs/design/runners-and-control-plane.md`)

> "fingerprinted mount" (`fb68edab`, 2026-05-30,
> `docs/design/runtime-semantics.md`)

> "Fingerprint -> L1 key; no read-set." (`fb68edab`, 2026-05-30,
> `docs/design/runtime-semantics.md`)

> "fs-watches every file of an MSVC install and POISONS in-flight builds"
> (`92b859ad`, 2026-07-07, `docs/design/vix-language-design.md`)

> "Capability invariance is daemon-enforced (advertise => watch => poison)."
> (`92b859ad`, 2026-07-07, `docs/design/vix-synthesis.md`)

> "fingerprint mutation between calls = poisoning, testable as memo misses."
> (`92b859ad`, 2026-07-07, `docs/design/vix-synthesis.md`)

> "runner advertises the toolchain (path + probed identity) in its capability
> set; the receipt records the capability identity." (`3214c371`, 2026-07-08,
> `docs/design/package-manager-and-provenance.md`)

> "mount-what-you-hashed vs fingerprint-what-you-must-trust-and-watch"
> (`53cc6c86`, 2026-07-08,
> `docs/design/capabilities-ambient-vs-materialized.md`)

## 6. What Was Thin

There was no single recovered grand schema named `ToolchainFingerprint` that
covered Rust, Zig, Xcode, MSVC, and arbitrary ambient tools. The history instead
shows several partial schemas converging:

- concrete CAS IDs for Rust and Zig;
- version/build string keys for Apple CommandLineTools;
- `HostObservation` obligations for ambient discovery;
- runner-advertised typed capabilities;
- `toolchain_roots` and opaque/fingerprinted mounts in runner protocol drafts.

There was also no deleted daemon implementation that fully realized
FSEvents/inotify whole-toolchain watching plus poison. The strongest
watch/poison evidence is design prose and protocol/test intent, not completed
deleted implementation.

That thinness matters: the recovered material confirms the shape of today's
memory, but the implementation details of whole-tree watcher scope, event
coalescing, race windows, per-OS APIs, and poison propagation still need fresh
design before they become spec.
