# Prior-art adjudication — vix & rodin

*Synthesis of three independent adversarial scrubs (Gemini, Claude
Research, GPT) run against the kill-format prompt on 2026-07-04. Each
was instructed to kill claims C1–C7 (vix) and R1–R2 (rodin), quote
evidence, and report search trails. This document adjudicates their
disagreements and records what survives as claimable, what is ceded,
and what must be read before publishing anything.*

*Trust weighting: Claude and GPT cite real, checkable papers
(PubGrub, gg, BuildXL docs, Vesta SRC papers, RIKER, IncPy). Several
of Gemini's quotes look confabulated (a "sota.io 2026" Unison deploy
post; a "Vesta manual" provenance quote matching no known Vesta text).
Where Gemini alone kills, treat as unverified.*

## Scoreboard (adjudicated)

| Claim | Verdict | One-line resolution |
|---|---|---|
| C1 full combination | **SURVIVED** (3/3) | No single system: pure language + never-materialize demand + every-call content-hash memo + durable cross-machine journal + observed-read-set effects. |
| C2 closure-hash identity | **KILLED as worded** — Unison | Cede the identity mechanism. Rewrite: "every call as automatic durable memo boundary in a build runtime" — that survives (Value.memo is opt-in). |
| C3 demand tunneling | **WOUNDED; core SURVIVED** | Nix laziness stops at derivation granularity; Vesta's path-deps are cache precision under call-by-value. MERGE tunneling (one path of a merged tree → only the contributing producer) found nowhere. |
| C4 two-tier + negative lookups | **KILLED on mechanism** — BuildXL | Weak fingerprint × observed pathset incl. absent probes + dir enumerations is BuildXL, verbatim. Survives: the soundness posture (VFS mediates all reads → bugs cause only false negatives) and call granularity. |
| C5 journal as attestation | **NEAR-KILLED** — gg + Trustix | gg's "memoization assertions" are signed thunk-hash→value-hash provenance; Trustix is a Merkle log of build input/output pairs used as a cache-trust index. Survives: external observations as journal entries; lockfile = journal slice as semantic identity. |
| C6 command-line grammars | **WOUNDED; core SURVIVED** | goma/reclient input processors and ccache parse CLIs for cache identity. Unfound: commuting-flag normalization for key equality + sandbox ceilings derived from grammar roles, one grammar as single source of truth. Patents unchecked. |
| C7 in-flight join | **KILLED on the ingredient** — GHC blackholing | Same-heap-thunk joining is decades old; Skyframe/DICE join declared node keys. Survives: joining independent computations discovered by content hash, fleet-wide. |
| R1 queryable resolution + counterfactuals | **SURVIVED** (3/3) | PubGrub derivation graphs exist for FAILURES only; no resolver ships selected-version kill-derivations or counterfactual re-solve-with-manifest-diff. Kill vector left unsearched: unsat-core/MUS-extraction literature. |
| R2 widening via read-set stability | **SURVIVED** (3/3, "deeply novel") | PubGrub is CDCL-with-ranges (cede that); widening a learned incompatibility across neighbor versions justified by VERIFIED read-set stability of metadata fetches exists nowhere found. |

## What this changes

1. **merge-demand.vix is the novelty demonstration, not just the
   acceptance test.** The one language-semantics claim every scrubber
   failed to kill is precisely what THE MILESTONE demonstrates: demand
   one file of a merged output tree, the non-contributing producer
   never materializes, proven by trace absence. When we publish, the
   demo IS the argument.
2. **Stop claiming the ingredients; claim the composition and the
   four surviving cores.** Claimable, in strength order: R2's
   verified-stability widening; C3's merge tunneling; C1's
   composition; R1's counterfactual product surface; C6's
   grammar-as-single-source; C4's soundness posture (design argument,
   not mechanism).
3. **Positioning sentence writes itself now**: Vesta is the ancestor
   (call-by-value, bridges, no demand semantics), Unison is the
   sibling on identity (opt-in memo, no build effects, no read-sets),
   BuildXL is the sibling on effects (action granularity, no
   language). vix is the point where those three lineages intersect,
   and none of the three can reach it by a small step — Unison lacks
   the effect layer and every-call memo; BuildXL lacks the language;
   Vesta lacks demand.
4. **Skyframe's restart-not-suspend model** (missing deps restart the
   SkyFunction; state-keeping is an ongoing retrofit) independently
   confirms that our "suspension ONLY for started-and-blocked, frames
   as the whole cost" ruling is a real differentiator, not just an
   internal aesthetic.

## Mandatory reading before publishing (the second-pass list)

- **Vesta book** (Heydon/Levin/Mann/Yu), caching + fine-grained
  dependency chapters. Open question that decides how much of C2's
  rewrite is safe: does Vesta's cache key cover the closure's CODE
  transitively (likely yes for models) and TYPES (unknown)?
- **gg**: "From Laptop to Lambda" + the earlier draft ("A Thunk to
  Remember") for memoization assertions. Closest thing to our journal
  entries; decide whether C5 is worth claiming at all afterward.
- **PIE / Pluto** (Pluto OOPSLA 2015; PIE ‹Programming› 2018; ASE
  2018; Konat's TU Delft thesis). The modern academic echo of "a
  language whose evaluator is an incremental persistent build
  system." Deltas hold today (task-boundary keys, declared `require`
  deps, local persistence, bottom-up capable) but this is the
  literature a reviewer will cite first.
- **Mill** code-hash invalidation (callgraph reachability) — nearest
  industrial cousin to closure-hash blast radius.
- **Trustix / laut** — the C5 adjacent in the Nix world.
- **BuildXL wiki + CloudBuild paper** (Esfahani et al., ICSE-SEIP
  2016) + Microsoft build-cache patent family — C4/C6 patent risk
  lives here; no scrubber ran a patent search.
- **Unsat-core / MUS extraction for version solving** — the one
  unsearched kill vector against R1.

## Gaps all three scrubs share (honesty ledger)

No patent search anywhere. No Vesta citation-descendant chase.
Tvix/Snix, Fac, OMake, Jenga, redo unexamined. No mailing-list/HN
archaeology. IBM/Rational ClearCase's derived-object winking
(VFS-based build audit + cache substitution!) was not examined by any
scrubber — closest commercial ancestor to the VFS soundness posture;
add to the second pass.
