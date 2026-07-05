# Adjudication: fact-validity research reports (claude/gemini/gpt)

Three external deep-research reports on the widening research prompt
(docs/design/rodin-widening-research-prompt.md), adjudicated 2026-07-05.
Reports: claude.html (3.3k words, self-verified citations, most
rigorous), gpt.txt (3.3k, most technically penetrating on soundness),
gemini.rtf (1.4k, thinnest but concordant; one misattribution noted).

## Triple convergence (the strong signals)

1. BOTH MERGES CONFIRMED: (a)+(e) are one idea (interval algebra =
   instrument, lattice = frame); (c)+(f) are one idea (entailment
   monotonicity = theory, incremental-SAT = engineering literature).
   Four directions, not six. All three reports made the same merges
   independently.
2. THE MONOTONICITY ARGUMENT IS SOUND — it is entailment monotonicity
   (F ⊨ C ⇒ F∪G ⊨ C), bedrock; GPT gives the transfer proof in set
   form (Sol(M') ⊆ Sol(M) ⇒ Sol(M')∩R ⊆ Sol(M)∩R = ∅) which does NOT
   depend on boolean CNF — it transfers to Regions as sets of
   assignments directly. Lineage: Hooker 1993 (incremental SAT),
   SATIRE (DAC 2001 — learned-clause dependency tracking = support
   sets avant la lettre), Eén–Sörensson 2003 (selector literals).
3. THE HOLE — ALL THREE FOUND THE SAME ONE: closed-world premises.
   Monotonicity protects consequences of positive clauses; it does
   NOT protect "P@v has no dep Z" / "feature F does not exist" /
   version-universe enumeration premises. dep-added and feature-map
   additions are purely-additive edits that FALSIFY such premises.
   Since Rodin resolves features as propagated literals, any fact
   that forced a feature literal false via non-existence has a
   closed-world premise. Consequence: entailment-lifting applies
   only to facts whose proof leaves are all monotone-safe; the
   Premise enum must be audited kind-by-kind; negative feature
   literals in the support set are the danger markers. The 134
   feature-map co-occurrences in the taxonomy are the danger zone.
4. PER-EDGE (b) IS STANDARD TECH OUTSIDE RESOLVERS: verifying traces
   (Build Systems à la Carte), Adapton/Salsa read-tracing, provenance
   semirings (PODS 2007), DRed (SIGMOD 1993), differential dataflow.
   The classic soundness bug is UNDER-RECORDED SUPPORT; all three
   demand instrumented read-tracing through a single choke-point API
   (private raw accessors; replay re-reads through the traced
   interface — otherwise fact and proof silently agree and are both
   wrong). ABSENCE MUST BE A FIRST-CLASS SUBSCRIPTION: facts that
   observed "no edge Z" index under an absence key (P, ¬Z) so
   dep-added probes the absence index.
5. UNOCCUPIED TERRITORY CONFIRMED (again): no production resolver
   reuses learned facts across manifest edits. resolvo's
   "incremental" = lazy fetching within one solve; PubGrub reuses
   incompatibilities within one solve only. Closest real analog:
   GreenTrie (ISSTA 2015, symbolic-execution constraint caching) —
   returns cached UNSAT when new formula ENTAILS cached formula.
   Our §3 argument, deployed in a different domain. (Claude's find;
   the strongest single citation of the batch.)
6. (d) RESIDUAL WEAKENING RANKED LAST BY ALL THREE: no direct prior
   art (vivification/minimization assume unweakened base formula);
   value inversely correlated with lockstep-pin churn (exact-pin
   bumps annihilate residuals); only sound behind mandatory replay.

## The one real divergence

Ranking of (b) vs (c)+(f): claude puts entailment-transport first
(cleanest soundness), gemini+gpt put per-edge support first (biggest
measured win). All three agree they COMPOSE — (b) decides which
premises a fact watches; (c) decides which changes to those premises
are harmless. GPT's build sequence is the synthesis and the ruling
recommendation:
  1. premise-keyed support (instrumented traces, absence keys,
     inverted index premise→facts)
  2. exact subset/equality-on-used-region checks (VersionSet algebra,
     Cargo semantics exactly — probe-sampling is a measurement tool,
     NOT a proof rule)
  3. monotone strengthening transport (per-premise classifier:
     equal / stronger / weaker-or-incomparable / absent; replay gate
     with ONE new proof rule: VersionSet weakening, so lifted proofs
     mechanically replay)
  4. residuals last, behind the replay gate, if ever.

## Sharpest technical refinements to adopt

- GPT: RegionProjection(fact,Y) ∩ delta = ∅ is NOT a sufficient lift
  test — a proof can consume Y-intervals in an empty-intersection
  derivation without Y appearing in the final no-good region. The
  correct per-operand criterion: instrument interval operations in
  the proof DAG; an edit is harmless for a node iff O∩UsedSet ==
  N∩UsedSet (or emptiness-preservation for disjointness nodes).
- Claude: the proof format needs exactly one new rule — VersionSet
  WEAKENING (enlarge a set appearing positively) — so old proofs
  replay under narrowed premises by prefixing a weakening step.
  Without it, replay-verification rejects semantically-valid lifts.
- Claude: per-edge cert form: (must-exist, upper-bound VersionSet)
  per supporting edge + absence keys; certs COMPOSE (meet of two
  facts' certs = cert of anything derived from both) so Derived
  nodes get certs free during replay.
- GPT: optional deps are GUARDED IMPLICATIONS (guard → req), never
  bare edges; resolver-v2 feature-unification context (target/kind,
  weak dep?/features) must be part of the premise identity.
- Taxonomy payoff, restated: the 417 ^0.x.y→^0.x.(y+1) narrowings
  (single most common churn) become NO-OPS under transport; 708
  narrowed + 125 required-adds ≈ 79% of dominant changes are
  monotone-safe (modulo closed-world audit); the 7.29× edge-locality
  multiplies on top.

## Report quality notes

- claude: self-verified citations in-session; flagged its own two
  unverified items (version-ranges API names; PacSolve venue).
  Falsifiers drawn from our own data (async-graphql optional churn,
  the rename break, lockstep pins). Best of batch.
- gpt: best soundness conditions (per-operand instrumentation,
  guarded implications, mixed-break falsifier). Treated our
  measurements as input-not-verified — correct posture.
- gemini: concordant but thin; MISATTRIBUTION: "PubGrub (Nelson,
  2020)" — PubGrub is Natalie Weizenbaum (2018). Useful tempering
  note: disjointness-lifting degrades to invalidation on
  incompatible bumps (the delta is huge on the dominant 553-instance
  pattern), so (a) alone buys little — consistent with the 1.02×
  field-narrowing measurement.

## Verdict

The four directions are ONE MECHANISM in three layers (support
tracing → semantic classification → entailment transport), with
residuals as an optional fourth. It is the exec engine's own
observed-read-set doctrine applied to solver facts — Rodin's fact
store becomes a memo store keyed by observed reads, invalidated by
semantic (not byte) change. Prior art exists for every layer
SEPARATELY; the composition across manifest edits in a resolver is
unoccupied. This upgrades, not merely preserves, the R2 novelty
claim. Implementation awaits Amos's go.
