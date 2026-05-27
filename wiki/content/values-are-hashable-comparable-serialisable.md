+++
title = "Values are hashable, comparable, and serialisable"

[extra]
type = "ArchitectureNote"
topic = "language"
created = "2026-05-26"
tags = ["language", "runtime", "cache", "values"]
+++

A core language-level property. Every value the Vixen runtime produces — scalar, struct, enum variant, function, closure, tree, blob, stream — is required to be:

- **Hashable.** A canonical content hash exists for any value. Two values with the same hash are interchangeable; two equal values have the same hash. The hash is what gets mixed into cache keys.
- **Comparable.** Equality is decidable. Same canonical form ⇒ equal; distinct canonical forms ⇒ not equal. There is no "this thing is sometimes equal" or value-identity-via-pointer.
- **Serialisable.** Bytes exist. A value can be written out, transmitted, stored in a content-addressed cache, fetched back, and reconstituted into the same value.

The current runtime fulfils this for most value shapes but **serialisation is not yet complete** for every value kind (closures, streams). It is a stated design goal that every value eventually be fully serialisable; some shapes still need work.

## Why

The property bends the entire language toward the use cases Vixen exists for. The cache, the executor fleet, the closure-shipping story, the demand-warmed flow, and the build-graph-is-provenance-graph property all reduce to "I can hash this, I can ship it, I can verify it came back equal."

- **Caches need it.** The whole point of [[exec caches the observer's return value, not the Process bytes|caching observer results]] is that any `T` can be stored and retrieved. If some `T` weren't serialisable, that subset of exec results couldn't be cached.
- **Closure shipping needs it.** Sending an observer (or any closure) to a remote executor requires serialising the closure. No serialisation, no remote exec.
- **Demand signals need it.** The structured inputs in a [[Demand-warmed cache|miss signal]] are values. They get serialised onto the queue, deserialised by the trusted-builder fleet.
- **Equality is provenance.** "This output was produced from these inputs" reduces to "hash(inputs) → handle(output)." Without canonical hashing, attestation breaks.
- **Distributed evaluation needs it.** A closure shipped to an executor must produce a result that's hashable + serialisable, or the result can't return through the cache to the caller.

## Consequences

- **No identity comparisons.** Values aren't compared by pointer or by allocation site; they're compared by content. Two function values that close over the same captures and the same body compare equal even if allocated separately.
- **No mutable references at the language level.** A value can't change content without becoming a different value. (Mutation happens at the runtime / executor boundary, but it's invisible to the language.)
- **No reflection on internal layout.** The language only exposes the canonical form, not the in-memory representation. The runtime is free to pick whatever representation it wants per platform.
- **Wire format is implicit.** The serialisation is whatever the runtime decides; the language doesn't expose a "write this value to bytes" primitive — values just flow through the cache and the wire.

## Status

Implemented for most value shapes. Closures and streams are still maturing — closure-shipping (Tier 2's distributed eval) is one of the [[Streams as first Tier-2 primitive?|open primitive questions]]; the streaming side benefits from the cache-everything story but adds its own representation problems.

## Related

- [[exec caches the observer's return value, not the Process bytes]]
- [[The build graph is also a provenance graph]]
- [[Demand-warmed cache]]
- [[Streams as first Tier-2 primitive?]]
- [[Process model]]
