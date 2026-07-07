+++
title = "Fixtures and oracles"
+++

The house testing style is **differential**: run the real thing and a trusted
oracle on the same input, compare. It beats example-based unit testing for
this project because the hard bugs here are *disagreements with reality* —
with Cargo's resolution semantics, with SemVer's precedence rules, with what
a compiler actually does — and reality is available to consult.

The oracles in active service:

- **Real `cargo`.** Resolver fixtures are materialized as actual Cargo
  workspaces on disk; `cargo tree` / `cargo generate-lockfile` and the unit
  graph are ground truth. rodin's score is "fixtures where we match cargo,"
  not "tests we wrote pass."
- **Real `rustc`.** Build-pipeline tests drive actual compiler invocations
  and diff the resulting unit graph against Cargo's own plan for the same
  workspace — including build scripts and proc-macros.
- **Reference crates.** Version precedence is pinned against the `semver`
  crate across hundreds of generated pairs; the wire format is pinned against
  reference codec implementations.
- **The machine against itself.** Force-copy vs. reuse; interpreter vs. JIT;
  incremental vs. from-scratch hashing — where two paths must agree, that
  agreement is a permanent test, and it's the cheapest oracle there is.

Conventions that keep this honest:

- **Pin your snapshots.** Tests against the crates.io index use captured,
  dated snapshots committed as fixtures (with provenance notes) — a moving
  oracle is not an oracle. Live-network tests exist but are gated and
  skippable.
- **Gate on host tools, skip gracefully.** Tests needing rustc/cc probe for
  them and skip when absent — differential coverage shouldn't make the suite
  unrunnable on a minimal machine.
- **When you fix a divergence, keep the fixture.** Every disagreement with an
  oracle that ever existed remains a test. The fixture suite is the project's
  accumulated knowledge of where reality has sharp edges.
