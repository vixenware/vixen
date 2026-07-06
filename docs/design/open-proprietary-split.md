# The open / proprietary split (and the long-term strategy)

*Working note — where the vixen ecosystem is headed, so agents stop re-deriving
the boundary. Kept in the private repo because it records product/business
strategy; do NOT copy the monetization detail into the public facet monorepo.*

## The boundary

Almost everything becomes **open source**. The dividing line is not "our code vs.
theirs" — it is **what runs on a user's machine vs. what runs in our cloud.**

- **Open (the on-device substrate)** — anything that executes on a user's
  computer, open *for trust*: you cannot ask people to run an opaque sandbox or
  filesystem on their own hardware. Includes:
  - **vix** — the language + demand machine.
  - **rodin** — the cargo-shaped resolver, implemented in vix.
  - **the sandbox** — landlock (Linux), seatbelt (macOS), projfs (Windows), fuse3.
  - **the VFS** — `vx-vfs` / `vx-vfsd` / `vx-fskit` / `vx-store` / `vx-io` & co.

  These belong in the **facet monorepo** (github.com/facet-rs/facet, public).

- **Proprietary (cloud / backend / control plane)** — what users do *not* run
  themselves. This is the business and its value-add:
  - the **store** as a service,
  - **SLSA** provenance / attestation,
  - **debugging / profiling** infrastructure,
  - access to a **compute marketplace**,
  - **CI integration**,
  - the control plane that orchestrates the on-device agents.

Open-client / proprietary-cloud: the client is auditable; the money is the
services people don't self-host.

## Long-term migration

The facet monorepo becomes the home of the whole open substrate. Order of
arrival:

1. **vix** (already there) + **rodin** (in progress — the first migration).
2. **sandbox** + **VFS** follow later (a separate migration, not the rodin task).

Cloud / control-plane stays proprietary.

## Consequences for the rodin task (now)

- rodin + vix are the first residents of the open monorepo — that is why rodin
  work happens in facet-cc, not here.
- The Rust reference (`rodin-core` / `rodin-facts` / `rodin-survey`) + the dead
  `vix-solve` leave: knowledge distilled into `facet-cc/rodin/docs/`, then the
  crates are deleted.
- `rodin-fixtures` (the cargo-differential harness) moves to facet-cc, retargeted
  from the Rust system-under-test to rodin.vix.
- No open component may depend on a proprietary one. Near-term this is moot for
  rodin (its fixtures need no cloud); long-term the sandbox/VFS it might touch are
  open too.
