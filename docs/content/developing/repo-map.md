+++
title = "The repo map"
weight = 1
+++

Two repositories, one boundary, and the boundary is about *timing*, not
secrecy.

**The facet monorepo** (public, `github.com/facet-rs/facet`) holds the
foundations and the language: facet (reflection), taxon (types/identity),
weavy (substrate), phon (wire), snark (parsing), vox (RPC), vix (the language
and machine), rodin's vix implementation, and the shared tooling. If it's a
building block someone could want without wanting vixen, it lives here, in
the open, now.

**The vixen repository** (`code.vixen.rs/vixen/vixen`) holds the product:
the store, the virtual filesystem backends, the sandbox profiles, the
services, this knowledge base. Most of it is *intended to open* as it
stabilizes — the standing policy is that the only permanently-proprietary
component is the cloud control plane, which lives in neither of these repos.
Read "private" here as "pre-release," not "hidden."

Practical notes for contributors:

- **Dependencies point one way.** vixen consumes the public crates (as
  published/git dependencies); the public repo never depends back. Where
  vixen must *implement* a public interface (the exec backend), the
  connection is an RPC protocol, not a link — see
  [services](@/concepts/services.md).
- **History is append-only.** Both repos have a no-history-rewrite policy.
  Mistakes get fixed forward; nothing is scrubbed.
- **Some absences are load-bearing.** If a component seems conspicuously
  missing where one obviously belongs, read
  [Deliberate holes](@/developing/holes.md) before "helpfully" filling it.
- **Working docs vs. this site.** Design documents accumulate in
  `docs/design/` as raw working material; this knowledge base is the curated
  layer. Promotion from one to the other is deliberate, not automatic.
