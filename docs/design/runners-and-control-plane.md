# Runners, capabilities, and the control plane — working notes

**Status: working notes from a design session. Non-normative.** Records the
*operational* architecture behind `exec` routing — runners as daemons, capability
detection, advertisement to a control plane, and CLI-driven placement. The
*language surface* for this already exists in the normative spec (below); the
*system that makes it real* did not, until this note.

**Scope:** this is **Vixen build's** control plane — *not* CCC (a cache-warming
job queue) and *not* CI runners. The existing app.vixen.rs code conflates the
three; see [`product-boundaries.md`](product-boundaries.md) before forming a model
from that code.

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
- there is **always a local daemon** on your own machine — it is the
  **orchestrator** (it makes placement decisions) and the **filesystem watcher**
  (it notices when files used by past builds change);
- executor daemons **detect what their host has installed**, continuously, and
  **advertise** those capabilities to a **control plane** — a registry/directory,
  authenticated;
- your local daemon **queries** that registry (and the LAN) for available
  executors and **places** each `exec` itself: the control plane advertises, your
  daemon decides.

This also supersedes the old `discover` intrinsic (§6).

---

## 1. Daemons: one orchestrator (always local) + executors

A **VxDaemon** is long-lived — it persists across builds, which is what makes
continuous capability detection (§2) and read-set watching possible. The same
program plays two roles:

- **Your local daemon — the orchestrator.** There is *always* a daemon on your own
  machine. It drives the build: it makes **placement** decisions (which executor
  each `exec` lands on) and it **watches your filesystem** so it knows when files
  consumed by previous builds have changed (read-set invalidation —
  [`runtime-semantics.md`](runtime-semantics.md) §6–7). There is **no daemon-less
  / "embedded" mode**; the orchestrator is always your local daemon.
- **Executor daemons.** A daemon on any host can *be an executor* — the thing
  `exec` ships payloads to (`r[exec.signature]`): it owns the sandbox + VFS, runs
  the process, evaluates the observer locally, rechecks host obligations, and
  returns only the observer's produced values. Your own machine's daemon can be an
  executor too; remote ones reach your orchestrator via the LAN or the cloud
  registry (§3).

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

## 3. Advertisement to a control plane (a registry)

An executor daemon **advertises its detected capabilities to a control plane** — a
**registry/directory** of *what can run where*, holding the live set of
`(executor, advertised capabilities)`. Advertising to a cloud control plane
requires **authentication** (§7). The control plane is a *directory*, not a
decision-maker: it answers "what executors exist and what can they do"; your local
orchestrator (§1) does the deciding.

## 4. Placement is the orchestrator's predicate match

The `vx` CLI talks to **your local daemon** (the orchestrator, §1), which **places**
each `exec`. A build expresses requirements as `Requirement[T]`
(`r[constraint.requirement]`), e.g.:

```vixen
RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")
```

The orchestrator gathers candidate executors — from the LAN and from the cloud
control-plane registry (§3) — and **matches that predicate against their advertised
capabilities** to select one. This is exactly `r[eval.memoization]`'s "type-driven
routing, no stringly-typed executor selector," made operational: the ability
value's predicate picks the executor; nobody names an "executor class" by string.
**The control plane supplies the candidates; the orchestrator makes the choice.**

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

## 7. Deployment: local always, cloud optional

The orchestrator is **always your local daemon** (§1). What varies is **where it
sources executors**:

- **Local-only** — your daemon places across executors it reaches on the **LAN**
  (including your own machine). No cloud.
- **Cloud-connected** — your daemon *also* queries a **cloud control plane**: a
  registry where remote executor daemons have **advertised** their capabilities
  (authenticated). Your daemon still orchestrates; the cloud just widens the pool.

There is **no third "embedded / no-daemon" mode** — the local daemon is always
present (it is also the filesystem watcher). And even cloud-connected, **the
decisions are yours**: the cloud control plane is a directory of executors + auth,
not an orchestrator.

**Vixen build must be fully usable with no commercial cloud.** The cloud is the
managed *registry* deployment; local-only is first-class.

### Identity is not (necessarily) GitHub — a real issue, to settle elsewhere

Authenticating to a cloud control plane needs an identity. **GitHub auth is one
option, not the foundation.** We want easy sign-up via GitHub or other **OIDC** —
but Vixen has a *real identity question of its own*, precisely because Vixen is
going **up against GitHub**. Just as a **Vixen organization is not necessarily a
GitHub organization**, **login is not necessarily through GitHub**. This needs
settling **sooner rather than later — but not in this session**: recorded here, to
be designed in a separate session with another agent. (See
[`product-boundaries.md`](product-boundaries.md): identity/orgs are app.vixen.rs's
shared layer, and they are *Vixen's*, not GitHub's.)

## 8. Open questions

- **Detection mechanism** per capability: watch vs poll vs hybrid; latency bounds
  on advertisement freshness.
- **Registry protocol** — vox RPC (confirmed). Open: the shape of the
  `(runner, capabilities)` registry and its update/subscription model, defined
  **once and deployment-agnostic** so local and cloud both speak it.
- **LAN discovery** — how your orchestrator finds LAN executors with no cloud
  endpoint: configured addresses, LAN discovery (mDNS / DNS-SD), etc.
- **Identity abstraction (protocol level)** — what credential the protocol carries
  so cloud auth and LAN trust both fit without the protocol knowing which is in use
  (the *broader* identity question is §7, to be settled separately).
- **Staleness / trust** — how stale an advertisement may be before a build refuses
  it; recheck-on-runner is the correctness backstop, but placement uses
  possibly-stale advertised data.
- **Hermetic vs host-present** — how the choice is expressed in a build and who
  makes it (author policy vs runtime).
- **Relationship to packages** — a capability-advertising package runs *on* a
  daemon to do detection; how that package is deployed/trusted on runners.
