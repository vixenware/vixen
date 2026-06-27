+++
title = "Executor abilities"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-05-27"
+++

How work is routed across the fleet — what defines an executor's capacity to satisfy a build, how builds request what they need, and how policy gets expressed at the language level.

## Abilities are typed values defined by packages

Each ability domain is a struct declared by the package that owns the domain:

```vixen
// in @vixen/rust
struct RustToolchain {
    host: Triple,
    target: Triple,
    version: SemVer,
    provider: Provider,

    // rustc-specific surface
    rustc: ExecPath,
    cargo: ExecPath,

    // C compiler bundled with / paired-by-default-with this Rust toolchain
    // Used for FFI deps; can be overridden by a more constrained CToolchain request
    cc: ExecPath,
    ld: ExecPath,
    sysroot: TreePath,
}

// in @vixen/c
struct CToolchain {
    host: Triple,
    target: Triple,
    version: SemVer,
    provider: Provider,  // Apple, ZigCross, Gcc, Clang, Msvc, Mingw, ...

    cc: ExecPath,
    ld: ExecPath,
    ar: ExecPath,
    sysroot: TreePath,
}

// in @vixen/cuda
struct CudaCapable {
    compute_capability: SemVer,
    vram_gb: U64,
}
```

Per-language toolchain types are **coherent bundles** — everything you need for *this* language. `RustToolchain` exposes not just `rustc` but also `cc`, `ld`, `sysroot` (whatever the Rust toolchain pairs with for FFI deps). Most build code that wants to compile Rust *and* has C FFI deps doesn't reach for a separate `CToolchain`; it uses `rust.cc` directly. A user who specifically wants to *replace* the C compiler the Rust toolchain ships with reaches for a `CToolchain` constraint explicitly.

The package owns the type. The type is its contribution. New ability domains ship as packages — `@vixen/freebsd`, `@vixen/wasi`, whatever — and existing executors and builds keep working while new builds can match against them.

