# The scheduler(s)

*Private. From a design conversation with Amos, 2026-07-09 (Opus 4.8). Amos:
"unironically the scheduler is the hardest part of vixen… as much as rodin feels
like a challenge, the scheduler(s) are the part that make it real outside of
local demos."*

Per `open-proprietary-division.md`: **the semantics are open, the plural is the
product.** This document is the plural. The one part of it that belongs *open* is
§2's constraint on the cachet — that's trust surface — and it is also stated in
the public note `/vix-design/where-a-build-runs`.

---

## 0. What makes it hard, stated exactly

**It is the only component with no as-if backstop.**

Every other part of the machine is allowed to be wrong. A bad location costs a
recompute. A bad island partition costs bytes. A wrong warmth estimate costs a
cold read. Each of those is protected by a sentence that says *the scheduler will
absorb it.* Nothing absorbs the scheduler. It **is** the cost plane, and an error
there is an invoice, not a slow test.

Its inputs are the worst kind: hyperlocal facts (this laptop, that Mac Studio on
the LAN, the battery), multiple runner sources (some free, some metered),
multiple stores holding **disjoint** objects, geographic distribution, legal
requirements, and a fleet that changes while the graph is running. Part of it must
run locally (it is the only thing that knows the hyperlocal facts); part of it
must run in the cloud (identity, ACL, billing).

## 1. The gift: it cannot be wrong about correctness, so it needs no consensus

Placement is value-irrelevant. Work is idempotent. Duplicate work is not a
conflict — it is a duplicate. Monotone propagation is confluent (banked, the
parallelism doctrine).

Therefore the scheduler **never needs to agree with itself.** Two nodes
independently computing the same demand is fine. A partition that splits the
fleet is not an outage. A stale capability advertisement produces a rejected
dispatch, never a corrupt artifact.

So it may be a gossip layer: eventually consistent, partition-tolerant,
approximate, racy — and it cannot produce a wrong build. Kubernetes needs
consensus because a pod is a *place* and placement is the state. Our demands are
*values*, and their place is nobody's business.

### The proprietary boundary is the consensus boundary

Now look at what genuinely requires consensus: **identity, ACL, billing, the
transparency log.** Things that must not be double-counted, must not be forged,
must be totally ordered.

That is exactly the list of what lives in the cloud control plane, and exactly
the one thing that stays proprietary.

> **The proprietary boundary is the consensus boundary.** Everything that needs a
> leader goes in the control plane; everything else gossips.

This is not a business decision that happens to align with the architecture — it
is the architecture explaining why the business decision is right. It also tells
you precisely what a self-hosted fleet can do without us: **everything except
billing.**

### Three schedulers, not one

- **Local** — the demand root. Always on the user's machine. Knows the hyperlocal
  facts and never asks permission to know them. Must work fully offline.
- **Fleet gossip** — capability advertisements, store contents, warmth, load.
  Monotone, leased, no consensus, partition-tolerant.
- **Cloud core** — identity, ACL, billing, transparency log. Consensus. Small.
  Boring. Proprietary.

## 2. Policy is not the recipe

| source | says | may change the value? |
|---|---|---|
| the recipe (`.vix`) | what the value *is*; which capabilities, implicitly, via its commands | **yes — the only thing that can** |
| the capability | what a tool is, and how it is identified (`rustc -vV`) | no |
| the policy (vixen) | hard constraints and soft preferences over nodes | no |

**"No programmer control" was never "no operator control."** The islands doctrine
shuts the door on a *program* steering placement, because a program that steers
placement can make its value depend on where it ran. It says nothing about the
operator. Policy — a committed, versioned, readable styx artifact — may be as
bossy as it likes, precisely because it cannot change the value.

