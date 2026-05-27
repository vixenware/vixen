+++
title = "Design cache key + sandbox together before shipping cache code"

[extra]
type = "Task"
status = "todo"
priority = "high"
tiers = ["2"]
tags = ["cache", "sandbox"]
created = "2026-05-26"
+++

The cache key for an exec invocation is "exhaustively, every byte the process read, plus the binary, plus the invocation arguments." The sandbox is what enforces that the process *cannot* read bytes outside the read-set we hashed. These aren't two decisions — they're one decision with two halves. Sandbox spec and cache-key spec must be derived from each other.

Pre-decision, exec is **not cached** in Vixen today. That's deliberate. Better no cache than a half-baked one that returns wrong bytes.

The doc needs to nail down: the minimum sandbox closure we accept (FHS-free, store-path-only, sealed `/proc` and `/etc`, Nix-style); which environment variables are key-bearing; how ambient toolchains under "fact discovery / attached obligations" interact with the closure; how declared-identity tokens (e.g. "Xcode 26.4 manifest hash") collapse the read-set under trust.

Output: a written design that gates any Tier-2 cache code landing.
