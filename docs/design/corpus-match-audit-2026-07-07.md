# Corpus match/divergence audit — 2026-07-07

Read-only cross-check of design + implementation work that landed on 2026-07-07
against the prior-art research corpus in `docs/design/`. Performed because a
large amount of design and implementation work landed in `~/oss/facet-cc`
(branch `rodin`, plus worktrees under `~/.paseo/worktrees/1t3lgrd0/` and
`~/.paseo/worktrees/0yo8z1dg/`) without the implementing agents having
consulted this corpus first. No files were edited and no state-changing git
commands were run to produce this audit.

## Method

1. Read the corpus (list below) in full or in relevant part.
2. Read the day's landed work: `~/oss/facet-cc/RESURRECTION.md` and `NEXT.md`,
   plus the specific commits/branches/worktrees named for each of the four
   areas under audit.
3. For every claim compared, assigned one of four verdicts:
   - **CONSISTENT** — corpus and landed work agree; both sides cited.
   - **DIVERGENT-BENIGN** — different approach, but not incompatible.
   - **CONTRADICTS-ADJUDICATION** — the corpus explicitly decided otherwise;
     corpus decision quoted/cited, actual landed work described.
   - **CORPUS-SILENT** — genuinely new ground the corpus never addressed.

This audit was produced with the help of a delegated research subagent
(codex/gpt-5.5, read-only, auto-review mode) that did the corpus + repo
reading and drafted the tables below; the findings here are that agent's
output, reviewed and relayed by the orchestrating session.

## Corpus documents consulted

Read in full or targeted depth:

- `prior-art-adjudication.md`
- `rodin-fact-validity/adjudication.md` (+ skimmed raw inputs it adjudicates:
  `rodin-fact-validity/claude.html`, `rodin-fact-validity/gemini.rtf`,
  `rodin-fact-validity/gpt.txt`)
- `rodin-widening-research-prompt.md`
- `rodin-warm-facts-derivations.md`
- `march-solver-recovered.md` and `march-solver-recovered-verbatim.md`
- `rodin.md`
- `engine-demand-semantics.md`
- `vix-substrate-vfs-sandbox.md` and `vix-substrate-vfs-sandbox-addendum.md`
- `artifact-intelligence.md`
- `infra-as-demand.md`
- `store-architecture.md`
- `vx-store-as-vix-memo.md`

Skimmed for relevance (on-topic portions checked, otherwise ruled
out-of-scope for this audit): `agent-resurrection-note.md`, `rodin-in-vix.md`,
`rodin-block-forensics.md`, `rodin-c-evidence.md`, `rodin-churn-taxonomy.md`,
`taste-record.md`, `vix-language-design.md`, `vix-spec.md`, `vix-synthesis.md`,
`infra-port-recon.md`, `fission-fskit-stuck-audit.md`, `machine-lowering.md`,
`secrets-as-sealed-values.md`, `seatbelt-strict-profile.md`,
`tooth-2-frames-abi.md`, `vixd-machine-migration.md`, and on-topic material
under `briefs/` and `sketches/`.

## Landed work examined

- **Conflict learning in `rodin.vix`**: commit `f0f74179a` on branch
  `rodin-tail-loops-learning`, worktree
  `~/.paseo/worktrees/1t3lgrd0/rodin-learning`.
- **Sandbox staging / observed read-sets**: branch `sandboxed-exec-stage-1`.
  Note: this was *not* found at the initially-guessed
  `~/.paseo/worktrees/1t3lgrd0/vix-exec-seam-audit` (that worktree is on
  branch `vix-exec-seam-audit`, a different piece of work). The actual
  location is `~/.paseo/worktrees/0yo8z1dg/sandboxed-exec-spine`, commit
  `17babfa4`.
- **Test selection via relocation-graph walks + ICF section hashes**: spike
  report at
  `~/.paseo/worktrees/1t3lgrd0/spike-reloc-walk/spikes/reloc-walk/REPORT.md`.
- **Molten reuse + tail loops**: worktrees
  `~/.paseo/worktrees/1t3lgrd0/molten-consume`,
  `~/.paseo/worktrees/1t3lgrd0/tail-loop`, and the
  `vix-unintern-hotloop` / `vix-unintern-profile` / `vix-unintern-2`
  worktrees, cross-referenced with `~/oss/facet-cc/RESURRECTION.md`.

## Executive verdict

The strongest contradiction is in Rodin conflict learning. Commit `f0f74179a`
implements useful learned-region mechanics in `rodin.vix`, but it rebuilds
the old declared-structure/effect-equality lane and omits the adjudicated
load-bearing pieces: observed read-set support, absence subscriptions,
derivation/proof replay, and proof-bearing warm-fact semantics.

