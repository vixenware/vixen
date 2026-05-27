+++
title = "Executor abilities"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-05-26"
+++

How the runtime decides which executor a piece of work runs on, without caring about *which physical executor* — only about *what abilities it has*.

## Abilities, not identities

The build's lockfile pins **executor abilities** — typed values describing what an executor can satisfy — not specific executors. "I need a Mac box with Xcode 14.2, macOS SDK 14.4, arm64, rustc 1.91" is the durable thing the lockfile records. *Which* physical executor matches that on any given build is the orchestrator's problem.

Consequences:

- **Capacity scales transparently.** New executors come online matching your ability spec — they become routable as soon as they register. Nothing in the lockfile changes.
- **Cross-version dispatch doesn't happen.** The ability spec says "Xcode 14.2"; executors with 14.3 aren't eligible. The orchestrator picks among the eligible by latency / load. The "two executors with different Xcode versions" question reduces to "filter, then schedule," not "dispatch and hope the linker copes."
- **The poison-vs-take-notes question shrinks.** If the orchestrator only routes to executors *matching* the locked ability, mid-build obligation drift is rare — the executor was selected because its abilities matched at routing time. Verification-races-with-work catches the remaining "host changed mid-compile" failure mode.

## Abilities are typed Vixen values

Each ability domain is a struct declared by the package that owns the domain:

```vixen
// in @vixen/mac
struct MacToolchain {
  xcode: SemVer,
  macos_sdk: SemVer,
  arch: Arch,
  rustc: RustToolchain,
}

// in @vixen/linux
struct LinuxToolchain {
  glibc: SemVer,
  rustc: RustToolchain,
  arch: Arch,
}
```

The strong type system that the rest of Vixen is built on applies here too. Abilities are typed values, not stringly-keyed property bags. Different ability domains live in different types; orthogonal concerns can't accidentally merge into one untyped blob.

New ability domains ship as packages: `@vixen/cuda` declares `struct CudaCapable { compute_capability: SemVer, vram_gb: U64, ... }` and existing builds keep working while new builds can match against it.

## Three separable layers

| Layer            | What                                              | Where it lives                | What changes it    |
| ---------------- | ------------------------------------------------- | ----------------------------- | ------------------ |
| **Abilities**    | Typed values describing what an executor can do   | Build lockfile + fleet registry | Discovery probes, user lockfile updates |
| **Identity**     | UUID, endpoint, signing key                       | Fleet registry                | Executor registration, key rotation |
| **Availability** | Current load, latency-from-customer, online state | Orchestrator (runtime)        | Constantly changing |

The build cares only about layer 1. The orchestrator threads layers 2 and 3 at routing time, after matching layer 1.

## How abilities are advertised

Each ability type ships with a **probe** — Vixen code provided by the package that owns the type. `@vixen/mac` ships `fn probe_mac_toolchain (Host) #host -> MacToolchain`.

When an executor joins the fleet, it runs every probe whose ability type is opted-in for that host (see [Who decides the opt-in set](#who-decides-the-opt-in-set)). Each probe produces a typed value. The executor publishes `{identity, [typed ability values]}` to the fleet registry over Vox. Re-publishes when a value changes.

The orchestrator queries the registry by **ability predicate** expressed in Vixen — `MacToolchain.xcode >= "14.0" && MacToolchain.arch == arch_arm64` or similar — and receives back the set of executor identities whose advertised values match. Strong typing on both sides: typed values published; typed predicates matching; structural match.

## When abilities are refreshed

Layered cadence:

- **On executor startup.** Mandatory. Sets the initial published set.
- **On host-state-change triggers.** Filesystem watches on the paths the probes inspect (`/Applications/Xcode.app/Contents/version.plist`, `/usr/local/bin/clang`, etc.); install hooks from package managers where available. Fast detection of intentional changes.
- **Long periodic timer** (hourly, configurable). Backstop for missed triggers.
- **Not on every dispatch.** Too expensive — re-probing Xcode on every Mac dispatch eats the latency win we got from caching. Dispatch trusts the last-published value; the verification-races-with-work mechanism catches drift between dispatch and build commit.

The build's lockfile can declare a per-ability-type max-staleness tolerance for runs where stricter is required. Default: trust last-published as of dispatch.

## Who decides the opt-in set

Three responsibilities, three places. No coordination required between layers.

- **Package authors own the ability *types*.** Each domain package owns its types and ships the probes. `@vixen/mac` defines what "having a Mac toolchain" means.
- **Executor operators own the ability *domains* their executor advertises.** When standing up a fleet box, the operator opts it in to the ability domains it can satisfy: "this machine runs `@vixen/mac` probes; it does not run `@vixen/cuda` probes." Small config at the executor; possibly inferred from "what's installed" but explicit is safer.
- **Build authors own the ability *predicates* they require.** The build's lockfile declares "I need a `MacToolchain` matching this predicate." That's the customer's pinning surface.

New ability domains ship as packages with no changes needed at the registry, the orchestrator, or existing executors.

## What the lockfile records

A typed map keyed by some build-internal name, with each value being a typed ability instance:

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

Exact serialisation surface TBD (probably styx, given the rest of Vixen's config story). The shape is "Vixen values written to a file"; the type system is the schema.

## Related

- [Architecture and infrastructure](/architecture-and-infrastructure/) — the trusted-builder fleet shape that this model generalises.
- [Build and resolution](/build-and-resolution/) — the discovery + obligations mechanism that powers probes.
- [Cache and trust](/cache-and-trust/) — the verification-races-with-work and tightening-monotonic disciplines that apply to ability values as much as to anything else cached.
- [Design the executor scheduler](/design-executor-scheduler/) — the remaining Task: orchestrator routing logic *given* the ability model.
- [Epic: vx cargo build, end-to-end](/vx-cargo-build-trace/) — the trace this fits into.
