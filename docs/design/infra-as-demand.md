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
