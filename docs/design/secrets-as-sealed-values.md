# Secrets: sealed values (co-design opened 2026-07-04)

Amos's standard: sops locally, OS keychain custody (no key files on
disk); Vixen cloud provides KMS.

DESIGN SYNTHESIS:
- sops() is a PROBE: Doc with sealed leaves = pending-shaped values
  (public identity, sealed flesh); reveal = coercion gated on key
  capability. Age::acquire with private half in OS keychain
  (Keychain/TouchID, secret-service/TPM) — open App/Agent ruling
  makes keychain-touching code auditable.
- PER-LEAF LAZY REVEAL (sops can't): only the demanded leaf
  decrypts, provable by trace absence; every reveal is a journal
  observation → secret-access audit queries.
- CLOUD KMS = claims machinery applied to keys: envelope encryption,
  tenant root in HSM wraps data keys; RECIPIENTS = claim-namespace
  membership; rotation = re-wrap w/ identity blast radius; fleet
  executors get lease-scoped data keys (standing-runs primitive);
  client journal × KMS log corroboration. BOOTSTRAP ROOTS declared
  as outside-the-graph trust anchors (HSM / keychain).

TWO LAWS (need Amos before build):
1. SECRET PLAINTEXT IS NEVER HASHED into public identity — sealed
   identities derive from ciphertext; nondeterministic seal = finer
   memo keys = safe direction.
2. SECRECY IS TAINT RIDING DEMAND EDGES: derived-from-revealed
   artifacts are themselves sealed (same recipients, salted
   identity). sops seals files; we seal DATAFLOW. Open design:
   taint granularity, interactive-approval points, plan-vs-apply
   reveal timing.

## Co-design rulings (Amos, 2026-07-04)

Q1 TAINT GRANULARITY: (b) STRUCTURAL PRECISION — per-leaf where the
machine understands the derivation (doc/tree/map/TEMPLATE ops),
whole-output for opaque exec runs. This makes TEMPLATING the crux:
a template render is the canonical structured secret flow (hole A
secret, hole B public, output text). Precision requires a
TAINT-AWARE, demand-aware template engine.
  => GINGEMBRE UNIFICATION (Amos): a template IS a grammar (snark
  parses it) and rendering IS evaluation with holes as demand edges
  — so a template is a vix function of its holes. Lowering gingembre
  (Dodeca SSG, partially on snark, currently uses picante) to the
  machine pays 4 ways: Dodeca warm incremental rebuilds; config-gen
  = same primitive; per-hole secret taint = real; picante dropped.
  NEXT: recon round (gingembre today / what picante provides / the
  grammar shape) before committing — parallel to vixen-infra recon.

Q2 LATTICE: recipients = INTERSECTION of input recipient sets;
widening only via explicit capability-gated journaled DECLASSIFY.
Two rules, nothing else. CONFIRMED.

Q3 REVEAL TIMING: plan never reveals (shows identities, not
plaintext) → plan produces the REVEAL MANIFEST for free →
apply-start BATCH approval (one prompt, session lease); CI
substitutes KMS policy. COMPLICATION (Amos): multi-round approval
if a revealed value determines which secret is needed next
(data-dependent secret demand = pick()/lazy-map for reveals). RULE:
secret references STATICALLY RESOLVABLE by default (manifest
complete, one approval); secret-names-secret is a DECLARED
MULTI-PHASE apply, approved per phase. Multiple Touch-IDs only when
explicitly opted into multi-phase.

Q4 SALTED IDENTITY: RULED (Amos) — HYBRID (c): public identity =
hash(ciphertext) (per-seal, dictionary-proof, finer-is-safe) + an
OPTIONAL recipient-sealed content tag for dedup/change-detection;
rotation chooses bump-vs-preserve the tag. All four secrets laws now ruled.
