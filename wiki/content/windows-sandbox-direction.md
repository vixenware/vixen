+++
title = "Windows sandbox direction"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["tier-2", "sandbox", "windows", "platform"]
+++

Linux has [[VFS and sandbox stack|landlock-exec]]. macOS has a stated direction (Apple Sandbox / "seatbelt") but no code. Windows has neither — no design captured, no code, no notes.

For [[Tier 2 — Vixen Build]] to deliver "cache everything" on Windows runners, we need a Windows sandbox that enforces the same property as Landlock on Linux: a process *cannot* read bytes outside the declared read-set, *cannot* write outside the declared write set, and *cannot* talk to the network unless we declared it could. Without that enforcement, the cache key on a Windows build is a guess, not a guarantee, and [[Cache key tightening is monotonic; loosening is dangerous|the cache returns wrong bytes]].

## Candidate mechanisms

Each Windows sandboxing primitive has different scope, complexity, and per-process overhead. Vixen needs one that:

- Constrains filesystem reads to a declared subtree (the VFS mount).
- Constrains filesystem writes to a declared subtree.
- Denies network by default.
- Allows process execution from declared paths only.
- Is cheap enough to apply per-action (one process per cargo subcommand).

### AppContainer

Microsoft's modern sandbox mechanism. Used by Edge/Chrome content processes, by UWP apps, by Win32 apps that opt in. Capability-based: the process gets a list of capability SIDs that grant access to specific resources (filesystem subtrees declared via `SECURITY_CAPABILITIES`).

- **Pro:** designed for exactly this use case (untrusted process with declared resource access). Win10+ ubiquitous.
- **Pro:** integrates with integrity levels (low-integrity by default).
- **Con:** setup is verbose — capability SIDs, security descriptors, `CreateProcess` with extended attributes.
- **Con:** some compiler toolchains (especially older ones) misbehave inside AppContainer.

### Restricted tokens + Job Objects

Classic Chromium-era pattern. Strip privileges from the process token; place it in a Job Object that restricts UI, network, child process creation. Filesystem access controlled via ACLs on the VFS mount.

- **Pro:** more flexible. Works on older Windows.
- **Pro:** Job Objects also give us resource limits (CPU time, memory, IO).
- **Con:** more code, more nuanced security model.
- **Con:** Microsoft is steering away from this in favour of AppContainer.

### Mitigation policies as defence-in-depth

`SetProcessMitigationPolicy` to disable child process creation, dynamic code execution, etc. Not a sandbox on its own — combined with one of the above.

## Open questions

- Which primary mechanism do we pick?
- Toolchain compatibility: do MSVC / clang-cl / mingw / rustc-msvc behave under our chosen mechanism without special-casing?
- Can we reuse the ProjFS VFS mount as the access-controlled filesystem subtree, or do we need a separate hard-link/junction tree?
- Is there a Windows equivalent of `landlock-exec` — a standalone exec wrapper — or does the sandbox have to be applied from inside the parent (`vx-runner` directly invokes `CreateProcess` with sandbox attributes)?
- How does the cache key derivation pick up Windows-specific environment variables that affect compilation (`PATH`, `INCLUDE`, `LIB`, `VCToolsInstallDir`, etc.)?

## What unblocks resolution

A small spike: pick AppContainer as the working hypothesis, get a "Hello, world" cargo build running inside it on a Vixen runner, observe what breaks. The list of what breaks tells us whether AppContainer is viable or whether we need to retreat to restricted tokens.

## Disposition

This is a blocker for Windows Tier 2. We can ship Tier 1 (CCC) on Windows runners because CCC's correctness only needs cache keys, not enforcement. Tier 2 needs the enforcement.

## Related

- [[VFS and sandbox stack]] — current state of the sandbox surface.
- [[Tier 2 — Vixen Build]] — what this is blocking.
- [[Cache key + sandbox: one design doc?]] — the broader cache-correctness question this rolls up into.
