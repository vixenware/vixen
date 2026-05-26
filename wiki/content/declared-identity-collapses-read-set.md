+++
title = "Declared identity collapses the read-set under trust"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["cache", "trust", "sandbox"]
+++

## Context

The exhaustive cache key for an exec invocation is "every byte the process read, plus the binary, plus the invocation arguments." In the limit, that means hashing the entire file closure the process could read from — clang's binary, every header in `/usr/include`, every library it links against, every locale file it might open, every entry of `/etc/passwd` and `/proc/cpuinfo` it inspects.

The exhaustive key is correct. It is also impractical: hashing a 4GB toolchain installation per cache lookup is not workable, and the closure is not even fully knowable without running the process to observe its read set.

There is a structural shortcut.

## Options considered

1. **Hash the exhaustive read-set** for every invocation. Correct. Slow.
2. **Trust nothing; rebuild everything locally** to verify. Defeats caching.
3. **Treat declared toolchain identity as a structural shortcut** — replace the hashed contents of an identifiable toolchain with a single opaque token, *under the assumption that the declaration is honest*.

## Decision

Option 3. Cache keys collapse the read-set of a *declared* toolchain into a single token derived from the declaration (e.g. "xcode 26.4 manifest hash X"). The exhaustive byte-by-byte read-set is the floor when nothing is declared or trusted; a trusted declaration replaces a subset of that read-set with one token.

## Examples

- **GitHub Actions runner image.** The runner image has a known digest on disk and a published manifest. The Vixen executor reads the digest, verifies the image identity, and uses the digest as a single token in the cache key — replacing the hash of every binary in `/usr` and every file in `/etc`.
- **Xcode 26.4.** A developer with Xcode 26.4 installed presents the version + manifest hash. Vixen substitutes that for the exhaustive hash of every binary in `/Applications/Xcode.app`. The threat model — "this user has a patched clang in the middle of an otherwise-stock Xcode" — is vanishingly rare for honest builds.
- **A nixpkgs store path.** Already content-addressed; the store path *is* the declaration, and it collapses an enormous read-set into a fixed-width hash.

## Consequences

- **Trust is the lever, not exhaustiveness.** The shortcut works only because we trust the declaration. If we don't trust it, fall back to the exhaustive read-set.
- **The shortcut is per-edge of the cache trust graph.** A given trust relationship (customer's private cache, public cache, regulated-industry quorum cache) decides which declarations it accepts. See [[Trust is per-cache, not global]].
- **Tampering blast radius is local.** A user who patches their local clang and lies about the declaration poisons *their own local cache* under our trust model. They cannot poison the public cache because they have no ingress rights to it — see [[Demand-warmed cache]] and [[Unauthenticated CCC requests miss but contribute demand signal]].
- **Tightening recovery still applies.** If a declaration we trusted turns out to be insufficiently specific (two different binaries hashed under one declared identity), the fix is to mix more inputs into the key — see [[Cache key tightening is monotonic; loosening is dangerous]]. Tightening is monotonic; the old over-broad entries become unreachable misses.
- **Sandbox + declaration interact.** The sandbox closure has to actually constrain reads to what the declaration covers. If a sandbox lets clang read `/etc/something` we didn't cover, the declaration's coverage is wrong and the key is wrong. See [[Cache key + sandbox: one design doc?]].

## Related

- [[Cache key + sandbox: one design doc?]]
- [[Trust is per-cache, not global]]
- [[Cache key tightening is monotonic; loosening is dangerous]]
- [[Tier 2 — Vixen Build]]