Sandbox staging is mostly a benign staged slice: stage 1 is declared-input
confinement with loud undeclared-read failure; the corpus requires observed
VFS read-sets as the final substrate, and the landed types explicitly say
stage 2 replaces declared sets with VFS-observed reads.

Relocation-walk test selection is complementary/new ground. The corpus
anticipated artifact probes and binary facts, but not this exact
relocation-reachability plus atom-hash test selector.

Molten reuse + tail loops mostly land in corpus-silent implementation
territory, but they are consistent with the engine demand semantics:
intra-demand self-tail iteration is not a memo boundary, while non-tail
recursion remains `INVOKE`/memoized.

## 1. Conflict learning in `rodin.vix`

| Topic | Verdict | Corpus side | Landed side |
| --- | --- | --- | --- |
| Learned no-goods as regions | CONSISTENT | `rodin-widening-research-prompt.md:55-60` says facts are learned no-goods as `Region` plus support and replayable derivation; `rodin.md:76-82` says exclusions are learned regions. | `f0f74179a`, `rodin/rodin.vix:139-154` defines `Region` and `LearnedNoGood { region, active }`. |
| Point-region construction | DIVERGENT-BENIGN, with risk | `rodin.md:126-128` records the known gap: learned regions were fatter than support and v0.1 must minimize facts to derivation support. | `rodin.vix:885-901` builds support from conflict pkg plus selected guard parent, then adds the last hypothesis if absent. That is better than broad whole-state regions, but the "last hypothesis" add can still make the fact wider than strict proof support. |
| Declared-structure/effect widening | CONTRADICTS-ADJUDICATION if treated as R2 | `prior-art-adjudication.md:439-463` explicitly says declared-per-edge widening is real but "NOT the novel claim"; `:465-484` says R2 became real only when `ObservedReadSet` existed. `rodin-widening-research-prompt.md:74-88` distinguishes declared-structure widening from observed-read-set widening. | `rodin.vix:904-971` widens by finding same selected parent version and `clause_same_effect`; there is no observed read-set, no `ObservedReadSet`, no premise field/path witness. |
| Read-set support and absence keys | CONTRADICTS-ADJUDICATION | `rodin-fact-validity/adjudication.md:35-44` requires instrumented read tracing through one choke point and says absence must be first-class. `:66-75` recommends premise-keyed support first, then replay-gated semantic lifting. | `LearnedNoGood` is only `region, active` (`rodin.vix:152-154`). `learn()` (`:1034-1040`) installs facts without support traces, absence subscriptions, or replay material. |
| Containment-frontier install/deactivation | CONSISTENT | The corpus expects exclusions as region facts and warns against hash-equality identity; no specific table algorithm is mandated. | `rodin.vix:974-1031` uses region containment to skip already-covered facts and deactivate facts contained by the new region. This matches the region-subsumption intent. |
| Applying learned no-goods | CONSISTENT | `rodin.md:18-22` separates keepable facts from retractable hypotheses. | `rodin.vix:1081-1121` detects disjoint/pending/fully-entered/unit and narrows domains or conflicts from active learned facts. |
| Restart/backtrack with retained facts | CONSISTENT locally; not enough for warm facts | `rodin.md:44-47` permits keepable facts within a pinned world. `march-solver-recovered.md:101-105` says cached facts must be replayable before trust. | `rodin.vix:1235-1250` carries learned facts across failed candidates; `:1360-1374` retries seeded state with learned facts. Local retention is fine. Persisted/warm reuse would not be fine without proof replay. |
| Warm-fact/proof semantics | CONTRADICTS-ADJUDICATION | `rodin-warm-facts-derivations.md:194-219` requires scope gate, proof fetch, proof replay, separate widening witness, and install only after replay. `:306-326` rules: inline proof graph, real `RuleKind::Widen`, `WarmFactVerifier`, no-proof warm facts rejected by default. | Landed `LearnedNoGood` has no derivation/proof digest/status. `learn()` installs a naked region. This skips the prescribed proof-bearing fact shape. |
| Test evidence | CORPUS-SILENT on this exact test | The corpus asks for proof/replay, not merely a count. | `vix/tests/rodin_fixtures.rs:1467-1486` asserts one active learned no-good for a version conflict. Useful regression, but it does not prove read-set/replay semantics. |

**Bottom line:** the learning machinery is directionally useful, but it
implements the wrong proof boundary. The corpus did not merely suggest
read-set/proof replay; it adjudicated those as the mechanism. This landed
work should not be described as implementing the R2/validity adjudication.

## 2. Observed read-sets / sandbox staging

