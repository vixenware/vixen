+++
title = "Provenance, audit, and sovereignty"
weight = 3
+++

**You now have to prove what's in your software.** Not as good practice — as
law. The EU Cyber Resilience Act is in force; its vulnerability-reporting
obligations begin September 2026 and full application lands December 2027.
NIS2 makes supply-chain security a board-level duty in critical sectors; DORA
does the same for finance. The common demand underneath all of them: *know
exactly what is in every artifact you ship, be able to show how you know, and
answer fast when something in it turns out to be vulnerable.*

Most teams answer with paperwork: an SBOM generated in CI, exported to a
spreadsheet, stale by the next release. vixen's answer is structural, and it
falls out of the same two mechanisms as everything else on this site —
observed reads and content identity:

- **The build's audit trail already exists — it's the [receipt](/using/receipts/).**
  Every artifact's receipt is the machine's actual decision trace: every input
  read, every tool invoked, identified by content hash, complete by
  construction. Not a report *about* the build; the build's own working.
- **An SBOM that cannot drift.** Because builds are hermetic, an undeclared
  input isn't under-reported — it's a build failure. The dependency story in
  the receipt is exhaustive, or the build didn't happen.
- **Artifacts that carry their own recipe.** Binaries built by vixen embed,
  as signed metadata, the entire recipe that produced them: the full input
  graph by content hash, toolchain included. Auditing a deployment means
  reading the artifacts you possess, not hunting down repositories — and
  rebuilding for verification is one command, from the binary alone.
- **Advisories keyed to content, not version ranges.** A vulnerability
  matches the actual code in your artifact — including knowing that your
  patched build of an affected version is *not* affected. Fleet-wide audit
  answers "is the vulnerable code in anything we ship?", exactly.
- **Operated from the EU.** The registry and cache infrastructure are run by
  vixenware, an EU company, on EU infrastructure — residency and jurisdiction
  as properties you can point at, not FAQ answers.

## The honest part

This page mixes what runs today with what is designed and being built —
deliberately, because the design is the commitment. What's real now: hermetic
observed builds, receipts, and content-addressed caching, at the scale the
[status page](/status/) documents with dates. Recipe-embedded artifacts,
content-keyed advisories, and the EU-operated registry are specified and
under construction; they are described here as designed, and
[status](/status/) is the ledger that says when each one stops being a
promise. If you are evaluating vixen for compliance work on a real deadline,
read that page first — it will not flatter you.
