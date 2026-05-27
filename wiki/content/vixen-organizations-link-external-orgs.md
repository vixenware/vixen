+++
title = "Vixen organizations link to external orgs for publishing"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["packaging", "auth", "organizations", "identity"]
+++

## Context

Vixen has its own organization concept — first-class, distinct from GitHub or Forgejo organizations. A Vixen org is the entity that owns Vixen-side resources: package scopes, billing accounts, private caches, trust settings. Customers don't *just* show up with a GitHub org and start publishing — they create a Vixen org and link external orgs to it.

This indirection matters: a Vixen org isn't the same thing as a GitHub org. One Vixen org can link multiple external orgs (mirrors of the same project on different forges). The linkage is what lets OIDC claims from external CI map back to Vixen-side identity.

## Decision

Identity for package publishing flows:

1. **Customer creates a Vixen org.** Self-serve via the Vixen web flow.
2. **Customer establishes a link to one or more external orgs** (GitHub, Forgejo, GitLab, etc.). The link is proved via OIDC — the customer triggers a workflow in the external org that mints an OIDC token, presents it to Vixen, and Vixen verifies the org-ownership claim.
3. **Once linked, OIDC tokens from any CI workflow in those external orgs map back to this Vixen org.** Subsequent publishes from CI present the token; Vixen looks up the link; the publish authenticates as the Vixen org.
4. **The Vixen org owns one or more package scopes.** Cardinality is one-to-many: a single Vixen org can own multiple scopes (`@bearcove`, `@bear-tools`, `@bearcove-experimental`, etc.) for sub-branding or sub-project separation without spinning up multiple Vixen orgs. Publishing goes to whichever scope the publish call names; the OIDC linkage verifies the publisher's Vixen org owns that scope.

## Consequences

- **No direct GitHub-org-to-scope mapping.** An external org doesn't *become* a scope. It links to a Vixen org which has a scope. The indirection means changing forges (GitHub → Forgejo, etc.) doesn't lose the scope or the package history — just add a new link to the same Vixen org.
- **Multi-forge support is structural.** A Vixen org can link a GitHub org and a Forgejo org and a GitLab group. All produce OIDC tokens that authenticate as the same Vixen org. No special-casing per provider.
- **Vixen org as the billing root.** The Vixen org is also where CCC tier, cache quotas, trust settings live ([[CCC authentication uses OIDC trusted publishing]]). Packaging-identity reuses the same primitive.
- **Org ownership disputes are handled at the Vixen-org layer.** Two parties claiming the same Vixen org name go through Vixen's resolution process, not through GitHub or Forgejo policy.
- **Scope namespace flows from the Vixen org.** See [[Vixen package names are scoped, npm-style]].

## Name reservation for well-known brands

Squatting is a real concern at the Vixen-org name layer, since first-come-first-served on Vixen org names would let people grab `microsoft`, `meta`, `apple`, etc. before those companies set up accounts.

Mitigation: a reserved-name list of well-known brand and project names (generated initially via LLM survey of "300 organizations that might want to claim their name later," refined by hand). A Vixen-org-creation request matching a reserved name doesn't auto-create; it triggers a human-review flow ("hey, we'll be in touch").

Imperfect — squatting is fundamentally hard — but covers the obvious cases.

## Related

- [[Package management for Vixen]]
- [[Vixen package names are scoped, npm-style]]
- [[Package publish uses OIDC trusted publishing only]]
- [[CCC authentication uses OIDC trusted publishing]]
