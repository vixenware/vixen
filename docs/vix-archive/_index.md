+++
title = "vix"
description = "The vix content-addressed incremental demand machine — language, machine internals, and the in-progress machine redesign."
+++

Design docs for **vix**, the content-addressed incremental demand machine, and
its host (the "machine": store, memo, read-sets, lowering to weavy).

## Active

- [Machine redesign inventory](/machine-redesign/) — what `driver.rs` does today
  (cited), the design verdicts (V1–V10), what weavy/phon already provide, and the
  target form.
- [**Machine redesign — per-problem analyses**](/redesign/) — deep,
  individual change/add/remove/how-it-works for each major problem, for approval
  before any code changes. **This is the set to annotate.**

## Reference

- [Vix Checker Spec](/checker-spec/)
- [Cargo Manifest Build Lane](/cargo-manifest-build/)
- [Where a type is declared: Rust or vix](/where-values-are-declared/)