- **Hard constraints** (feasibility; a violating plan is *refused*): capabilities
  (derived from the recipe's commands), data residency, tenancy, budget ceiling.
- **Soft preferences** (an objective function): price class, latency, battery,
  locality.

Data residency is the interesting one: it does not change the value and it has
legal force. **Placement is compliance-relevant while remaining
value-irrelevant.** That is the seam the EU sovereignty positioning sits on.

### The receipt splits, and the cachet must not see placement

- **Semantic receipt** — read-set, input identities, toolchain identity.
  Placement-independent. **This is what the cachet seals.**
- **Operational record** — node, provider, region, duration, RSS, bytes moved,
  cost. Placement-dependent. This is the invoice and the residency audit.

> **If placement leaks into the cachet, two identical builds attest
> differently.** Two runs of the same recipe must produce the same cachet.

That property *is* the product. It is also a tripwire: for any field anyone
proposes to seal, ask whether a different node would have changed it.

*(This constraint is trust surface and belongs in the open docs too.)*

### AMENDMENT (Amos): the invoice is placement-independent too

Amos: *"we can shield customers from our placement mistakes by fixing their costs
to what an optimal scheduler would've done (with some margin)."*

An earlier draft of this note said "same cachet, different invoices." That is
weaker than what we want, and wrong. **Same cachet, same invoice.** Placement
cannot change the value; it must not change the price either. Two builds of one
recipe — one on a laptop, one across three continents — cost the same. No cloud
vendor offers this; variable bills are the thing every customer hates.

It also DELETES an invariant this note previously carried ("never double-count a
billable unit" under chaos / speculation / partition-heal). That whole bug class
cannot exist if **the invoice never sees execution**. Race two linkers, eat both,
deliver the latency.

**But do not price against "what an optimal scheduler would have done."** The
optimum is NP-hard (uncomputable in the contract) and unauditable (the customer
cannot check it). Price on the graph instead:

> **price = f(the demands actually recomputed, a published rate card)**

A rate card keyed by command identity — measured core-seconds per command class,
learned from our own receipts. Summed over the demands that had to be *computed*
rather than reused. Deterministic, reproducible, auditable against a published
table, placement-independent by construction, and reuse visibly shrinks the bill:
**the product's central claim becomes a line item.**

Our margin is exactly the gap between the rate card and what the fleet really
cost — which is precisely what the scheduler should be optimizing.

### Nobody discovers a sixty-euro build afterwards

Not by adding a verb to the program — that puts placement in the semantic plane
and costs reproducibility. Three things instead:

- **Predict, don't observe.** The scheduler knows each island's capability set
  before dispatch. `vx explain <demand>` prints the plan — islands, required
  capabilities, admissible nodes, chosen node, why, predicted cost — without
  running it.
- **Veto, don't steer.** Budget ceilings, provider allowlists, residency are hard
  constraints evaluated at planning time. The planner refuses, or asks.
- **Trace everything.** Money raises observability from a nicety to a product
  requirement.

## 3. Do not right-size the cores. The question is ill-posed.

Nobody can answer "how many cores should this job get." The speedup curve is
sublinear and workload-dependent, and the numerator — developer time — has no
price. There is no Pareto frontier to sit on.

**Dial it up.** Give the job the 64-core worker, in small time slices, and make
the scheduler good enough that sub-second jobs are economical. Billing is
per-core-second; a wide burst for two hundred milliseconds is cheap. The objective
is not per-job efficiency, it is **makespan under hard constraints**: throw width
at the critical path, let everything else fill the gaps.

What *is* worth learning from receipts:

- **Duration** — to order the critical path. Wrong: suboptimal order.
- **Read-set** — to know where the bytes are. Wrong: a cold read.

Neither can make an answer wrong. Both are cost-model plane.

**Calibration, so nobody spends a year on an optimizer:** greedy list scheduling
by critical path is within (2 − 1/m) of optimal makespan on *identical* machines
with *no* communication cost (Graham, 1966). Heterogeneity and transfer costs
break the bound outright — it is not a guarantee here. The lesson survives the
loss of the theorem: **don't build an optimizer. Build good facts, a decent
priority function, and flawless observability**, and spend the difference on the
simulator (§6).

## 4. Outputs are memories that fade

Build outputs are content-addressed, so eviction is always safe: a miss costs
latency, never staleness. A blob is *warm* on a node that recently produced or
read it, *cool* in a store, *gone* when evicted — and recomputable at every stage.

> **The read-set is a placement oracle.** Score a node by the warmth of the blobs
> the demand is predicted to read — a prediction the previous run's receipt
> already contains. Wrong prediction: a cold read. Never a wrong answer.

Which makes placement structurally identical to the location plane's **candidate
nomination**: content-free, known at demand time, freely revisable, wrong only in
the cost plane. Two nominations, different scales, one law —
*nomination-never-validation.*

### Sticky by default

Placement decisions must be cheaper than sub-second jobs, and a node that just ran
your job holds your memories.

- **Stay on the node.** Migration is the exception and needs a reason: a
  capability the node lacks, or a large cold input that lives elsewhere.
- **Back-to-back on the same node** is the default for a run of islands. It is the
  locality optimization and the tenancy boundary at once.
- **Tenancy is a hard constraint**, not a preference. A node that ran your build
  holds your source in its VFS. It must not also be running someone else's.

## 5. Two superpowers most schedulers cannot use

**Speculation is free.** Duplicate work is never wrong and kill-anytime is always
sound, so we get MapReduce's backup-task straggler mitigation with no correctness
argument to make. Run the linker on two nodes, take the first, kill the loser.
Nobody else can do that without a fence.

**Recompute-versus-fetch is a decision, not a fallback.** For Bazel or Nix, an
object missing from your cache is a fetch, or a miss. For us it is a *cost
comparison*: bytes over the wire against CPU on a warm node — because we always
have the graph to re-derive from. **A partitioned store stops being an outage and
becomes a price.** Absence is properly scoped, too: never "this object doesn't
exist," always "store S didn't have it at time T," which is exactly the
first-class Absence witness in `warm-facts-spec.md`.

## 6. It has a correctness oracle. It has no performance oracle.

**CORRECTION (Amos, same conversation):** an earlier draft of this section said
the scheduler has no oracle at all. That hides the easy half.

**The local build is a perfect correctness oracle.** Run the graph on one
machine; keep the outputs. Run it across a fleet under any placement, any policy,
any failure, any speculation — the outputs must be **bit-identical**. Placement is
value-irrelevant; that is the whole design.

So chaos testing here is stronger than any distributed system normally gets. Most
can only assert *liveness* under fault injection ("it eventually finished"). We
assert **equality**. Kill nodes mid-compile, expire advertisements, partition the
store, race two linkers: same artifact. `FOUNDATION.md` already mandates `--chaos`
from rung 001 for the language; for the scheduler it is not a rung, it is the
primary test.

Testing therefore splits three ways:

- **Correctness** — differential against the local build, under maximal chaos.
  Deterministic. Bit-identical.
- **Constraints** — invariants (below). Checkable on every run, forever.
- **Performance** — *here* there is no oracle. Only a distribution of outcomes.

### Three instruments for the performance half (Amos)

**Scenarios are the ratchet.** Deterministic, one per doctrine, regression tests
rather than benchmarks because the simulator is deterministic:

```
two nodes, one has rustc                -> the rustc island goes there
policy: EU-only; US node idle and free  -> US receives nothing
inputs warm on A, cold on B, else equal -> A
five dependent islands, all placeable   -> all five land on one node (sticky)
one of 200 compiles is 100x slow        -> speculation fires; loser killed
node dies mid-job                       -> demand re-placed; billed ONCE
object only in an unreachable store     -> recompute, don't stall
plan exceeds budget ceiling             -> refused before dispatch, not after
advertisement expires mid-graph         -> no dispatch to that node
```

**The game is a discovery instrument, not a test.** Scheduler pathologies are
legible to an eye and illegible to a metric — convoying, thrashing, starvation, a
node that goes cold over and over, a straggler nobody speculated on. You see them
in an animated Gantt view instantly; you would have to already suspect them to
measure them. Its output is **new scenarios**, not pass/fail. Second value: *if
you cannot draw the scheduler's state, it is too complicated.* The game is a
forcing function on the design.

**Real-world replay is the only thing that can tell you the model is wrong.** A
simulator validates the scheduler against your beliefs; every bug the model cannot
express, it certifies.

### The simulator has an oracle, even though the scheduler does not

> **You cannot validate the scheduler. You can validate the environment — and then
> the environment validates the scheduler.**

Every receipt records the operational trace: node, provider, duration, RSS, bytes
moved. Feed a recorded trace's *inputs* into the simulator and ask whether it
predicts the *observed* makespan within tolerance. **Reality is the oracle for the
simulator.** Ratchet it: simulator fidelity is a score — how many recorded fleet
traces does it reproduce within 10%? Until that number is respectable, scenario
outcomes are fiction, and everyone should know it.

The traces come from running the **rust-rdr / cargo-fork workflow** (the stated
priority demo target) as a standing demand, forever, on our infrastructure. The
demo, the benchmark corpus, and the simulator's oracle are the same program.

### Speculation is free in correctness, and free to the customer

Two nodes race the linker; we eat both. Under graph-priced billing (§2 amendment)
the invoice never sees execution, so the "never double-count a billable unit"
invariant this note once carried is not enforced — it is **unstateable**.
Duplicate work costs margin, never a customer.

### Tier-2: policy search over the model, and the one way it fails

Amos: *"we can simulate builds without actually running them... compress time...
do that millions of times... feed models so they can suggest policy changes. Not a
classical optimizer per se, but not too far either."*

That is reinforcement learning against a simulator, whatever we call it, and it has
one famous failure mode: **you will learn to exploit the simulator.** RL finds the
model's bugs before it finds the world's structure, and it looks exactly like
success until deployment. Two guards, both already present:

- **The fidelity ratchet is load-bearing, not hygiene.** Held-out real traces the
  policy never trained on, scored predicted-vs-observed makespan. A simulator that
  cannot reproduce a build it has never seen is judging itself.
- **Learn the soft objective. Never the hard constraints.** Feasibility
  (capability, residency, tenancy, budget) is structural: the policy chooses among
  admissible placements and cannot enumerate an inadmissible one. A learned policy
  may be arbitrarily bad and can never put EU data in Virginia. The hard/soft split
  of §2 is exactly the safe/unsafe split for learning.

**Methodological rule: vary the policy and the fleet, never the workload.** Take
real graphs out of tier-0 receipts and simulate different worlds around them. Generate
synthetic build graphs and you are optimizing against a distribution that does not
exist, and you will never find out.

### LLMs on traces: nominations, not verdicts

Amos: *"Here's a trace, figure out what was suboptimal."* Good — pathologies are
pattern recognition over a legible artifact and need no ground truth. But an LLM's
finding is a **nomination**: it becomes a scenario, and the deterministic simulator
confirms or refutes it. Otherwise we accumulate plausible stories about why last
Tuesday was slow. Nomination-never-validation, at the meta level. It also requires
the trace to be legible to a reader — the same forcing function as the game.

### Why a learned policy is responsible here at all

Correctness is protected because placement is value-irrelevant. The customer is
protected because the invoice is graph-priced. Therefore a bad learned scheduling
policy can produce exactly one kind of damage: **our own margin.**

And the actual-vs-ideal delta is simultaneously the margin we manage and the reward
we train on. The same number.

### The original framing, retained

Everything else in this system has one. Rodin has `cargo tree`. Manifest ingestion
has `cargo metadata`. Unit derivation has `--unit-graph`. Version ordering has the
semver crate. Amos's method is oracle-driven, and it is why the corpus work has
been fast.

**You cannot diff a placement against a reference.** There is no correct answer,
only a distribution of outcomes. That — not the distribution, not the geography —
is why it is the hard part.

So it cannot be tested the way everything else here is tested. Two things instead:

**Invariants, not outputs.** These are the scheduler's rungs, checkable on every
run of every build, forever:

- never violate a hard constraint (capability, residency, tenancy, budget)
- never seal placement into a cachet
- never lose a demand
- never dispatch to a node whose advertisement has expired
- never double-count a billable unit

**A deterministic discrete-event simulator, replaying recorded fleet traces.** The
operational record in every receipt — node, provider, duration, RSS, bytes moved —
*is* a trace. The fleet's history is the benchmark corpus we accumulate anyway for
compliance. A/B-ing a scheduler policy becomes a replay against a thousand real
builds, with makespan and cost as the metrics, reproducible because the trace is
content-addressed.

**The compliance artifact, the placement oracle, and the scheduler's test corpus
are the same object.** That keeps happening in this design, and it is usually the
sign that it is right.

## 7. The scheduler is a standing demand

A scheduler that recomputes its plan every heartbeat is dead at fleet scale. One
whose plan is a **memoized demand over evolving facts** recomputes only the
placements whose facts changed — early cutoff, on the product's own thesis. Fleet
state, capability advertisements, store contents, warmth: all demanded facts with
read-sets and receipts.

Which is already written down: *"K8s on the kill list — the orchestration layer is
standing demands + capability daemons + receipted effects"* (`infra-as-demand.md`
lineage). Placement is the first serious vixen program that is not a build.

Bootstrap care: the local scheduler is the demand root and must never itself be
placed. Nothing forces in-program; the daemon demands the plan and acts.

## Open

- Does the **AST** teleport (every runner hosts `vixc`) or the **lowered island**
  (every runner hosts `weavy`)? Lowering artifacts are epoch-scoped and as-if, so
  both are sound. It decides whether a runner can be a static binary on a machine
  you do not administer — which *is* the sovereignty story.
- Is **capability disjointness** a mandatory island cut criterion, alongside
  effects and unprovable demand? If yes, an island can never discover mid-run that
  it needs a capability it lacks, and the distributed demand graph stays statically
  analysable. This is the restriction that makes placement tractable.
- Capability **class** is syntactic (union over reachable command grammars — a
  static over-approximation, costs perf never correctness). Capability **instance**
  (which rustc) is negotiated at acquisition, and *is* value-affecting, so toolchain
  identity belongs in the semantic receipt while the node that hosted it does not.
- Warmth decay function, advertisement lease length, and hysteresis to stop
  thrashing. All cost-model; all wanted by the simulator before the fleet.