| Topic | Verdict | Corpus side | Landed side |
| --- | --- | --- | --- |
| Final read-set requirement | CONSISTENT target | `vix-substrate-vfs-sandbox.md:12-19` says the substrate leaf must produce observed, not declared, read-sets. `:298-314` says `Outcome` attaches `tracked_observations` from `vx-vfsd`. | `RESURRECTION.md:200-202` records the intended split: seatbelt/landlock confine-only; vx-vfsd observer; stage 1 confine, stage 2 observe. |
| Stage 1 declared set | DIVERGENT-BENIGN as a stage | Corpus says declared sets are not an acceptable final design (`vix-substrate-vfs-sandbox.md:12-17`). | `vx-services/src/runner.rs:142-143` explicitly says stage 1 uses allowed/expected reads and stage 2 replaces them with VFS-observed reads. |
| Actual implementation | DIVERGENT-BENIGN | `vix-substrate-vfs-sandbox.md:389-397` says the real gap is wiring `tracked_observations` into a real launcher. | `vx-exec/src/lib.rs:45-104` stages declared inputs, runs sandboxed process, harvests outputs, and returns `observed.into_read_set()`. But `ObservedWorld` records reads during staging (`:628-655`), not process VFS reads. |
| BuildXL/Detours lineage | CONSISTENT | `prior-art-adjudication.md:159-163` flags BuildXL syscall interception as adjacent but different from vix VFS mediation; `:104-111` credits Vesta/BuildXL for negative lookups and dir enumeration. | The landed stage uses sandbox confinement and staged mounts, not injected syscall interception. That does not contradict the corpus; it is a partial slice before VFS observation. |
| macOS first | DIVERGENT-BENIGN | Addendum says macOS deny-default seatbelt shape already exists and should be lifted into one-generator design (`vix-substrate-vfs-sandbox-addendum.md:46-51`). | `vx-exec/src/lib.rs:475-505` uses `seatbelt_exec`; non-macOS returns "implemented for macOS seatbelt in stage 1" (`:507-518`). This is narrower than corpus scope but acceptable for stage 1. |

**Bottom line:** no outright contradiction as long as nobody claims stage 1
is the observed-read-set substrate. It is a confine-only rung, and the
corpus's prescribed `tracked_observations` join is still unimplemented.

## 3. Relocation-graph test selection + ICF/atom hashes

| Topic | Verdict | Corpus side | Landed side |
| --- | --- | --- | --- |
| Artifact-level probing | CONSISTENT | `artifact-intelligence.md:3-8` says executables should be lazily probed into sections/symbols/dynamic deps; `:30-35` says probed ABI facts can feed Rodin instead of declared semver. | `REPORT.md:30-40` parses Mach-O objects/rlibs and libtest registration; `:44-62` defines symbol-bounded atoms and normalized atom hashes. |
| Test selection by relocation reachability | CORPUS-SILENT | March only names CI test selection/sharding as a possible Vixen domain (`march-solver-recovered.md:124-129`). The corpus does not specify relocation-rooted per-test selection. | `REPORT.md:64-91` gives the invalidation matrix and catches downstream generic monomorphization via changed atom content. |
| Relationship to BuildXL observed reads | DIVERGENT-BENIGN | BuildXL material is about observed file/path inputs and negative lookup pathsets (`prior-art-adjudication.md:20-28`, `:104-111`). | Reloc-walk is object-code evidence, not observed filesystem read tracing. It is complementary, not a replacement. The report itself says it is not a full predicted-read-set mechanism (`REPORT.md:117-123`). |
| Soundness posture | CONSISTENT | Store/artifact docs treat probes as claims-with-evidence, not magic truth (`artifact-intelligence.md:13-19`). | `REPORT.md:93-108` lists relocation-invisible behavior and says unresolved cases fall back to rerun (`:117-123`). That matches conservative claim discipline. |

**Bottom line:** this is not a contradiction. It is a useful new
artifact-intelligence instance, but it should be framed as an evidence layer
with fallback, not as the BuildXL/VFS read-set mechanism.

## 4. Molten reuse + tail loops

