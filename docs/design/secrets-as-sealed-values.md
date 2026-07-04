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
