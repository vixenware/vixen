+++
title = "Design and implement Windows sandbox (AppContainer)"

[extra]
type = "Task"
status = "todo"
priority = "high"
tiers = ["2"]
tags = ["sandbox", "windows", "platform"]
created = "2026-05-26"
+++

Linux has [`landlock-exec`](/architecture-and-infrastructure/#vfs-and-sandbox-stack--sandbox-state-per-os). macOS seatbelt is recoverable from commit `f4effa0a`. Windows has neither — no AppContainer code, no Job Object code, no integrity-level work in the tree.

For Tier 2 to ship on Windows runners, we need a Windows sandbox that enforces the same property as Landlock on Linux: a process cannot read bytes outside the declared read-set, cannot write outside the declared write set, cannot talk to the network unless declared, can only exec from declared paths.

Direction: **AppContainer with capability SIDs scoped to the VFS subtree.** Microsoft's modern primary, used by Edge/Chrome content processes, designed for exactly the "untrusted process with declared resources" case. Restricted tokens + Job Objects as the flexible fallback if AppContainer turns out to misbehave with our toolchains.

Concrete next step: a spike that gets a `Hello, world` cargo build running inside AppContainer on a Windows runner, observe what breaks. The list of what breaks tells us whether AppContainer is viable or whether we need to retreat to restricted tokens.

Open sub-questions:

- Toolchain compatibility: do MSVC / clang-cl / mingw / rustc-msvc behave under AppContainer without special-casing?
- Reuse of the ProjFS VFS mount as the access-controlled filesystem subtree, or a separate hard-link / junction tree?
- Sandbox application point: from inside the parent (`vx-runner` calls `CreateProcess` with sandbox attributes) or a standalone exec wrapper analogous to `landlock-exec`?

This is a blocker for Windows Tier 2. Tier 1 (CCC) ships on Windows runners because CCC's correctness only needs cache keys, not enforcement.
