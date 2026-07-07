+++
title = "Install"
weight = 1
+++

> **Early access note.** The exact install commands ship with the first access
> wave. This page describes the shape of an installation — what ends up running
> on your machine and why — so the commands, when you get them, won't be
> mysterious.

A vixen installation has three parts:

**The CLI** — what you type. It is deliberately thin: it parses your intent and
talks to a daemon. If the CLI ever feels smart, something has gone wrong
architecturally.

**The daemon** — the ambient service on your machine. It owns the capabilities:
the content store, virtual-filesystem mounts, sandboxed process launch, and
watching your working copies for changes. It runs whether or not a build is in
progress — a warm daemon is what makes "nothing changed, answer in
milliseconds" possible, because the watching already happened.

**The runner** — the service that accepts build work and advertises what it can
do (which toolchains are present, what platforms it targets). On your laptop,
daemon and runner run side by side and you never think about the distinction.
The distinction exists because it's also the *distribution* seam: a beefy build
box is just a machine whose runner advertises more, and dispatching work there
is the same conversation over longer wires. See
[Daemons, runners, and services](@/concepts/services.md).

## Platform expectations

macOS and Linux are the supported early-access platforms — the sandbox rides
Seatbelt and Landlock respectively, and the virtual filesystem has native
backends for both. Windows is deferred, deliberately: its filesystem
virtualization story needs its own design, and we'd rather ship two platforms
honestly than three approximately.

Toolchains (rustc, cc) are *discovered and pinned*, not bundled: the runner
advertises what it found, and builds record exactly which toolchain produced
what. Reproducibility comes from recording, not from pretending every machine
is identical.
