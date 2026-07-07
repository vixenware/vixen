+++
title = "vx-store — the store"
+++

vx-store is the persistence layer: a content-addressed store where values,
artifacts, and build metadata live between runs and between machines.

Its keys are hashes of content (the same identity discipline as everything
else — see [content addressing](@/concepts/content.md)), and its layout is
built for the access patterns a build system actually has:

- **Chunked large objects.** Big artifacts are split by content-defined
  chunking, so two mostly-identical files (say, consecutive builds of a large
  binary) share most of their storage automatically — deduplication happens at
  the chunk level, without anyone diffing anything.
- **Structured small objects.** Metadata nodes — directory trees, memo
  entries — are typed values on the [phon](@/constellation/phon.md) wire,
  addressed by hash, forming Merkle structures: verify a root, trust the tree.
- **Location transparency.** A hash means the same thing in every store, so
  stores compose: your laptop's store, a team server, CI's — the question is
  never "where was this built" but "do you have this hash," and fetching a
  missing value is safe by construction because the content is checked against
  the address.

The store is also the substrate for the virtual filesystem in
[hermetic execution](@/concepts/hermetic.md): mounts are materialized *from*
the store, and outputs flush *into* it — a process's whole world is
store-shaped, which is precisely what makes its behavior reproducible.

The memo — "the build system's memory" — persisting here is what turns
vixen's incrementality from per-session to permanent: a cache that survives
reboots, travels between machines, and can be shared by a team. That
integration is staged work; the store exists and the design treats the memo
as just another kind of content.
