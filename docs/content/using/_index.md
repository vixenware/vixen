+++
title = "Using vixen"
+++

This section is the practical guide for early access: getting vixen onto your
machine, building a real Cargo workspace with it, and — the part that makes it
worth your time — learning to *read what it did and didn't do*. vixen is built to
show its work; once you can read a demand trace, "why did this rebuild?" stops
being a mystery forever.

- **[Install](@/using/install.md)** — the daemon, the CLI, what runs where.
- **[Your first build](@/using/first-build.md)** — point vixen at a Cargo
  workspace; what happens, step by step.
- **[Receipts](@/using/receipts.md)** — reading demand traces: what was computed,
  what was reused, and the proof.
- **[Test selection](@/using/test-selection.md)** — how vixen decides which tests
  a change can affect, and how to check its reasoning.
- **[Current limitations](@/using/limitations.md)** — the honest page: what's
  fixture-scale, what's real-scale, which Cargo features are deliberately not
  covered yet.

Early access note: capabilities land in tiers. Pages in this section are tagged
with the tier they require; if a page describes something your build of vixen
doesn't do yet, the tag is why.
