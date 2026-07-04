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

## Gingembre recon adjudicated (Amos rulings, 2026-07-04)

1. THE RECON FOUND DEBT, NOT DOCTRINE (Amos, on
   shortcodes-as-HTML-post-process): "Of course. And that's wrong.
   And we already know the right shape." Shortcodes belong IN the
   grammar (snark embedded/injected regions — the machinery the
   playground's layered injection already uses); the post-process is
   tech debt to burn, not an architecture to respect.
2. THE CHRYSALIS RULING: "Dodeca BADLY wants to disappear within
   vixen. It was a proto-vixen. Manual. Fragile. Budding." Full
   unification IS the goal; picante's 30+ queries are all debt; the
   recon's PARTIAL/DOESN'T-FOLLOW entries measured
   distance-to-goal, not validity. The recon doc = the migration
   map (branch gingembre-snark-unification).
3. LOOP MEMO-KEY IDENTITY: confirmed core-vix design (Amos: "we do
   need to solve that"). The React-key question for demand engines:
   per-element invocation identity under list edits. Axes:
   position-keyed (shifts thrash memo) / content-keyed (reorder-
   stable, duplicate + mutation questions) / explicit key_by
   (user identity). Trees have a natural key (path); the design
   question is arbitrary runtime lists. QUEUED for co-design;
   gates template for-loops and any runtime iteration.
4. CLOSURE CODE SHIPPING: DE-ESCALATED (Amos: "Why not? But also
   why? I don't see any problem either way"). Not doctrine — the
   fleet design picks the pragmatic option (spine=identity travels;
   code fetched as content-addressed flesh on demand, the store's
   own model applied to code); nothing gates on it.
