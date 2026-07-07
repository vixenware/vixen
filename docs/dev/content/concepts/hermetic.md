+++
title = "Hermetic execution"
weight = 5
+++

Everything vixen promises — trustworthy caches, exact invalidation, sound test
skipping — rests on one claim: *the recorded inputs of a computation are its
actual inputs*. Inside vix code the machine can enforce that directly. But builds
shell out to real tools — `rustc`, `cc`, code generators — and a child process
can read anything the operating system lets it. Hermetic execution is how the
claim survives contact with other people's programs.

The model has two rungs, deliberately staged:

**Rung 1 — confine.** The process runs in a sandbox (Seatbelt on macOS, Landlock
on Linux) that allows exactly the declared inputs, the declared toolchain, and
the designated output locations. An undeclared read doesn't silently succeed and
poison the cache — it *fails, loudly, immediately*. This inverts the failure
mode: instead of "the build works but the cache is a lie," you get "the build
breaks until the dependency is declared." Annoying once; sound forever.

**Rung 2 — observe.** Inputs are served through a virtual filesystem, mounted
per-execution from the content-addressed store. Every open, read, and directory
listing is *recorded* — the process's true read-set, at file granularity, with
content hashes. Declared dependencies stop being promises and become evidence.
The VFS also means inputs never need to be copied into place: the store *is*
the filesystem the process sees.

A detail worth appreciating: per-execution isolation is cheap because the
virtual mounts are virtual. One real mount hosts many logical prefixes; the
sandbox is what keeps each process inside its own. Spinning up an isolated,
fully-declared environment per compiler invocation costs almost nothing, which
is what makes "every exec is hermetic" viable as a default rather than an
opt-in ceremony.

## What hermeticity is *not* for

This is not primarily a security boundary against malicious code — it's a
*correctness* boundary against accidental dependencies: the stray `~/.cargo`
config, the system header nobody declared, the environment variable that made
it work on one machine. Treating those as impossible rather than unlikely is
what lets a cache hit mean something.

Current state and platform coverage live on the [Status](/status/)
page. The design intent is fixed; the rungs land in order.
