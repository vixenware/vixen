# The Vixen package manager + provenance brief

Status: opening brief, 2026-07-07. Amos-stated requirements captured verbatim-in-intent;
design questions ranked; nothing here is adjudicated yet except where marked RATIFIED.
Companion corpus: store-architecture.md, vx-store-as-vix-memo.md, artifact-intelligence.md,
infra-as-demand.md (required grounding for any committee round on this territory).

## Amos's pillars (product requirements, not suggestions)

1. **Best of npm and crates.io; forget the rest.** From npm: scopes, publish ergonomics,
   web UX, provenance attestations (bolted on in 2023 — ours is native). From crates.io:
   immutability discipline, semver culture, docs integration. Forgotten: deletion
   (left-pad), flat namespace, token-soup auth.
2. **Full traceability from day one; as auditable as possible.**
3. **2FA non-negotiable.**
4. **Infra in the EU, operated by vixenware corp.** The registry is likely the first real
   inhabitant of the cloud control plane (the one permanently-proprietary component).
5. **Gentoo-inspired overlay system.** The motivating user: an AWS-shaped enterprise
   running an internal overlay so it can patch any package, at will, invisibly to build
   logic — first-class, not `[patch]`-section folklore.
6. **Scoped namespaces from day one.** RATIFIED ("bye crates.io's regrets").
7. **Recipe-embedded artifacts.** See below — the strongest claim in the stack.

## What the constellation makes structurally cheap

- A package version is a content hash; publishing is appending to a **transparency log**
  (Merkle, append-only — the shape vx-store already thinks in). Yank is metadata;
  deletion does not exist. This is traceability (pillar 2) falling out of the store
  design rather than being a feature.
- Resolution receipts name the log entry AND the overlay that supplied each package —
  overlays (pillar 5) and auditability (pillar 2) reinforce each other because the
  overlay is a *resolver* concern: rodin resolves against the composed view, so the same
  machinery that explains a version choice explains a provenance choice.
- Hermetic, receipted builds mean the registry can carry **build attestations** as
  ordinary data: "this artifact came from these inputs via this toolchain, here is the
  observed read-set." SLSA Build L3 struggles to approximate what our sandbox emits by
  construction.

## Recipe-embedded artifacts (Amos, verbatim intent)

> Artifacts should be stamped with the entire recipe. A binary out of Vixen CI should
> have embedded in itself, as metadata, the entire recipe to produce it. If everything is
> open source, you should be able to take a binary and rebuild it by virtue of having the
> binary, without hunting down the recipe. ("And then I can finally shut up the nix
> fanboys forever.")

Design sketch:

- **Vehicle**: an ELF note section (`.note.vixen.recipe`) / Mach-O section
  (`__VIXEN,__recipe`). Prior art in miniature: `cargo auditable` embeds a dependency
  list in `.dep-v0`; Go embeds buildinfo readable by `go version -m`. Ours is the
  maximalist completion: the full resolved input graph by content hash + fetch
  coordinates (registry + overlay), toolchain included — the whole pancake stack is just
  more content-addressed inputs. rustc, linker, sysroot: hashes in the graph like
  everything else. "On this node" matters only to the signature (who claims this), not to
  what was built (content carries that).
- **Self-sufficiency over pointers**: embedding only a log-entry hash would require the
  log to be reachable; embedding the full recipe requires only the *inputs* to be
  fetchable. Embed the recipe (compressed), and also key it into the transparency log by
  its hash — both properties, no dependence.
- **Verification UX**: `vx rebuild ./binary` — extract recipe, fetch inputs by hash,
  replay, compare output hashes. Bit-for-bit reproduction as a one-command property of
  the artifact itself. Fine granularity makes the verification rebuild *cheap* (memo),
  which no coarse-derivation system can say.
- **Proprietary inputs**: the recipe still names their content hashes and scopes. A party
  with access verifies fully; a party without still gets the complete audit trail —
  partial reproducibility is *explicit in the recipe*, not a silent failure.
- **Signing chicken-egg**: you cannot naively sign a hash of a file that contains the
  signature. Authenticode and macOS codesign both solve it by excluding the signature
  region from the signed digest; ELF has no blessed convention, so we define one
  (digest = all sections except `.note.vixen.sig`), plus a detached copy in the log.
- **Embed the request, not just the resolution** (consequence of `vx upgrade`, Amos
  2026-07-07): the recipe carries BOTH the pinned resolved graph and the original solve
  request (manifest constraints, features, config). Three verbs then separate cleanly:
  - `vx rebuild binary` — replay the pinned resolution; bit-for-bit verification.
  - `vx upgrade binary` — re-run rodin on the same request against a newer index
    snapshot, diff resolutions, rebuild only the delta (memo: you pay for the openssl
    wave, not the world), stamp a new recipe referencing its predecessor — artifact
    genealogy. `vx upgrade --only <pkg>` = minimal-change re-resolution (`cargo update
    -p` semantics, artifact-side).
  - `vx audit <dir>` — registry annotates content hashes with CVEs; every deployed
    binary names its items; the fleet answer is "upgrade in place," not "hunt down the
    repos." Patching becomes an operation on artifacts you possess — the problem distro
    package managers exist for, dissolved.

  **Advisories keyed by content hash, not version range** (Amos-endorsed, 2026-07-07):
  OSV/RustSec/GHSA key on package@version-range; the registry PROJECTS those advisories
  onto content hashes. Two properties version ranges can't give:
  1. *No false positives on backports.* An enterprise overlay patching openssl produces
     a distinct hash; the registry marks THAT hash not-affected even though upstream's
     range says affected — the overlay patch IS the backport. Version-range auditing
     punishes exactly the orgs doing the right thing; hash-keyed auditing rewards them.
     (This is also the overlay-identity question answered: overlay entries MUST be
     distinguishable-by-hash from upstream, or audit collapses back to ranges.)
  2. *Transitive truth.* A binary doesn't "depend on openssl 3.0.1" — it embeds specific
     item hashes. Audit answers "is the vulnerable code in this artifact," not "does the
     lockfile mention a scary version" — which also kills the vendored-under-another-name
     blind spot.
- **vs Nix**: a Nix artifact is mute — reproduction requires the flake/derivation and the
  store; store paths are input-addressed (CA-derivations experimental for years); and the
  unit of reproduction is the coarse derivation. Here the artifact carries its own
  derivation, addresses are content, and the replay is item-granular. The binary is its
  own flake.

## Ambient-system policy (Amos, 2026-07-07 — DECIDED)

The question "what does a recipe say when build.rs probes the ambient system (pkg-config,
system libcurl)?" has an interim and a final answer:
- **Interim**: builds run inside a container image pinned by exact digest, and the recipe
  records that digest. Every ambient read resolves against an immutable filesystem, so
  observability and reproducibility are both satisfied by the image hash. (This is a
  content-addressed sysroot in crude form — a degenerate overlay.) Not the part we want
  to tackle now; the image hash makes it sound in the meantime.
