# Infra as demand: the ansible/terraform yes-and (2026-07-04)

Terraform reinvented half a build system (DAG, plan, state-as-cache,
drift) with the halves badly coupled; ansible reinvented the other
half (idempotency as author discipline). Vixen's primitives are the
general forms of all of it:

1. STATE FILE = THE JOURNAL. tfstate is a hand-maintained,
   corruptible snapshot of belief. Ours: applied resources are
   claims-with-evidence keyed by invocation identity, in claim
   namespaces (team state = tenant journal). No separate state
   subsystem exists.
2. PLAN = THE PENDING GRAPH. `terraform plan` is "build the graph,
   don't force it" — literally pending invocations; RenderedValue
   already renders pending without forcing. Plan/apply divergence is
   structurally impossible: apply demands the same graph plan showed.
3. DRIFT = TIER-2 VERIFICATION. Refresh asks "does reality match
   state" — the two-tier exec cache with cloud APIs as the VFS:
   coarse fact (resource id) × observed read-set (probed attrs).
   Drift is a failed tier-2 check; the producer re-runs.
4. IDEMPOTENCY = MEMO, BY CONSTRUCTION. Not module-author
   discipline: same identity → memo hit → didn't run. Retries are
   free; partial applies resume.
5. STATE LOCKING = IN-FLIGHT JOIN. Two applies of the same identity
   JOIN instead of racing or lock-failing.
6. HISTORY/COUNTERFACTUALS FREE: journal generations = state
   history; "what changes if I bump X" = the Rodin counterfactual
   surface pointed at infrastructure.
7. PROVIDERS = CAPABILITIES (Aws::acquire fingerprints creds + API
   surface); INVENTORY = PROBED HOSTS (a machine is an artifact:
   arch/libc/kernel probed at acquire).
8. THE MOAT — ONE GRAPH, SOURCE TO RUNNING INFRA: deploy(host,
   image) demands image demands the build. Edit a .c file, demand
   the deployment: exactly the affected image rebuilds, exactly the
   affected instances reprovision — merge tunneling across the
   build/deploy boundary. And artifact intelligence closes the loop
   pre-flight: image glibc floor vs probed host libc = undeployable
   configurations are unbakeable, no execution, checked in the plan.

## Round 2 (Amos: "plan+apply is just evaluation — expand; and there
## has to be a diff/reconciliation process, world moves underneath")

PLAN+APPLY = ONE EVALUATION, ONE SWITCH: "(known after apply)" is
terraform's hand-built pending values; plan = eval where misses stay
pending (+ structured would-do diff), apply = same eval where misses
force. Divergence only when reality moves between runs — and tier-2
names the exact observation that moved.

THE ONE NEW CONCEPT — ADDRESSED EFFECTS WITH CUSTODY:
- Resources have durable ADDRESSES distinct from invocation
  identity; the effect is RECONCILE(address, desired), a run whose
  READ-SET INCLUDES PROBED REALITY (provider API = probe pointed at
  the world). Identity covers desired × observed-current; drift =
  tier-2 miss → re-reconcile (minimal mutation = rediff over
  probed-current vs desired). No drift + no change = memo hit,
  provably nothing ran.
- DELETION: ownership ledger per root (custody set in the journal);
  plan diffs demanded vs owned addresses; orphans → destroy/forget
  policy verbs. Store GC pointed at reality (undemanded ≠ destroyed
  because the world bills for orphans).
- IMPORT = ADOPTION: claim whose evidence is an observation, not a
  run; probe generates the config skeleton (probe values ARE config
  values). Kills terraform-import's write-config-then-fight-diffs.

PITCH (their pain): no state file/locks/surgery; plans that can't
lie; demand-driven refresh (probe only what's demanded); safe
partial applies (resume = re-demand); module memo sharing across
teams; THE MOAT: plan spans compile→rollout with pre-flight
probe checks (image glibc vs probed host libc dies in the plan).
UX: the existing trace UI aimed at infra — vx plan = pending-mode
DAG w/ rediff diffs; vx apply = same graph live; vx why <addr> =
journal query (invocation, config span, generations); vx adopt =
probe + config gen. Build and apply are the same screen because
they are the same machine.

## Round 3: kubernetes/argo (blue/green, auto-rollback)

FITS: GitOps = demand(deployment) over source w/ memo (Argo CD's
pitch is our degenerate case); blue/green = parallel tree
materialization, blue memo-held + custody-kept (STILL WARM), cutover
= one selector reconcile, environment GC = custody policy ("keep N
generations"), history = journal generations; AUTO-ROLLBACK =
DEMAND MOVING: promotion is Cond over analysis evidence
(select(ok ? green : blue), phase-4 dynamic demand) — rollback
re-decides to blue which never stopped being a value. Argo
Workflows = subsumed (DAG jobs = vix with a worse language).

MISSING (four, bounded, none touch the engine core):
1. PUSH INVALIDATION inlet: watch streams → mark journal
   observations stale → re-demand standing roots. Feed, not
   semantics.
2. STANDING-DEMAND scheduler: "keep demanded forever" — warm daemon
   is 90%; missing loop ties reload-diffs + stale observations to
   auto re-demand of registered roots.
3. TEMPORAL vocabulary: windows/deadlines/hold-steps for canary
   analysis. Lands as SCHEDULER POLICY per the batching precedent,
   but the vocabulary is genuinely new surface to design.
4. FIELD-LEVEL CUSTODY: overlapping owners (HPA inside our
   Deployment) — refine custody ledger to field paths; k8s
   managedFields is the prior art to steal.
BOUNDARY (state plainly): vixen manages control-plane truth, not
runtime supervision — kubelet keeps its job; a service's "output"
is a claim continuously re-verified.
