# Rodin in vix: the position (banked 2026-07-05)

## The question
Should Rodin be implemented in the vix language? Previously ruled out
(solver = tremendous mutable state; no user types back then). Re-opened
by the fact-validity adjudication: the #1 soundness condition across
all three research reports — complete read tracing enforced by
construction, replay re-reads through the traced interface — IS the
vix machine. Every problem this week has the same shape (dependency
tracking + memoization + observed reads): exec tier-2, VFS read-sets,
checker incrementality, solver facts. Rodin is the last holdout
hand-building what the platform does natively.

## The two-part answer
PART 1 (agreed, no gamble): fact LIFECYCLE moves to the machine.
Kernel stays a Rust capability (premises in as traced values, learned
facts + replayable derivations out as store values). Support tracing
= demand trace (under-recorded support becomes UNREPRESENTABLE).
Semantic classification = a SEMANTIC-CUTOFF COMPARATOR at the tier-2
verify seam (successor to projection-narrowed memo: structural →
semantic cutoff; "new premise entails old" = one VersionSet subset
test). Transport/replay = warm reload. Fleet-shared solver facts fall
out of store posture (facts are claims with replayable proofs =
trustless-bytes/claims-need-policy, verified by replay not trust).

PART 2 (the push): the kernel itself may not need to stay Rust.
CDCL's mutation is LINEAR, not essential:
- trail: single-owner push/pop → functional-in-place under uniqueness
  (Perceus/Lean-style; refcount==1 is a RUNTIME fact our store
  already tracks — we need not prove uniqueness statically)
- watch lists: single-owner derived index, same story
- VSIDS decay: the standard impl is ALREADY the functional trick
  (lazy global multiplier)
- clause DB: grow-only + forgetting = persistent append, arena-shaped
- subsumption: content-addressed clause identity = store dedupe
  NATIVELY (our own 113M-scan fire was 9,390 duplicates of 2 —
  the pathology was a missing content-address)

GRANULARITY (kills the old fear): solve interior is FLESH, not spine
— weavy-JIT'd function calls, not demand nodes, not traced per step.
Demand granularity sits at the fact level. Millions of propagation
steps = native code, zero tracing overhead.

THE ONE NEW COMPILER FEATURE: reuse analysis (Perceus-style drop-
reuse: consumed cell + same-shape construction = in-place). Published
technology (Koka/Lean), general value (any vix program with local
accumulation), squarely in the copy-and-patch/deopt wheelhouse.

THE REFRAMED PERF BAR: mutation was the only practical way when every
solve was COLD. Fact transport makes routine edits memo hits (the 417
^0.x.y narrowings → no-ops); the kernel runs only on novel conflicts.
A pure kernel 5-10x slower than rodin-core behind an incremental
layer that eliminates most kernel runs wins on every real workload.
Perf requirement moves from the inner loop to the memo layer — where
the platform is the product.

## Standing directive (Amos, verbatim spirit)
NOT accepting one failed spike as a verdict. Keep pushing until
there is absolutely no way in hell it works — us and all the research
assistants we can muster. Too perfect to give up on. rodin-core (Rust,
95.6% fidelity) is NEVER discarded: it is the oracle every step of
the migration is differentially checked against.

## The two lanes
1. Spike A (fact lifecycle): one fact class hosted as machine values,
   kernel as capability, semantic-cutoff comparator, differential
   oracle vs rodin-core on identical solves; warm-edit measurement on
   the taxonomy's own data.
2. Spike B (pure kernel): unit propagation in functional vix on a
   real workload slice (the 457-decision solve), measured honestly:
   naive interp → JIT → reuse-analysis estimate. Gap within the
   warm-solve envelope → kernel migrates too; Rust core remains
   oracle. Gap brutal → capability split stands, reuse analysis
   lands anyway as a general language feature.

## Corrections (2026-07-05, from rodin-pure-kernel.md, verified in code)

The position above misdescribed the kernel in two ways — both flagged
by the pure-kernel design pass and verified by direct code read:
1. There IS no trail / watch-list machinery (grep: zero matches).
   rodin-core is CSP-style domain narrowing over VersionSets with
   region conflict learning, and State is ALREADY clone-per-branch
   (lib.rs:2098). The kernel is far closer to functional than this
   doc feared: the port is representation-only for State; Solver's
   &mut fields are uniquely-owned per solve() (reuse-safe by
   construction); Derivation/Clause/Exclusion stores are ALREADY
   append-only; VersionSet/Region are ALREADY pure.
2. VSIDS decay is an EAGER full-map scan per conflict (lib.rs:2808),
   not the lazy-multiplier trick — i.e. today's Rust leaves a known
   optimization on the table that the functional port can take.
Also sharpened: content-address dedupe covers only identical-region
duplicates; subset-containment subsumption remains real work.
Persistent structures make branch-clone O(1) vs today's O(|state|)
deep clone — the port may be FASTER on the branchiest path; this is
Spike B's first falsifiable measurement. Break-even slowdown
recomputed from taxonomy data: ~4.8x. The one genuine language gap:
VersionSet/Region as declared value kinds — ALREADY IN FLIGHT on the
open side (vix-semantic-cutoff branch builds VersionSet now).