| Topic | Verdict | Corpus side | Landed side |
| --- | --- | --- | --- |
| Intra-demand self-tail iteration | CONSISTENT | `engine-demand-semantics.md:64-71` makes memo boundaries strict at `Call`; `:250-255` says suspension is only for started-and-blocked work and not spawn-everything. | `RESURRECTION.md:64-71` identifies the structural gap: every vix call was an `INVOKE` boundary, so no natural loop kept accumulators molten; tail loops are intra-demand iteration. |
| Lowering self-tail calls to jumps | DIVERGENT-BENIGN | Corpus does not prescribe tail-call lowering, but it requires one implementation, typed lowering, and trace-based behavior (`engine-demand-semantics.md:223-261`). | `tail-loop/vix/src/machine/lower.rs:3072-3088` routes tail expressions through `self_tail_call`; `:4015-4098` checks same function/ref, copies coerced args into param slots, and emits `Op::Jump` to `loop_header`. |
| Non-tail recursion remains demand/memo | CONSISTENT | Calls are memo boundaries (`engine-demand-semantics.md:68-71`). | `lower.rs:1639-1708` diagnoses non-tail self-calls as demand boundaries "not looped"; test `demand_driven.rs:1186-1210` asserts non-tail self-call still spawns per recursion. |
| Tail loops preserve child demand | CONSISTENT | Structural iteration and calls still demand children when selected (`engine-demand-semantics.md:82-87`). | Test `demand_driven.rs:1222-1241` asserts a tail-loop body's child calls are still demanded/spawned. |
| Molten in-place aggregate reuse | CORPUS-SILENT on exact mechanism | Corpus does not describe molten refcount/consume-receiver mechanics. It does require no dynamic-language machinery and typed lowering (`engine-demand-semantics.md:230-238`). | `molten-consume/vix/src/machine/lower.rs:2500-2510` records consuming rebind receivers; `:3835-3843` uses consuming move only when the receiver matches and has no pending lazy alias reads; driver reuse checks `entry.refs == 1` before mutating (`tail-loop/vix/src/machine/driver.rs:4567-4615`, similarly `:2990-3008`). |
| Intern-boundary observability | CONSISTENT | Memo key strictness demands args at call boundaries (`engine-demand-semantics.md:68-71`). | `RESURRECTION.md:64-66` says `INVOKE_HOST` interns molten args for the memo key. Tail-loop tests assert enabled trace is a strict subsequence of forced-`INVOKE` while result/read projection stays equal (`demand_driven.rs:1006-1140`). |
| March solver lineage | CORPUS-SILENT / not a spec | `march-solver-recovered.md:6-10` says March is recovered intent, not a decision. It does cover no-goods, fact/assumption split, replayable facts, and no solver DSL (`:59-70`, `:101-105`, `:344-358`), not molten/tail-loop lowering. | Tail loops are a Vix machine/runtime mechanism, not a March solver design commitment. No contradiction found. |

**Bottom line:** this area does not contradict the corpus. It mostly sits
below the design level the corpus discussed. The important boundary is
preserved: self-tail loops are intra-demand; non-tail calls remain memo
boundaries.

## Required reading before future Rodin/Vix design charters

- [`rodin-fact-validity/adjudication.md`](rodin-fact-validity/adjudication.md)
  (:35) — prevents naked learned facts without traced support, absence keys,
  and replay-gated proof transport.
- [`rodin-widening-research-prompt.md`](rodin-widening-research-prompt.md)
  (:74) — records the declared-structure vs observed-read-set distinction and
  why support granularity matters.
- [`prior-art-adjudication.md`](prior-art-adjudication.md) (:439) — prevents
  relabeling declared-structure widening as R2; records BuildXL/Vesta
  ancestry.
- [`rodin-warm-facts-derivations.md`](rodin-warm-facts-derivations.md) (:306)
  — records proof-bearing warm facts, `RuleKind::Widen`, `WarmFactVerifier`,
  and no-proof rejection.
- [`rodin.md`](rodin.md) (:76) — compact normative Rodin decisions: hypotheses
  vs facts, exclusions with derivations, fact scoping, support minimization.
- [`vix-substrate-vfs-sandbox.md`](vix-substrate-vfs-sandbox.md) (:12) —
  prevents settling for declared read-sets; records `tracked_observations` as
  the existing observer.
- [`vix-substrate-vfs-sandbox-addendum.md`](vix-substrate-vfs-sandbox-addendum.md)
  (:46) — prevents re-deriving macOS seatbelt as greenfield.
- [`artifact-intelligence.md`](artifact-intelligence.md) (:3) — frames
  binary/object probes as structured store facts; useful before reloc/RDR/
  test-selection charters.
- [`engine-demand-semantics.md`](engine-demand-semantics.md) (:64) —
  load-bearing for memo boundaries, laziness, trace contracts, and
  tail-loop/machine changes.
- [`march-solver-recovered.md`](march-solver-recovered.md) (:6) — useful as
  lineage and warnings, but explicitly not a spec; include it to avoid
  resurrecting rejected solver DSL/search constructs.

## Attention items

- Rodin learning should be labeled "local learned-region prototype /
  declared-effect widening," not R2 observed-read-set learning.
- Before any warm/persistent fact work proceeds, `LearnedNoGood` needs
  proof/support/widen evidence shape or a separate verified-fact wrapper; the
  current `region + active` shape is below the adjudicated bar.
- Sandbox stage 1 is okay only if the next charter explicitly wires
  `vx-vfsd tracked_observations` into `ExecOutcome`.
- Reloc-walk should stay in the artifact-evidence lane with conservative
  fallback; it should not be conflated with observed process read-sets.
