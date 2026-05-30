+++
title = "Packages and publishing"

[extra]
type = "Doc"
tiers = ["2", "3"]
updated = "2026-05-26"
+++

How Vixen distributes domain logic, who publishes it, and how identity flows from CI to a Vixen org to a scope.

## Why packages matter from Tier 2

[Domain logic lives in Vixen packages](/vixen-language/#domain-logic-lives-in-vixen-not-in-the-runtime), not in the runtime. `@vixen/rust` knows cargo; `@vixen/c` knows clang; `@vixen/protobuf` knows protoc. The package mechanism is alive from Tier 2 onwards because Tier 2 itself depends on `@vixen/rust` being a package customers consume.

Customers will eventually lock specific versions. Downgrade-on-regression is a real workflow. Package management is not optional.

## Package shape: manifest + entry point

A package is a directory containing:

```
package-name/
├── main.vx            # entry point module; manifest in its frontmatter
└── …additional .vx files referenced from main.vx
```

The manifest lives as **frontmatter** on `main.vx` — a leading fenced block (`` ```styx … ``` ``) at the top of the file, written in styx and validated against a schema (same machinery as the wiki's `+++` frontmatter). See [Vixen language → Frontmatter](/vixen-language/#frontmatter). Minimum fields:

- `id` — package name (scoped, see below)
- `version` — semver
- `description` — one-liner
- `license` — SPDX identifier
- `authors`
- `dependencies` — other Vixen packages with version constraints
- `min-runtime-version` — minimum Vixen runtime version the package's intrinsic usage requires

`main.vx` is the single entry-point module; it re-exports anything else the package surfaces. No multi-entry-point complexity (no npm-style `exports` map) on day one — submodules sit as sibling `.vx` files and are referenced from `main.vx`.

There is no separate `package.styx`: a `.vx` file carries frontmatter natively (a fenced block analogous to markdown's `+++`), so the manifest rides on `main.vx`. The parser keeps the fence as one opaque token exposed via `SourceFile::frontmatter()`; the runtime parses its styx body for the manifest fields above.

## Names are scoped, npm-style

Cargo's flat global namespace is a known design mistake. The Rust community works around it constantly — projects prefix every crate name with their org or initiative because there's no namespacing. Squatting is common, ownership is invisible from the name, first-come-first-served rewards landgrabs.

Not repeating it. Vixen package names are **scoped**:

- `@vixen/rust` — first-party.
- `@vixen/c` — first-party.
- `@bearcove/anything` — published by the bearcove org.
- `@community-author/anything` — published by a community maintainer.

No bare names. No legacy compat with cargo's flat model.

Consequences:

- Ownership is visible in the name. A dependency list shows who maintains each package; no googling required.
- Squatting at the package-name level is impossible — you can only publish under a scope you own. Two unrelated parties can pick the same package name as long as their scopes differ.
- First-party packages are unambiguous. `@vixen/*` is reserved for the Vixen team.
- No migration path from cargo. Vixen packages are a different namespace from cargo crates. `@vixen/rust` (the package that *knows about* cargo) has no cargo counterpart.

A Vixen org can own **one or many scopes** — `bearcove` could own `@bearcove`, `@bear-tools`, `@bearcove-experimental`, etc., for sub-branding without spinning up multiple Vixen orgs.

## Identity: Vixen orgs link to external orgs

Vixen has its own organization concept — first-class, distinct from GitHub or Forgejo organizations. A Vixen org is the entity that owns Vixen-side resources: package scopes, billing accounts, private caches, trust settings. Customers don't show up with a GitHub org and start publishing — they create a Vixen org and link external orgs to it.

The flow:

1. **Customer creates a Vixen org.** Self-serve via the web flow.
2. **Customer links one or more external orgs** (GitHub, Forgejo, GitLab, etc.). The link is proved via OIDC — the customer triggers a workflow in the external org that mints an OIDC token, presents it to Vixen, Vixen verifies the org-ownership claim.
3. **Subsequent OIDC tokens from any CI workflow in those external orgs map back to this Vixen org.** Publishes from CI present the token; Vixen looks up the link; the publish authenticates as the Vixen org.
4. **The Vixen org owns one or more package scopes.** Publishing goes to whichever scope the publish call names; the OIDC linkage verifies the publisher's Vixen org owns that scope.

Consequences:

- No direct GitHub-org-to-scope mapping. An external org doesn't *become* a scope. It links to a Vixen org which has scopes. Indirection means changing forges (GitHub → Forgejo, etc.) doesn't lose the scope or the package history — add a new link to the same Vixen org.
- Multi-forge support is structural. A Vixen org can link GitHub + Forgejo + GitLab simultaneously, all producing OIDC tokens that authenticate as the same Vixen org.
- Vixen org is the billing root. Tier subscriptions, cache quotas, trust settings, and package scope ownership all live there. Same primitive across products.

### Name reservation for well-known brands

Squatting at the *Vixen org name* layer is a real concern. First-come-first-served on Vixen org names would let people grab `microsoft`, `meta`, `apple`, etc. before those companies set up accounts.

Mitigation: a reserved-name list of well-known brand and project names (LLM-generated survey of ~300 organizations that might want to claim their names later, refined by hand). Creating a Vixen org with a reserved name doesn't auto-create — it triggers a human-review flow.

Imperfect, but covers the obvious cases.

## Publish authentication: OIDC trusted publishing only

cargo/crates.io's model is "authenticated account holds a long-lived token, anyone with the token can publish." PyPI's modern evolution adds **trusted publishing via OIDC** — no long-lived secret. We already use OIDC for CCC auth; reusing it for package publish keeps the auth surface coherent.

Decision: **OIDC trusted publishing only.** No long-lived API tokens.

To publish, you set up your CI to mint an OIDC token, present it to the index, and the index verifies workflow + repo + ref claims map to the Vixen org that owns the target scope.

Consequences:

- No long-lived secrets to leak. A compromised CI token is minutes-scoped and tied to a specific workflow run.
- Every publish has verifiable provenance. "Published from `bearcove/vixen-rust@v2.4.1` workflow run `xyz`, verified by GitHub OIDC at time T" is recorded with the index entry.
- Dogfooded from day one. Vixen team publishes via the same OIDC flow customers eventually will. No internal-publish path that hides bugs.
- Higher friction on first publish (a maintainer has to set up CI before they can publish). Borne once per package; subsequent publishes are workflow-triggered.
- Per-CI-provider implementation. GitHub Actions on day one; GitLab CI / Buildkite / CircleCI follow.

## Registry shape: cargo-shaped UX, content-addressed backend

Cargo-shaped surface so customers and tooling map directly: a central Vixen-operated index, with alt-registries configurable per-workspace. PubGrub-flavoured semver resolution. Lockfiles record `(name, version, handle)`.

Backend deviates from cargo to fit Vixen's existing content-addressed architecture:

- **The registry is an index.** A small published service mapping `(scope/name, version) → Store content-addressed handle`. Index entries are tiny (a handle is 32 bytes plus metadata). The index can be replicated, mirrored, fronted by any CDN cheaply.
- **The bytes live in Store.** [Same `vx-store` infrastructure that serves cache content](/architecture-and-infrastructure/#store), pinned-retention so package bytes are never evicted. Packages aren't a different storage system; they're handles in the same CAS.
- **Lockfiles record handles.** Once resolved, a lockfile points at content-addressed handles directly. A frozen lockfile lets you build *without ever talking to the index again* — the bytes are findable by hash in any reachable Store mirror.

What this buys:

1. Builds don't depend on the registry's availability. Cargo with crates.io down halts. With this model: lockfile resolved + bytes in any reachable Store ⇒ you build. Index needed only for *resolution*, which is rarer than building.
2. Private packages fall out. Customer's private packages live in their private Store; their private index entries point at handles in their private Store. No separate "private registry" product to build; same machinery with different trust gates.
3. Alt registries are cheap. An alt index is just a different name → handle mapping. Bytes can live in the shared Store or a different one. Mix and match.
4. Aligned with everything else in Vixen. No special-case "this is the one part of the system that isn't content-addressed."

Cost: we operate an index server in addition to Store. A small box; conceptually a published JSON-ish file mapping `(scope/name, version) → handle`. Tooling has to know about both the index and Store.

## Resolution

[PubGrub solves both](/build-and-resolution/#pubgrub-solves-both-cargo-and-vixen-package-resolution) — the runtime supplies PubGrub as an intrinsic; the small core Vixen module that ships embedded with the runtime defines Vixen's resolution semantics; PubGrub solves. The UX is cargo-shaped: semver ranges in the manifest, lockfiles, `vixen pkg update` as the explicit re-resolution command.

## Store retention applies

Package bytes are pinned (see [Architecture](/architecture-and-infrastructure/#retention-dimension)). Never evicted. Yanks remove the index entry but leave the bytes — lockfiles already referencing the handle continue to build, new resolutions skip the yanked version. Effective equivalent of cargo's yank semantics.

## What's still open

- Mirroring policy for the index (deferred, same shape as the crates.io discussion).
- Whether we sign packages beyond the OIDC-provenance attestation (signed releases, trusted-publisher chains, reproducibility-quorum tiers).
