# The open/proprietary division

*Private notes. This is the line between the facet repo (open, vix) and this
repo (proprietary, Vixen/Vixenware). Settled with Amos 2026-07-03; revisit as
the product takes shape.*

## The rule

**The semantics are open. The plural is the product.**

Everything that defines what a correct build *is* — language, identity, memo
keying, read-set verification — is open, because it's the trust surface: users
must be able to check that caching is false-positive-never and that identity
means what we say. Everything that exists only *in the plural* — shared caches,
fleets, attestation with reputation behind it, org management — is what we
sell.

A subtlety that forced the line further open than instinct suggests: **the
local open daemon must be genuinely capable** — real sandboxing, real virtual
filesystems — or the system just doesn't work and nobody adopts it. So the
sandbox and VFS go open too (they're also kernel-adjacent, where closed source
is a trust liability, and see §TEE below for why open sandbox code is
*required* for the attestation product to mean anything).

## Artifact by artifact

Open (facet repo):

- **vix** — grammar (snark), AST, binder, oracle (reference evaluator),
  graph demand engine, exec seam (two-tier keys, `verify()`, seam types).
- **vix-wire** — the executor *protocol* over vox + toy/fake backends.
  It's a protocol testbed; fine that fleet/gravity *concepts* are visible.
- **vix-daemon** — the daemon RPC surface (Daemon service, DemandEvent,
  StepCommand — this is vix's LSP; the ecosystem builds against it) AND a
  reference daemon: local eval, oracle-backed. "The language runs on your
  laptop."
- Eventually: real local sandboxing + VFS (the capable local daemon).
- **snark playground** — the *language* playground (parsing, binding, IDE
  ops). Authoring tools generally lean open.

Proprietary (this repo):

- **The vixen playground / build console** — the daemon-connected IDE, the
  execution graph, and everything that grows toward "what happened to my
  build": historical, cross-build, fleet-wide, team-shared exploration.
  Placement decided 2026-07-03: the in-progress browser IDE moved here
  (apps/playground). The protocol is open; anyone *could* write their own
  console. Ours ships integrated.
- **The production daemon** — same open vox service, backed by the real
  engine: warm caches, fleets, multi-tenant.
- Fleet at scale: gravity/latency-aware scheduling, transfer planning,
  multi-region CAS, second/millisecond executor provisioning.
- Attestation signing service, org plane (SSO, capability ACL management,
  audit, cost attribution), billing, control plane.

Naming: public artifacts are vix-flavored (vixd = "vix daemon"); **Vixen** is
the product/company brand and stays out of the open repo's vocabulary.

## The trust ladder (what we can honestly claim)

`verify()` proves a cache entry's **preconditions** hold — read set, listings,
negative lookups match what the run pinned. It says **nothing about the output
bytes**. A malicious executor/cache can serve a poisoned artifact with a
perfectly consistent read-set record. False-positive-never is a claim about
*staleness and bugs*, not about *malice*. So, the ladder:

1. **Bookkeeping: verified.** Customers never have to trust our cache *keying*
   — verification is language-side and unbreakable by the runtime. (This is
   the seam we built for correctness; it turns out to be the business model:
   the closed runtime is safe to buy because the open language checks it.)
2. **Execution: audited.** Purity + hermetic sandboxing + pinned observations
   make any hit *spot-checkable*: re-run the closure, compare hashes. The
   enemy is nondeterminism (timestamps in objects, etc.) — the journal/pinning
   discipline is the weapon, and "reproducible" is a war, not a flag.
3. **Quorum.** Run the same closure on N independent executors (ideally
   different providers/TEE vendors), compare output hashes. Purity makes
   disagreement *evidence*.
4. **TEE-attested.** Hardware-signed "output O = exec(closure C)". See below.

Do not oversell: we are at (1) solid, (2) structurally enabled, (3)/(4)
designed-for but unbuilt. "Zero trust" is not solved; we are unusually close
to the ladder being climbable, which is more than anyone else has.

## Trusted execution (sketch — the fun part)

vix inside a TEE is unusually clean because of decisions already made:

- **The memo key IS the attestable statement.** The unit of execution is a
  pure closure with content-addressed identity (closure hash × plan ×
  capability) and pinned observations. An attestation is one tiny signed
  claim: `O = exec(C)`. Composable, cacheable — it lives naturally in the
  same table as the memo.
- **Provenance = the DAG of signed edges.** A final artifact's supply-chain
  story is a chain of `O = exec(C)` signatures; verifying a whole build is
  signature checks + hash plumbing, **no re-execution**. This is SLSA with
  actual semantics underneath (SLSA provenance is mostly "the CI said so").
- **The TCB is tiny.** The enclave needs only: the sandbox runner + the
  hasher. No trusted filesystem, no trusted network — every input arrives as
  a content-addressed blob and is verified *inside* the boundary. Demand-
  driven fetch composes: the enclave demands paths, checks hashes, done.
- **Open sandbox code is a requirement, not a giveaway.** TEE attestation
  signs a *measurement* of the code that ran. That measurement only means
  something if the customer can reproduce it — i.e. the executor/sandbox
  must be open source and reproducibly built. Giving away the sandbox is
  what makes the paid attestation product meaningful.
- **Observer closures stay inside the boundary.** Queries about trees run
  attested too; only the (signed) result crosses.
- Product ladder: free = our word; paid tier N = quorum; paid tier N+1 =
  TEE-attested with hardware roots from multiple vendors. Criticality tiers,
  priced accordingly.

## The moat, honestly (the weekend-LLM objection)

Objection (Amos): "BuildBuddy sells compute for Bazel" is a pre-LLM story —
today someone deploys a compatible implementation of an open protocol in a
weekend. Correct. **Code replication is dead as a moat.** What survives:

- **Coordination points.** THE public build corpus for the vix ecosystem —
  the place where everyone's LLVM/lua/crate builds already live — is a
  registry, and registries (crates.io, npm) are natural monopolies through
  coordination, not code. A weekend clone of the protocol starts with a cold
  cache and no one pointed at it.
- **Reputation-bearing attestation.** A signature is worth the signer's
  audit trail. Weekend clones don't have reputations; acquiring one takes
  the thing LLMs don't compress: time in market without incident.
- **Language stewardship.** The protocol evolves with the language; the
  blessed, first-party implementation is always the best fit, and defaults
  are sticky. If vix wins, its steward sells everything adjacent.
- **"Someone else operates it."** Companies buy hosted Postgres. Thin alone,
  real in combination with the above.

The uncomfortable corollary: the moat is not in this repo's *code* either.
It's in the corpus, the reputation, and the default-ness. The code here is
closed because it's ours, not because closedness protects it.

## The host runtime — a third home (direction agreed 2026-07-03, name pending)

The sandbox/VFS layer (today: Vixen.app — FSKit extension, sandbox, build
server, one installable app) goes **open source**, but NOT into the facet
monorepo. Three homes:

1. **facet** (open): language + protocol + libraries. Crates, not apps.
2. **A new open repo**, Vixenware-maintained, MIT/Apache (BSL would kill the
   attestation argument): the HOST RUNTIME — what Vixen.app is today. FSKit
   ext (macOS forces the signed-app shape anyway), sandbox profile gen, exec
   runner, the installable daemon. Consumes the facet crates. This repo is
   the TEE-measured artifact, so it wants to be small, self-contained, and
   reproducibly buildable — a crisp audit surface. Vixenware signs official
   builds (a trust anchor and a convenience, checkable via repro builds).
   This is the Tailscale-client / Datadog-agent shape: the privileged thing
   on the customer's machine is open; the network it talks to is the company.
3. **vixenware/vixen** (private): the plural.

Consequence for daemons: facet's `vixd` stays the protocol REFERENCE
(oracle-backed, no real sandbox); the installable daemon people run ships
from the runtime repo, implementing the same open service.

Name: not Vixen-branded (keeps the vix-open/Vixen-paid line clean). Fox
register candidates: **den** (a fox's home — the local host runtime) and
**skulk** (collective noun for foxes — the paid fleet). "Install Den, buy
the Skulk." Amos to decide; "Vixen Foundation"-style brand-carrying names
are the alternative if maintainership legibility wins.

## Playground placement (decided; refined after the third-home split)

- Language playground (snark): facet repo. Open.
- Build/daemon playground (browser IDE ↔ vixd): TODAY in **this repo**,
  apps/playground — but that's a workbench, not a home. Its final home is
  the **host runtime repo**: it is really the daemon's own local dashboard
  (single-eval, live-session introspection — adoption table stakes), and
  "install the app, open localhost, see your build" should be one artifact.
  Migrate when the runtime repo is born.
- The line between open dashboard and private console is per-CAPABILITY:
  live single-eval = open; historical / cross-build / team / fleet = product.
  The console may consume the dashboard's components and add the plural.
  Public demos of exploration features stay "ugly on purpose": proof the
  data exists, not the product.
- Note: the playground consumes the *open* generated TS client (gen_ts in
  vix-daemon, output path passed as an argument) and the open vox TS runtime
  from the facet checkout (sibling-path file: deps for now; publish rc
  packages when that gets annoying).
