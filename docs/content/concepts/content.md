+++
title = "Content addressing"
+++

Every value in vixen — a parsed manifest, a compiled object, a resolver decision,
a test result — is identified by a cryptographic hash of *what it is*. Not where
it lives, not when it was made, not what produced it: what it **is**.

Timestamps answer "did this file change since last time *on this machine*?"
Content hashes answer "is this *the same value*?" — a question whose answer is
true everywhere, forever. That difference quietly powers most of what makes vixen
worth using:

- **Memoization** — the memo key for a computation is the content of its inputs.
  Same content, same answer, no work.
- **Deduplication** — two identical values are one value. A dependency built for
  two different consumers exists once.
- **Early cutoff** — if recompiling a crate produces a byte-identical interface,
  its dependents' inputs are unchanged, so their demands memo-hit and the
  rebuild wave dies immediately. `touch` a file: nothing happens. Change a
  comment: the parse changes, but the *items* don't, and everything downstream
  cuts off.
- **Distribution for free** — a content hash means the same thing on your laptop
  and in CI. A shared store makes "CI already ran this test on exactly this code"
  a cache hit, not a feature.

## Identity for types, too

vixen extends content identity below values, to **types themselves**. A type's
identity is the hash of its canonical structure — its fields, their types,
recursively — computed by [taxon](@/constellation/taxon.md). Two independently
compiled processes agree on what `Config { port: u16, host: String }` *is*
without ever coordinating, because they hashed the same structure. This is what
lets stored values, wire messages, and in-memory data share one type vocabulary
across machines and even across languages.

## The costs, honestly

Hashing is not free, and a naive implementation hashes the same data over and
over — a growing list re-hashed on every append is quadratic. vixen's answer is
*incremental* hashing: containers carry their hash state and extend it in
constant amortized time, so identity keeps up with mutation instead of fighting
it. (This was measured, not assumed — the resolver's hot loop was profiled at
7,300× slower than reference before this machinery, and near parity after.)

The other cost is discipline: content addressing is only as sound as the claim
that inputs were *fully captured*. A computation that secretly reads something
uncaptured poisons its own cache entry. That's not a hashing problem — it's why
[read-sets are observed](@/concepts/read-sets.md) and
[execution is hermetic](@/concepts/hermetic.md).