There is **no `ExecutorClass`-style stringly-typed selector**. That was the wrong concept; routing is type-driven, not name-driven. Build code does not pass an `executor:` kwarg to `exec`. See [Routing via predicate-identified values](#routing-via-predicate-identified-values-planner-aggregates-at-exec-sites).

## Builds request abilities at runtime

Build code actively requests abilities by applying `where (...)` directly to an ability type. The body is a boolean expression where free identifiers desugar to fields of the ability type (see [Vixen language → Constraint expressions](/vixen-language/#constraint-expressions)):

```vixen
let mac = MacToolchain where (xcode >= 14.0 && arch == arch_arm64)

let rust = RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")
```

`Type where (...)` is the single fleet-routing surface from user-Vixen — no `.request` verb, no separate intrinsic. It produces a **predicate-identified value**: a thunk whose identity is the constraint, not yet bound to a specific executor. The actual fleet routing happens lazily, when something downstream demands the value.

This is **active request**, not declarative lockfile pinning. The lockfile records what got satisfied during evaluation, for reproducibility; it doesn't pin upfront before the build code has a chance to express policy.

**Policy is user-Vixen code**, not framework configuration. Fallback ladders ("try this, else that"), conditional resolution based on args, multi-target fan-out — all expressed via ordinary pattern matching and control flow.

The language ingredients are already there: `Option`, pattern matching, imports, the `where (...)` constraint expressions (see [Vixen language → Constraint expressions](/vixen-language/#constraint-expressions)). The runtime exposes the fleet matching machinery; user code composes.

## Packages wrap; users override

For most user code, ability requests are not written directly. They live inside the package implementing the high-level operation. `@vixen/cargo::build_workspace` handles toolchain requests internally; the user's `main` is just:

```vixen
fn main(workspace: Tree) with { ..VixenMain } -> Tree (
    let { build_workspace } = import "vixen:cargo"
    build_workspace workspace
)
```

When the user *does* want to override — pin a Rust version, restrict to a specific provider, target something non-native — the package's high-level operation takes overrides as kwargs, structured as constraints:

```vixen
build_workspace workspace with {
    rust: RustToolchain where (version == 1.91),
    target: "aarch64-apple-darwin",
}
```

The override threads into the package's internal logic — `@vixen/cargo` uses the user's `rust` value where it would normally have built its own.

This gives the user surface a smooth gradient:

- **Tier 2 customer using `vx cargo` as a cache** — never writes `where`. The package handles everything.
- **Tier 3 customer replacing CI with Vixen workflows** — writes Vixen code that orchestrates builds, reaches for `where (...)` regularly to pin versions, choose targets, select providers, declare canonical defaults for their organization. The same `where` syntax scales from "letting the package handle it" up to detailed CI authoring.

The package author's job is to expose the override kwargs in a way that matches what users actually want to customize. `@vixen/cargo` exposes `rust`, `target`, `features`, `profile` — the things cargo users already think about. New packages establish their own override surfaces appropriate to their domain.

## Routing via predicate-identified values; planner aggregates at exec sites

Each `exec` aggregates the predicate-identified ability values flowing into it and asks the orchestrator for an executor satisfying the intersection of their constraints.

```vixen
let mac = MacToolchain where (xcode >= 14.0 && arch == arch_arm64)
let rust = RustToolchain where (version >= 1.91)

// `rust.rustc` is the command the RustToolchain capability exposes — grammar +
// binary + fingerprint + exemptions bundled (see [The exec primitive](/exec/)).
// `@[ … ]` has no grammar name; the leading ${rust.rustc} carries it. The exec
// consumes `rust` (via that command) and `mac` (via mac.linker), so its provenance
// names both abilities.
let bin = exec(@[ ${rust.rustc} -C linker=${mac.linker} -o out/app ${workspace "src/main.rs"} ])
    with { observer: (run) => run.ok.out.file("out/app") }
// Planner sees bin needs both mac and rust; picks one executor matching both constraints.
```

There is no `executor:` kwarg on `exec`. Routing follows the flow of predicate-identified values through the build graph. Each exec independently plans its joint resolution — **the same `mac` predicate-identified value, consumed in two execs with different additional requirements, may resolve to different executors per-exec.** The value's identity is its constraint, not the concrete executor.

For hermetic toolchains fetched from the store, the resulting value is provenance-free — any eligible executor in the fleet can run an exec consuming it. Routing variables (load, latency, cache warmth) are settled by the orchestrator using fleet-scheduling signals invisible to build code.

## No soft ranking at the language level

The language gives build code **one** routing knob: predicate match. "I need `MacToolchain` matching `xcode >= "14.0" && arch == arch_arm64`." Either an executor satisfies the predicate or it doesn't.

There is no `prefer:` kwarg, no cost function, no partial order over candidates. When multiple executors satisfy the predicate, the orchestrator picks among them using fleet-scheduling variables that build code never sees:

- Current load / capacity
- Latency from the calling daemon
- Cache warmth — whether this executor recently mounted the relevant chunks
- Geographic proximity

These are orchestrator concerns. Build code doesn't weigh "ambient 1.95 on a 2-core box vs 32-core needing setup" because that tradeoff doesn't exist in our model: OSS toolchains are always-hermetic from store; proprietary toolchains are either installed on a fleet box or they're not. The classic ranking problem dissolves.

## Cross-compilation: alternate providers as alternate abilities

A real Mac fleet executor and a Linux box with `cargo-zigbuild` can both, in some sense, "produce aarch64-apple-darwin artifacts." But the artifacts they produce are not interchangeable — different linker, possibly different SDK headers, ABI-equivalent only at the margins. They are **different abilities**, not different satisfactions of the same predicate.

Two ways to encode the distinction; not picked yet:

- **Distinct types.** `AppleMacToolchain` vs `ZigMacCross`. Build code asks for one.
- **One type with a provider discriminant.** `MacToolchain { provider: Apple | ZigCross, ... }`. Build code predicates `provider == Apple` for ABI-critical paths; `provider in (Apple, ZigCross)` for testing.

Policy belongs in the package. `@vixen/cargo` (or a `@vixen/cross-rust` helper) knows that "build for App Store submission" requires the Apple provider and "local cross-test on my Linux dev box" accepts either. Customer overrides via CLI flag if needed.

Same shape applies to Windows: real-MSVC fleet executor vs local `cargo-xwin`. Different abilities; package policy chooses per target.

## Continuous cross-target validation — the LSP killer feature

A Mac developer writing code in their editor can see Windows compile errors as they save — produced by a remote Vixen executor that holds warm graph state for their editing session. Cross-platform validation moves from "wait for CI in minutes" to "see in seconds while you type."

Architecture shape:

- The local `vx-daemon` (a long-lived process; see `vx-daemon` crate) coordinates per-target evaluation streams.
- **Per-target sticky session.** Each non-native target is bound to a specific remote executor for the duration of the editing session. That executor holds the parsed workspace, resolved dep graph, last-compile state. A save sends the diff; the executor incrementally recomputes; diagnostics stream back.
- **Multi-stream evaluation.** Local-native + remote-Windows + remote-Linux all evaluate concurrently against the same edit stream. Each produces its own coherent stream of diagnostics; per-target IDE display.
- **Per-unit diagnostic caching.** Changing one file invalidates only its compilation unit's cached diagnostic; sibling units served from cache.

This makes `exec` observers returning `Stream[Diagnostic]` load-bearing — see [Streams as first Tier-2 primitive](/streams-first-primitive/). The streams Task is foundational for this product feature, not a Tier-2 nicety.

Single editing session can mix local + remote evaluation streams freely. The "no mixing local + remote artifacts" rule applies to **artifact production**, not editor-time validation: each target's stream produces its own artifact (or its own set of diagnostics) coherently within itself.

## Three separable layers

| Layer | What | Where it lives | What changes it |
|---|---|---|---|
| **Abilities** | Typed values describing what an executor can do | Build lockfile (output) + fleet registry | Discovery probes, package definitions |
| **Identity** | UUID, endpoint, signing key | Fleet registry | Executor registration, key rotation |
| **Availability** | Current load, latency, cache warmth, online state | Orchestrator (runtime) | Constantly changing |

Build code touches only layer 1. The orchestrator threads layers 2 and 3 at routing time, after matching layer 1.

## How abilities are advertised

Each ability type ships in a **capability probe package** — a package whose manifest declares it provides a probe for a specific ability type.

Frontmatter at the top of the package's `.vx` file (see [Vixen language → Frontmatter](/vixen-language/#frontmatter)):
```styx
id "@vixen/mac"
version 1.0.0
description "macOS toolchain capability probe and types"

capability {
    advertises MacToolchain
}
```

`main.vx`:
```vixen
struct MacToolchain {
    xcode: SemVer,
    macos_sdk: SemVer,
    arch: Arch,
}

namespace MacToolchain {
    fn probe(host: Host) -> Option[MacToolchain] {
        // …
    }
}
```

The probe lives in the type's namespace **by convention**. The daemon reads `capability.advertises MacToolchain` and calls `MacToolchain.probe` without further configuration.

When a `vx-daemon` starts, it walks its configured probe list (a flat sequence in `daemon.styx`):

```styx
probes (
    @vixen/mac
    @vixen/c
    @vixen/rust
    @vixen/cuda
)
```

…loads each package, calls each probe with a `Host` capability, and publishes the `Some(T)` results to the fleet registry over Vox. Probes that don't apply on the host (a Mac probe on Linux) just return `None` and advertise nothing — no OS-filter metadata needed.

The orchestrator queries the registry by structural constraint — `MacToolchain where (xcode >= 14.0 && arch == arch_arm64)` — and receives the set of executor identities whose advertised values match. Strong typing on both sides; constraints are structured expressions the runtime extracts from closure IR (LINQ Expression Trees in shape), so the registry can index by them.

For customer-local builds, "advertised abilities" is what the customer's own `vx-daemon` published from its own probes at startup against its own host.

## Auto-detect vs opt-in

The advertised set on a customer machine is **not** "everything Vixen could possibly route through here." It's the union of two disciplined sources:

**Auto-detect (the happy path).** Probes detect what's natively installed on the host — `clang` via Xcode Command Line Tools, `gcc` via the system package manager, `rustc` via rustup, MSVC via vswhere, etc. — and advertise those automatically. No user action required. If both `gcc` and `clang` are installed on a Linux box, the C probe advertises both as separate `CToolchain` instances; user code can constrain on `provider` if it cares, otherwise the scheduler picks.

**Opt-in (the weird stuff).** Cross-compile via `cargo-zigbuild`, cross-compile via `cargo-xwin`, any toolchain that requires Vixen to pull artifacts from the store and thread them through the sandbox/VFS — these don't auto-advertise. The user opts in explicitly via a per-package CLI command (see below).

This is a hard line. Customer local daemons don't claim "I can do everything in the public store" by default — they advertise what's actually set up on the machine. The fleet is broad-by-default (Vixen ops curate it); customer machines are minimal-by-default.

Vixen fleet daemons follow the opposite convention: pre-curated broad set, managed centrally by Vixen ops. A fleet Mac box advertises every macOS / iOS toolchain combination Vixen has licensed; a fleet Linux box advertises the standard cross-target set. Customer machines don't try to mirror this.

## Failure paths guide the user

When a request can't be satisfied, the package owning the capability produces a structured error pointing at the fix. Two categories:

**1. Install the tool.** For native capabilities the system *should* have but doesn't.

```
error: no C toolchain detected

Install one with:
  macOS:   xcode-select --install
  Debian:  apt install build-essential
  Fedora:  dnf groupinstall "Development Tools"
```

User installs; the daemon's next probe cycle (or the manual `vx daemon refresh`) picks it up; build proceeds.

**2. Enable via Vixen.** For non-native / store-fetched capabilities.

```
error: no toolchain for target aarch64-unknown-linux-gnu from host aarch64-apple-darwin

To cross-compile from this Mac, enable one of:
  vx rust enable-zigbuild   (local, pulls cargo-zigbuild from Vixen store)
  vx connect-fleet          (route to Vixen-cloud Linux executors)
```

`vx <pkg> enable-...` is a per-package CLI namespace. Each capability probe package defines its own enable subcommands; running one fetches the necessary tooling from the Vixen store and adds the relevant probe / advertisement source to the daemon's config. After `enable`, the capability auto-advertises on subsequent builds.

The package owns both the failure message and the enable UX. `@vixen/rust` decides what `vx rust enable-...` subcommands exist and what each does. `@vixen/c` may offer `enable-clang` / `enable-gcc` separately if the user wants something other than the system default.

This unifies the model: **every cross-host or non-native capability is opt-in by daemon config, discovered by the package's failure path, enabled per-package**. Native installed tools auto-detect; weird stuff opts in; the failure path documents itself.

## Refresh discipline

Layered cadence:

- **On executor startup.** Mandatory. Sets the initial published set.
- **On host-state-change triggers.** Filesystem watches on probe-inspected paths; install hooks from package managers where available.
- **Long periodic timer** (hourly, configurable). Backstop.
- **Not on every dispatch.** Too expensive. Dispatch trusts the last-published value; verification-races-with-work catches drift.

The build's lockfile can declare a per-ability-type max-staleness tolerance for stricter runs. Default: trust last-published as of dispatch.

## Who decides the opt-in set

- **Package authors own the ability *types*.** `@vixen/mac` defines what "having a Mac toolchain" means; ships the probe.
- **Executor operators own the *domains* their executor advertises.** A fleet operator (Vixen-the-org on its own fleet) opts each box into the ability domains it can satisfy: "this box runs `@vixen/mac` probes; it does not run `@vixen/cuda` probes." Explicit is safer than inferred.
- **Build authors own the *requests*.** Build code says "I need `MacToolchain` matching this constraint" via `MacToolchain where (...)`. That's the build-side surface.

New ability domains ship as packages with no changes needed at the registry, the orchestrator, or existing executors.

## Customer-local: the one place ambient discovery is forced

Most of the fleet runs hermetic toolchains from the public Vixen store, or proprietary toolchains Vixen installed under its own licenses. Discovery against the host is mostly vestigial for fleet boxes — declared identity collapses the read-set (see [Cache and trust](/cache-and-trust/#declared-identity-collapses-the-read-set-under-trust)).

Customer-local builds are different. The customer is the licensed end-user on their own machine. For proprietary toolchains (Xcode, MSVC), Vixen cannot ship the bytes to them; the customer's local install — or absence — is the only source. Discovery + obligations + verification-races-with-work is the mechanism that makes reuse safe across heterogeneous customer machines.

For OSS toolchains, customer-local can fetch from the public store (network available) or use any locally-cached chunks. Ambient discovery for OSS on customer machines is *available* but typically not preferred — hermetic from store is faster and more reproducible.

## Mid-build verification and ability drift

The orchestrator picks an executor at routing time based on its *advertised* abilities. Between the last probe that produced those values and the build that uses them, the host could have changed (someone updated Xcode, swapped a linker, edited a sysroot file). The advertised value is therefore a hypothesis until verified.

A verifier runs alongside the build, re-probing the abilities the build depends on. Compare against the request predicate (not against the advertised value — what matters is satisfaction of the build's stated requirement). Three cases:

- **Out-of-predicate drift, or original misadvertisement.** The executor was routed against a claim it doesn't honour — the actual re-probed value of ability `A` on executor `E` doesn't satisfy the predicate. Mark `(E, A)` stale at the fleet registry. Force a re-probe of `A` on `E`. Throw away in-flight work on this build that used `(E, A)`; re-route those units to executors whose `A` is still current and satisfies the predicate. `E` remains eligible for any other ability domain it advertises — only the specific `(E, A)` is invalidated.
- **Within-predicate drift, multi-executor build.** Re-probed value differs from advertised but still satisfies the predicate (advertised was 14.2, actual is 14.3, requirement was `>= 14.0`). Coherence is the concern: if some units committed against 14.2 and others against 14.3, link-time incompatibility can result. Mark `(E, A)` stale, throw away work that used `(E, A)`, re-route affected units. Other abilities of `E` continue serving — those values weren't disturbed.
- **Within-predicate drift, single-executor build.** Take notes. Per-unit cache keys reflect what was actually used; cross-unit coherence has the same risk as any mid-build host mutation on a local machine, with the linker as the last line of defence. No fleet ceremony.

The per-ability granularity matters. An executor advertising `MacToolchain`, `LinuxToolchain`, and `CudaCapable` doesn't get blacklisted when one of those drifts; only the drifted ability is marked stale.

If marking `(E, A)` stale leaves the build with no eligible executors for ability `A`, the build fails. Successful builds report drift events at the end as an informational signal so operators can investigate.

## What the lockfile records

A typed map keyed by some build-internal name, with each value being a typed ability instance — the **resolved** abilities, what got satisfied during this build's evaluation:

```
[abilities]
mac_target = MacToolchain {
  xcode = "14.2.0",
  macos_sdk = "14.4",
  arch = arch_arm64,
  rustc = ...,
}
linux_target = LinuxToolchain {
  glibc = "2.35",
  rustc = ...,
  arch = arch_x86_64,
}
```

The lockfile is an **output** of resolution, not a hand-authored pinning file — see [Build and resolution → lockfiles-as-outputs](/build-and-resolution/#lockfiles-are-build-outputs-not-input-mutations). Re-running the same build code against the same registry view + same probes produces the same lockfile.

Exact serialisation surface TBD (probably styx). The shape is "Vixen values written to a file"; the type system is the schema.

## Related

- [Architecture and infrastructure](/architecture-and-infrastructure/) — the trusted-builder fleet shape this model generalises.
- [Build and resolution](/build-and-resolution/) — discovery + obligations mechanism; lockfile-as-output framing.
- [Cache and trust](/cache-and-trust/) — declared-identity, verification-races-with-work, public vs private store partition, asymmetric infra.
- [Vixen language](/vixen-language/) — `Option`, pattern matching, imports, and the demand-driven evaluator that ties together user-Vixen policy code.
- [Design the executor scheduler](/design-executor-scheduler/) — the remaining Task: orchestrator routing logic given the ability model.
- [Streams as first Tier-2 primitive](/streams-first-primitive/) — load-bearing for continuous cross-target validation.
- [Epic: vx cargo build, end-to-end](/vx-cargo-build-trace/) — the trace this fits into.