- **Final**: there is no pkg-config, no ambient libcurl. **We build curl.** Native deps
  become ordinary nodes in the graph, built from source, content-addressed like
  everything else. The ambient world shrinks to the pinned toolchain, then to nothing.

## Trust edges (the design work Amos wants time on)

Enterprises will ask; the answers should be boring and structural:

1. **Overlay authorization is the whole game.** An overlay is a supply-chain injection
   point *by design* — that is its purpose. Who may add one to a resolution, how its key
   is bound to the org, and how the receipt surfaces "this build saw overlay X" must be
   first-class. (An unauthorized overlay is indistinguishable from an attack.)
2. **Overlay identity**: does an overlay entry's identity incorporate what it overlays?
   Proposal: yes — "AWS's patched openssl" is always distinguishable-by-hash from
   upstream, and the recipe shows the patch lineage.
3. **OIDC belongs at the registry boundary** (trusted publishing: CI workflow identity
   publishes, no stored tokens — the one great recent crates.io/PyPI idea), not the build
   boundary. Persistent vixen runners don't need the ephemeral-runner OIDC dance for
   *building*; they hold long-lived attested keys (hardware-backed where the platform
   allows).
4. **What happens to your GitHub Actions workflow** (open, Amos flagged): with persistent
   runners, GHA degenerates to a trigger — or disappears, with vixen CI watching the repo
   directly. Needs a worked answer for the migration story.
5. **The vocabulary for the enterprise conversation**: SLSA (slsa.dev, OpenSSF; Build
   L0–L3; L3 = provenance generated by the platform, unforgeable, isolated builds),
   in-toto (attestation format), Sigstore (cosign signing, Fulcio short-lived certs from
   OIDC identity, Rekor transparency log), reproducible-builds.org. We should speak it
   fluently and then explain where receipts exceed it: SLSA provenance is a signed
   *claim*; a vixen receipt is the *observed read-set*.

## Open questions, ranked (each gates the ones after)

1. **What is a package?** Source-only (crates.io model), artifacts-with-attestations, or
   both with artifacts as a transparent, verifiable cache layer. The third is the
   constellation-native answer; it has the largest security surface and the receipts to
   pay for it.
2. Overlay composition semantics: pure shadowing (Gentoo) vs patch-level composition
   (overlay carries a diff against upstream content).
3. Scope→identity→2FA binding; org key lifecycle; key rotation without publish gaps.
4. Registry protocol shape (presumably vox RPC + a dumb-HTTP sparse-index-compatible
   read path so the cargo model keeps working during migration).
