+++
title = "phon — the wire"
+++

phon is the serialization layer: how values leave a process — onto disk, over a
socket, across a language boundary — and come back *provably the same*.

Its design premise is that a wire format without a schema story is just
organized hope. phon messages are typed by schemas whose identities are
content-derived ([taxon](@/constellation/taxon.md) ids): the reader can check,
cryptographically, that it's decoding the shape the writer encoded — or
negotiate the difference. Schema evolution (adding fields, growing enums) is
explicit and checkable rather than folklore.

Three properties matter to the rest of the constellation:

- **Self-description without self-description tax.** Schemas travel once (or
  are already shared, since ids are content-derived); payloads stay compact.
  A cold reader can bootstrap from the self-describing mode; warm paths pay
  nothing.
- **Cross-language by construction.** The Swift implementation reads what the
  Rust implementation writes because both derive their codecs from the same
  schema model — this is what [vox](@/constellation/vox.md) rides to generate
  typed clients.
- **Fast paths that stay honest.** Codecs lower to
  [weavy](@/constellation/weavy.md) — interpreted or JIT'd — and the encode/
  decode paths are verified against reference implementations. Performance
  never gets to change the bytes.

In vixen specifically, phon is how the machine's values persist (memo entries,
stored artifacts' metadata) and how services talk (the daemon/runner protocol,
the editor tooling). If [taxon](@/constellation/taxon.md) is what a type *is*
and [weavy](@/constellation/weavy.md) is where its bytes *live*, phon is how
those bytes *travel*.
