+++
title = "Why vixen?"
weight = 1
+++

Which tool are you fighting? Self-select: **coming from cargo**, the question
is what changes about caching, granularity, tests, and trust. **Coming from
Bazel or Buck2**, the question is what "observed instead of declared" buys and
what it costs. Both sections end with what the incumbent does *better* today —
because for this product, honesty is the pitch.

## vixen and cargo

cargo is the reference for what your build *means* — vixen treats it as the
oracle and verifies against it continuously. The difference is everything
around the compiler calls:

- **Caching.** cargo's freshness is per-crate and local, based on timestamps
  and flags; a `touch`, a branch switch, or a second checkout starts real work.
  vixen's cache is content-addressed: same inputs, same answer, on any
  checkout, any machine, any day. Branch-switching costs what actually
  differs.
- **Granularity.** cargo rebuilds a crate when its files changed. vixen tracks
  *items*: a comment shift recompiles nothing; a private function's body
  change stops propagating at the first unchanged interface.
- **Tests.** `cargo test` runs what you ask. vixen additionally knows which
  tests a change *can't* affect — computed from the compiled artifacts'
  reference graphs — and skips them soundly.
- **Trust.** cargo assumes build scripts and tools read what they claim.
  vixen runs them hermetically: undeclared reads fail instead of silently
  poisoning the cache.

What cargo does better today: everything vixen hasn't reached yet — the long
tail of ecosystem quirks, editions of exotic tooling, sheer battle-years. See
[limitations](/using/limitations/); vixen's compatibility is measured, not
assumed, and the gap list is public.

## vixen and Bazel / Buck2

Bazel and Buck2 already have content-addressed caching, remote execution, and
hermeticity — at monorepo scale, in production, for years. The differences are
philosophical, and they compound:

- **Declared vs. observed.** Bazel's correctness rests on humans writing
  complete `BUILD` files; wrong declarations mean stale or over-built targets,
  and keeping them right is a staffed job. vixen's dependencies are *observed*
  — recorded from what computations actually read — so exactness doesn't
  depend on discipline.
- **Adoption cost.** Bazel asks you to restructure your repository around it.
  vixen reads your existing `Cargo.toml` — the build logic that understands
  Cargo is [written in the build language itself](/dev/constellation/rodin/),
  not baked into a binary you can't inspect.
- **Granularity, again.** Bazel's unit is the declared target; file-level at
  best. vixen descends to items and artifact sections — which is where the
  "skip 189 of 200 tests" numbers come from.
- **Observability.** When Bazel surprises you, you debug Bazel. When vixen
  surprises you, you [read the receipts](/using/receipts/) — the machine's
  actual decision trace, complete by construction.

What they do better today: scale-proven remote execution and multi-language
breadth. vixen's remote story is architecturally ready (same protocol locally
and across machines) but early-access ships single-machine first — again,
[status](/status/) is the honest ledger.

## The one-sentence version

cargo trusts timestamps; Bazel trusts your declarations; **vixen verifies
content and observes reads — so the cache is something you can trust without
maintaining it.**
