+++
title = "Cache and trust"

[extra]
type = "Doc"
tiers = ["1", "2", "3"]
updated = "2026-05-26"
+++

Correct caching is the entire product. A cache that returns wrong bytes is worse than no cache.

## Cache key tightening is monotonic; loosening is dangerous

The cache key for a unit must capture every input that affects its output. If it doesn't, two genuinely different invocations collide under one key and the cache returns wrong bytes — silently — for whichever loses the race.

If we discover a key was *not specific enough*, the fix is to **tighten** it: mix more inputs into the derivation. New keyspace, fresh. The old confused entries are not under any key the new derivation produces, so lookups against them all miss. They age out by LRU eviction. No invalidation step, no flush, no migration.

Tightening is always safe. Loosening is always dangerous — removing an input from the derivation can cause two genuinely different invocations to collide, and every previously-correct entry might now serve wrong bytes for some new invocation. We do not loosen.

No emergency flush button. That's a feature.

## Trust is per-cache, not global

Even with provably correct keys, anyone who can write `(key, value)` pairs into a shared cache poisons it for everyone reading. Cache poisoning is the headline attack.

The model: **trust is a property of a path *into* a cache, not of the cache itself.**

- Each cache has its own ingress policy. The policy decides which signatures / attestations / producer identities it accepts.
- The local on-disk cache is trusted by construction — you wrote it.
- Reads from any other cache flow through a *gate* that verifies the entry against the policy before insertion into the local cache.
- Once in the local cache, reads are unconditional.

Consequences:
- Caching ships *with* the trust gate from day 1. Not "cache first, retrofit signing later." That sequencing produces CVE-grade poisoning.
- Customer-side CCC instances do not have ingress rights to the public cache; they emit demand signals instead (see below).
- Org policy is expressible. A regulated industry can configure their internal cache to require N-of-M signed attestations. A startup can configure theirs to accept their team's GitHub identities. Same primitive, different gates.

## Declared identity collapses the read-set under trust

The exhaustive cache key is "every byte every input touches." In the limit, hashing 4 GB of toolchain on every lookup. Impractical and not even fully knowable without running the process.

Structural shortcut: replace the hashed contents of an identifiable toolchain with a **single opaque token derived from its declaration**, *under the assumption that the declaration is honest*. Examples:

- A GitHub Actions runner image: known digest on disk + published manifest → one token replaces the hash of every file in `/usr`, `/etc`, etc.
- Xcode 26.4: version + manifest hash → one token replaces the hash of every binary in `Xcode.app`. Threat model "user has a patched clang inside an otherwise-stock Xcode" is vanishingly rare for honest builds.
- A crates.io entry: `(name, version, registry-hash)` is already a content-addressed declaration. crates.io's version immutability means the descriptor *is* the identity. This is what Tier 1 uses for every cache lookup.

Trust is the lever. The shortcut works because we trust the declaration. If we don't, fall back to the exhaustive read-set. A tampered local install poisons only that user's *local* cache; they have no ingress rights to the public cache.

## How the cache warms: demand signals

Customer-side instances are **read-only** against the public cache. Writes come only from trusted builders we operate.

When a customer's request misses, the cache lookup itself carries enough information for backfill: the structured inputs (stable identities like crates.io descriptors at Tier 1; argv + env + read-set handles at Tier 2). The response to the lookup contains the hit/miss partition; the lookup *is* the demand signal. ccc-backend queues the miss for the trusted-builder fleet to materialise.

Practical shape:

- Tier 1 misses carry `(crate-name, version, registry-hash, compile-args, toolchain-identity)`. The trusted-builder fleet fetches sources from crates.io directly, builds with that toolchain inside the sandbox, pushes the output to the cache.
- Tier 2 misses carry the broader structured invocation. The fleet replays the build the same way.

No customer ever pushes bytes to the public cache. No customer can poison it. Cache poisoning by clients is impossible by construction.

The demand signal is unprivileged — anyone, including unauthenticated callers, can emit one. The cache *response* depends on auth:

- **Authenticated customers** get the hit bytes back. Authenticated misses bump the missed key up the trusted-builder priority queue.
- **Unauthenticated callers miss across the board.** No anonymous reads of the public cache at Tier 1. But their miss signals still feed the backfill queue at lower priority, so demand-driven warming proceeds regardless of who asked.

Free tier with anonymous reads is deferred until grant or VC funding underwrites the infrastructure. Architecturally, free-tier introduction is reversible — flip the read gate, leave the rest.

## Backfill runs inside the same sandbox + VFS as Tier 2

A trusted-builder backfill does not just shell out and `cargo build`. It runs the build inside the same sandbox + VFS substrate Tier 2 customer work runs in — landlock on Linux today, seatbelt on macOS once restored, AppContainer on Windows once designed.

Two reasons:

1. **Cache validity by construction.** The sandbox enforces that the build cannot read bytes outside the declared read-set. The cache key is derived from that same read-set. So the cache key faithfully captures everything the build depended on — by construction, not by audit. Without the sandbox, the build could quietly read `/etc/something` we didn't hash, and the cache entry would be subtly wrong.
2. **Tier 2 dogfooding.** Every backfill is a production exercise of the sandbox + VFS. Bugs in the per-OS sandbox, in the VFS mount lifecycle, in the FUSE / FSKit / ProjFS layers — they get hit on real workloads before any customer's build depends on them.

We do not maintain a "lightweight" backfill path and a "real" customer-build path. The sandbox + VFS is the only path; backfill exercises it because it is the substrate.

## Asymmetric infrastructure: customer-facing vs fleet-facing

Substrate is the same — CAS, VFS, sandbox, RPC over Vox — but the deployment posture differs by who controls the machine. Customer-facing paths favour ease of install and safety in heterogeneous environments. Fleet-facing paths favour performance and operational integration on hardware we own.

| Layer | Customer-facing | Vixen fleet |
| --- | --- | --- |
| Store access | Read-only on public store; demand signals on miss | Direct write (trusted-builder-signed) to public store; full access to private store |
| Toolchain ownership | Customer's installs (proprietary discovered, OSS hermetic-from-public-store) | Vixen-installed under Vixen's own licenses; private-CAS-served |
| VFS backend (macOS) | FSKit — supported, no kext entitlement needed, slower | MacFUSE (paid license) or custom kext — faster, operational complexity acceptable on hardware we control |
| VFS backend (Windows) | ProjFS | WinFsp / Dokany / custom dev-mode driver — faster than ProjFS, operational complexity acceptable on hardware we control |
| VFS backend (Linux) | FUSE | FUSE / direct mount — fewer constraints |

This is deliberate posture, not accidental asymmetry. Each row trades a property based on what changes between contexts:

- **Trust gate orientation.** Customer machines can read the public cache through a verifying gate; they cannot write. Fleet machines write to caches whose trust policy accepts trusted-builder signatures. See [Trust is per-cache, not global](#trust-is-per-cache-not-global).
- **Bytes that can't be redistributed stay private.** Apple's and Microsoft's EULAs prohibit transferring Xcode / MSVC bytes to third parties. They don't prohibit Vixen storing its own licensed copy in a private CAS and mounting via VFS on its own hardware — same legal posture as MacStadium or GitHub Actions Mac runners. The bytes never leave Vixen's licensed environment.
- **VFS backend choice follows entitlement and performance.** A kext on a consumer's Mac is a non-starter — entitlement complications, install friction. On hardware Vixen owns, the operational cost of a kext (or paid MacFUSE) is acceptable for the performance win.

## Public store vs private store

Concrete partition by content type:

- **Public store.** OSS toolchains (Rust, C/clang, protobuf, zig, …), customer-build artifacts produced by trusted builders, anything redistributable. Read-accessible by authenticated customers; demand signals trigger backfill. Multi-tenant CAS with the trust gate as ingress policy.
- **Private store.** Xcode, MSVC, any proprietary toolchains Vixen holds licenses for. Same CAS substrate (CDC chunks, VFS mounting), access-controlled to Vixen fleet executors. Customer builds never receive the bytes — only artifacts produced *by* the proprietary toolchain on a Vixen executor.

A single Vixen fleet executor pulls chunks from both stores: OSS bytes from the public store, Xcode/MSVC bytes from the private store. CDC chunking means closely-related versions (Xcode 14.2 and 14.3) share most chunks structurally — cache warmth across versions is automatic, not "pre-install N variants per executor."

Whether Xcode and MSVC actually function correctly served from chunked CAS via VFS is empirically unknown — code signing, `dyld` interactions, framework lookups, mmap on a VFS-backed path may or may not survive. Legal is settled (internal storage is fine; only redistribution to third parties is forbidden); technical is open. If the VFS path doesn't work, fall back to "pre-installed on each Mac/Windows fleet box" — still legal, just less elastic. Customer-side discovery is unaffected either way.

Customer-local discovery for proprietary toolchains is the one place this partition forces ambient: see [Executor abilities → Customer-local](/executor-abilities/#customer-local-the-one-place-ambient-discovery-is-forced).

## exec caches the observer's return value, not the Process bytes

A Tier 2 detail. `exec(args) with { ..., observer: Fn(Process, Tree) #sandbox -> T }` is the central build primitive. When the runtime demands its result, the cache stores the **observer's `T`**, not the underlying `Process` bytes (stdout, stderr, exit, write-set Tree).

Implications:

- Hits are cheap: a cached value is ready; no process runs, no observer re-evaluates.
- Streams in return values are fully realised on hit — `Stream[U]` yields its elements at full bandwidth, backpressure semantics are vestigial.
- Observer source identity is part of the cache key. Revving the observer invalidates downstream entries.
- The shape becomes uniform across kinds of work: `exec`, `acquire`, `render`, and the other persisted intrinsics all produce a `T`; all cache that `T` against inputs. There is no "process result" cache vs "computed result" cache.

The price (observer changes = cache miss) is paid willingly. The observer should be tiny anyway — the idiomatic shape is "observer = the small function that extracts the structured value the caller cares about." Big logic belongs in the rest of the Vixen program.

## Which intrinsics get persisted

The persistent cache stores only the results of intrinsics worth storing — the cost of writing to and reading from the cache must beat the cost of recomputing. That's a per-intrinsic decision, not a user-visible knob. See [Vixen language → Demand-driven evaluation](/vixen-language/#demand-driven-evaluation) for the user-facing framing; this section covers the runtime mechanism.

Policy lives on the intrinsic's Rust impl:

```rust
trait Intrinsic {
    fn cache_policy(&self) -> CachePolicy;
    fn evaluate(&self, ctx: &mut Ctx, args: &[Value]) -> Result<Value>;
}

enum CachePolicy {
    Persist,    // exec, acquire, Http, Toml/Json/Yaml::parse, pubgrub_solve, render, discover-tuples
    InMemory,   // String::concat, equality, hash, arithmetic, value construction, Tree projection
}
```

The evaluator dispatches on policy. `Persist` → derive cache key from arg identities, consult cache, on miss compute and store. `InMemory` → just compute and let the per-evaluation memo table handle dedup within the run.

### Three categories anchor the policy

- **External I/O.** `exec`, `acquire`, `Http`, `host.exec`, `host.read`, `host.which`, the rest of the `Host` surface. Network or process round-trip. Recompute cost is orders of magnitude above cache I/O. Always `Persist`. For `Host` operations, what gets persisted is the obligation tuple — recording the call + observed result — not the host's bytes themselves.
- **Substantial pure CPU.** `pubgrub_solve`, `Toml/Json/Yaml::parse`, `render`. Deterministic and expensive. Parsing a 30k-line `Cargo.toml`, solving a constraint graph with thousands of crates, rendering a templated config — all beat cache I/O cost. `Persist`.
- **Trivial pure CPU.** `String::concat`, `Hash::of`, equality, scalar arithmetic, record/list construction, `Tree::file`, `Map::get`, `Array::index`. Computing the result is faster than hashing the inputs to derive a cache key. `InMemory`.

### The cache layer is bypassed for `InMemory`, not consulted-and-missed

The key-derivation cost is itself non-trivial — hashing a `Tree` to get its content identity is real work. For an `InMemory` intrinsic the evaluator does not derive a persistent key at all. The per-evaluation memo table keys by structural identity, but it's an in-memory hash table that lives for the duration of `main` and goes away when it returns.

### Pure user functions are `InMemory`-only

A function declared in `.vx` with a body is pure. The evaluator memoises it per evaluation by `(function-identity, argument-identities)` and does not persist that memo. Two reasons:

1. The runtime can't tell whether a pure user function is expensive. `fn add (a, b) = a + b` and `fn slow_check (x) = expensive_pure_thing(x)` look identical from the evaluator's vantage point.
2. The valuable expensive parts inside user functions are intrinsic calls, which are cached at their own boundaries. If `@vixen/cargo::resolve_workspace` is slow, the slow parts are the `pubgrub_solve` and `Toml::parse` calls inside it — those hit the persistent cache. The user function's whole-result identity isn't separately worth storing because its constituent intrinsic results already are.

Recomputing `resolve_workspace(manifest)` after a process restart re-walks the Vixen code, but every expensive intrinsic call inside it hits the persistent cache. End-to-end cost is the same as caching the whole thing, without duplicating stored values. There is no user-facing opt-in to persist a pure-user-code intermediate; if a computation is expensive enough to be worth caching, it's because of the intrinsics it calls, and those cache themselves.

### Caching is per-result, not per-aggregate

A `Tree` value built up from many `exec`-produced blobs is not cached as one artifact. The Tree's identity is a Merkle root over its entries; the entries' identities are the cached `exec` results. Constructing the Tree from those entries is `InMemory` (record construction). The cache holds the leaves; the aggregate is rebuilt cheaply from leaf identities on demand. Same for any aggregate (`Map[K, V]`, `Array[T]`) — the expensive elementwise work is what's persisted.

### Open questions

- **Empirical thresholds.** Some intrinsics are borderline (`render` on a 200-byte template). When in doubt, start `InMemory` and promote to `Persist` based on observed cost. Promoting an intrinsic from `InMemory` to `Persist` is monotonic-safe — old runs simply have no cache entries for it; future runs do. Same shape as cache-key tightening above.
- **Size-aware policy.** Could the same intrinsic be `Persist` for some inputs and `InMemory` for others (e.g. `Toml::parse` for big files, `InMemory` for tiny ones)? Possible but adds branching at the dispatch point. Defer; flat per-intrinsic policy is fine for v1.
- **Per-cache policy.** Whether the public cache accepts every `Persist`-produced result is a separate question from whether the local cache does. The local cache `Persist`s everything in category; the public cache only accepts trusted-builder-signed entries via the per-cache trust gate above.
