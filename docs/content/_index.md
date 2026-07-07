+++
title = "vixen"
+++

**Your build tool does too much work, and you can't tell why.** A comment edit
triggers a rebuild. A dependency bump reruns every test. CI spends twenty
minutes proving nothing changed. And when the cache *does* hit, a small voice
asks whether you can trust it.

vixen is a build system for Rust — it reads your existing `Cargo.toml`,
resolves like Cargo resolves (verified against Cargo itself), and drives real
`rustc` — built on one structural difference: **it observes what every build
step actually reads, and identifies everything by content.** Not timestamps.
Not hand-declared dependency lists. Observed reads, hashed content. That's the
whole trick, and everything below falls out of it:

- **"Why did this rebuild?" has a real answer.** Every decision leaves a
  [receipt](/using/receipts/) — the demand trace — so surprise rebuilds are a
  lookup, not folklore.
- **Change a comment — nothing rebuilds.** Identity is content; unchanged
  items are unchanged, [provably](/proof/).
- **A dependency changed — only the tests that can reach it rerun.** Computed
  from the compiled artifacts' own reference graphs,
  [with an audit mode](/using/test-selection/) that proves skips sound.
- **A cache hit is a proof, not a hope.** Builds run hermetically: an
  undeclared read *fails loudly* instead of silently poisoning the cache.

## Where does it hurt?

- **Slow CI?** → [What happens to my CI?](/using/ci/)
- **Slow local edit-build loop?** → [Try it on your repo](/using/quickstart/)
- **"Will my Cargo setup even work?"** → [Compatibility](/using/compatibility/)
- **Coming from Bazel or Buck2?** → [Why vixen?](/why/)
- **"Prove it."** → [The proof page](/proof/) — real numbers, real receipts,
  fixture-scale honesty included.

vixen is in **early access**: [what works today](/status/) is dated and
specific, [what breaks](/using/limitations/) is public, and
[when *not* to use it](/using/when-not/) is a page we wrote on purpose.
The machinery underneath has [its own site](/dev/).
