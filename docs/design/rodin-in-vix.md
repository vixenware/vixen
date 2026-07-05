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

## Correction #3 (from vix/docs/reuse-analysis.md, open side)

The "solve interior is FLESH" granularity story above used
vocabulary, not shipped mechanism: the in-repo flesh/spine terms
refer to fleet code-shipping, and ValueStore has NO refcount or
ownership tracking today — the frame-local-vs-interned value split
DOES NOT EXIST YET. The reuse design therefore has a prerequisite:
build the FLESH TIER — mutable, frame-local, pre-intern aggregate
buffers that become immutable StoreEntry values only when crossing
a demand boundary (return/capture/demand/trace); uniqueness lives on
the unhashed flesh header and is DISCARDED at intern (unlike taint,
which is content-derived and folds into identity — the distinction
matters and is now documented). Reuse can never touch interned
values: dedupe means shared by construction.
Chosen reuse design: HYBRID — static last-use analysis in lower.rs
(at the existing Layout pass, zero annotations) marks candidate
sites; only those sites emit a dynamic refcount-check stencil
(mutate-vs-copy, reusing weavy's multi-successor guard mechanism).
SPIKE B LADDER (now concrete): flesh tier → reuse stencils →
transcribe rodin-pure-kernel.md's propagate/learn pseudocode →
benchmark vs rodin-core (naive / reuse / Rust-Vec ceiling; the
branch-clone O(1)-vs-O(|state|) claim is measurement #1).
Note the flesh tier is independently valuable: every vix program
currently interns aggregates eagerly; frame-local construction cuts
store pressure across the board.
META: three position-doc claims about our own code were falsified by
design agents in one day (lazy VSIDS, trail/watch existence, flesh
tier). Rule reaffirmed: cite code, not memory, when banking.

## FLESH-TIER SPIKE B: MEASURED RESULT (2026-07-05, stax profiled)

Bench (interpreter lane, Lane::Interp): naive-copy 15.3ms, reuse
14.6ms (~5% faster), Rust Vec 8us. Viability bar NOT met on first cut.
BUT — Amos asked "did it use stax?" It hadn't; neither had I; and my
asserted diagnosis ("~1800x = interpreter-dispatch signature") was
FALSIFIED by the actual profile:

  stax top --sort self: sha2::sha256::compress256 = 327ms / 381ms
  = 86% OF ALL CPU IS SHA-256 HASHING during intern_flesh_word ->
  intern_value_bytes_children (recursive) -> alloc_array_words.

The bottleneck is CONTENT-ADDRESSED INTERNING, not dispatch, not copy.
Reuse barely helped because reuse eliminates the COPY, but the value
still crosses an intern boundary and gets FULLY RE-HASHED. A CDCL
trail grows monotonically and is interned each step -> each intern
re-hashes the whole O(N) structure -> O(N^2) hashing. Copy was never
the cost; per-intern full-tree sha256 is.

CONSEQUENCE (reframes the bet AND my proposed next step):
1. The flesh tier v1 attacked the WRONG AXIS (copies). The real cost
   is content-addressing every intermediate.
2. My proposed "run it on the JIT lane" would ALSO be sha256-bound —
   the JIT removes dispatch, but interning-per-step hashes regardless.
   So the JIT bench alone would NOT have revealed or fixed this.
3. THE ACTUAL FIX DIRECTIONS: (a) keep the solve interior FLESH and
   UN-INTERNED across the whole inner loop (the pure-kernel design's
   "flesh, JIT'd, untraced" — intern once at the demand boundary, not
   every op); (b) INCREMENTAL/MERKLE hashing so pushing one element
   updates the digest in O(1)/O(log N), not O(N) re-hash; (c) both.
   Flesh-must-not-intern-in-the-hot-loop is the load-bearing one.
CORRECTNESS WIN STANDS: reuse is UNOBSERVABLE (oracle green) — value
semantics preserved. That safety property is done regardless of perf.
DOCTRINE NOTE: measure, don't guess. My "interpreter signature"
assertion was wrong; stax showed hashing. Amos's question caught it.
