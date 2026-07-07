+++
title = "Developing vixen"
+++

This section is for people changing vixen itself — the machine, the language, the
resolver, the constellation. It is opinionated on purpose: vixen's development has
a small number of load-bearing disciplines, and following them is the difference
between a contribution that folds cleanly and one that fights the reviewers.

- **[The repo map](@/developing/repo-map.md)** — what lives where, what's open
  today vs. opening later, and how the public/private boundary is meant to be
  read.
- **[The invariants](@/developing/invariants.md)** — demand-driven evaluation
  cannot be retrofitted, so it is guarded by tripwire tests that watch *how*
  results are computed, not just what they are. Know these before touching the
  machine.
- **[Fixtures and oracles](@/developing/fixtures.md)** — the house style of
  testing: differential tests against a trusted oracle (real `cargo`, the real
  `semver` crate, force-copy execution modes) beat unit tests that encode the
  author's assumptions.
- **[The method](@/developing/method.md)** — how work lands: branches, gates,
  staged commits, and why every claim needs its evidence attached.
- **[Deliberate holes](@/developing/holes.md)** — some things are missing on
  purpose. What that means and what to do when you find one.
