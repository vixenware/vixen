# Runners, capabilities, and the control plane — working notes

**Status: working notes from a design session. Non-normative.** Records the
*operational* architecture behind `exec` routing — runners as daemons, capability
detection, advertisement to a control plane, and CLI-driven placement. The
*language surface* for this already exists in the normative spec (below); the
*system that makes it real* did not, until this note.

Cross-refs: [`spec/language/core.md`](../content/spec/language/core.md)
(`capability { advertises T }`, `r[constraint.requirement]`),
[`spec/stdlib/build.md`](../content/spec/stdlib/build.md) (`r[exec.signature]`,
`r[exec.exemptions]`, `r[exec.verifier.host.coarse]`),
[`spec/language/build.md`](../content/spec/language/build.md)
(`r[eval.memoization]` — type-driven routing),
[`runtime-semantics.md`](runtime-semantics.md) (demand model, exemptions,
fingerprints, two-level cache).

---

## 0. The gap this fills

The spec already says: a package may `advertise` a capability; a `Requirement[T]`
(`T where (predicate)`) is "a routable demand that a resolver satisfies by
selecting a concrete `T`"; an `exec` is shipped to "an executor — a sandboxed
runner"; routing is type-driven with "no stringly-typed executor selector."

What the spec does **not** say, and this note records:

- runners are **long-lived daemons**, not per-build spawns;
- they **detect what their host has installed**, continuously;
- they **advertise** those capabilities to a **control plane**;
- the **`vx` CLI connects to the control plane** and that is where **placement**
  (which runner each `exec` lands on) is decided.

This also supersedes the old `discover` intrinsic (§5).

---

## 1. Runners are daemons

A **VxDaemon** runs on each host/machine in the fleet. It *is* the executor that
`exec` ships payloads to (`r[exec.signature]`): it owns the sandbox + VFS, runs
the process, evaluates the observer locally, rechecks host obligations on the
concrete runner, and returns only the observer's produced values. It is
**long-lived** — it persists across builds, which is what makes the next two
sections possible.

## 2. Capabilities are detected continuously, host-side

A daemon **detects what its host has installed** — C/Rust toolchains, SDKs, etc.
Detection is not inline in a build script; it runs **host-side, as
capability-advertising packages on the daemon**. This is the operational form of
the language's `capability { advertises T }`: a package whose job is to probe the
host and advertise the concrete ability values it finds.

Detection is **standing**, not one-shot. A daemon may:
- **watch** filesystem events on the paths it cares about (notice that Xcode's
  version changed under `/Applications/Xcode.app`), or
- **poll**, or
- some hybrid.

Mechanism is open. The invariant is that an advertised capability stays current
with the host — because that currency is load-bearing for cache correctness (§5).

## 3. Advertisement to a control plane

Each daemon **advertises its detected capabilities to a control plane** — the
fleet-wide registry of *what can run where*. The control plane holds the live set
of `(runner, advertised capabilities)`.

## 4. The CLI connects; placement is a predicate match

The **`vx` CLI connects to the control plane** to submit a build and to decide
**placement**. A build expresses requirements as `Requirement[T]`
(`r[constraint.requirement]`), e.g.:

```vixen
RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")
```

Placement is **matching that predicate against advertised capabilities** to select
a runner. This is exactly `r[eval.memoization]`'s "type-driven routing, no
stringly-typed executor selector," made operational: the ability value's predicate
picks the runner; nobody names an "executor class" by string.

## 5. Two toolchain flavors

How a toolchain reaches an `exec` is a policy choice with two shapes:

- **Hermetic** — specified, fetched (`acquire`), mounted as content (`mounts`).
  Identity = content hash. Reproducible and host-independent; you ship it.
- **Host-present** — already installed on the runner; made available through
  **exemptions** (`r[exec.exemptions]`). Identity = the daemon's advertised
  **fingerprint** (rechecked on the runner). You don't ship it; you depend on the
  host having it.

Pick hermetic for reproducibility/portability, host-present for large stable
toolchains you don't want to transfer.

### Capabilities *produce* the fingerprints (the cache connection)

A host-present toolchain's capability advertisement **is** the fingerprint an
`exec` exemption carries (e.g. "clang 15.0.1 at `/usr/bin/clang`"). So:

> capability advertisement → fingerprint → exemption → L1 cache key.

The daemon's continuous detection (§2) is what keeps that fingerprint **fresh**:
when the host's clang changes, the daemon re-advertises, the fingerprint moves,
and the L1 key invalidates correctly. At `exec` time,
`r[exec.verifier.host.coarse]`'s recheck verifies the advertised fingerprint still
holds on the concrete runner — the backstop against a stale advertisement.

This closes the fingerprint loop **without** a `discover` intrinsic: the loop runs
through standing daemon advertisement, not inline build-graph probing.

## 6. `discover` is superseded

The old model: a build script had a `discover` section that probed the host, then
a series of `exec`s consumed the discovered facts. That is gone. Host facts now
come from **standing capability advertisement** (daemon-side, §2–3), and selection
is by **predicate** (§4), not inline discovery. "Discovery" still happens — but as
the daemon's continuous capability detection, not as a build-graph operation.

(The other original motivation for `discover` — cargo manifest vs lock — is
handled separately, as a build-output-overlaid-as-input plus overrides; see
[`runtime-semantics.md`](runtime-semantics.md).)

## 7. Open questions

- **Detection mechanism** per capability: watch vs poll vs hybrid; latency bounds
  on advertisement freshness.
- **Control-plane protocol** — presumably vox RPC; the shape of the
  `(runner, capabilities)` registry and its update/subscription model.
- **Staleness / trust** — how stale an advertisement may be before a build refuses
  it; recheck-on-runner is the correctness backstop, but the placement decision
  uses possibly-stale advertised data.
- **CLI ↔ control plane** — connection, auth, and whether the CLI talks to one
  control plane or a federation.
- **Hermetic vs host-present** — how the choice is expressed in a build and who
  makes it (author policy vs runtime).
- **Relationship to packages** — a capability-advertising package runs *on* a
  daemon to do detection; how that package is deployed/trusted on runners.
