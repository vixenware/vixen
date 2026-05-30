# Product boundaries — app.vixen.rs, CCC, and Vixen build

**Status: working notes. Non-normative, but authoritative *intent* — ahead of the
code.** This records a three-way distinction that is currently written **nowhere**
(not even in app.vixen.rs) and is confused *internally*: the existing code and
database schema conflate these products. Read this before forming a mental model
from the code.

---

## The three things

### 1. app.vixen.rs — the shared **cloud layer**

GitHub auth (login), **organizations** (tenancy), **billing** (Stripe). Identity +
tenancy + money. It is **not** an execution model. It **services both** of the
products below — which is *why* the code looks conflated.

### 2. CCC (cargo-cloud-cache) — async **cache warming**

A service that asynchronously precomputes / **warms a build cache** for Cargo.

- It **never runs customer workloads.** It warms a cache.
- Its execution need is therefore a **job queue** (async work items) — **not**
  runners, **not** a control plane.
- The `runner` pool that exists in app.vixen.rs was built for CCC and turns out to
  be **unnecessary even for CCC**.

### 3. Vixen build ("fix and build") — the demand-driven **build system**

The actual Vixen build system (this repo).

- Its execution model is a **control plane**: long-lived, capability-advertising
  executor **daemons**; the `vx` CLI places each `exec` by **predicate** over
  advertised capabilities. See [`runners-and-control-plane.md`](runners-and-control-plane.md).
- **No** GitHub runners, **no** ephemeral-VM pool.

---

## Shared vs distinct

- **Shared:** the cloud layer only — GitHub identity, organizations, billing
  (app.vixen.rs).
- **Distinct execution models:** CCC = async **job queue** (cache warming); Vixen
  build = **control plane** (capability daemons + placement). Different shapes —
  **do not unify them.**
- **Content-addressed cache:** CCC's backend is a real content-addressed cache,
  and both products may read/write cached artifacts — but that is **storage
  infrastructure**, separate from either execution model. *Sharing a cache ≠
  sharing an execution model.*

---

## Traps (what the current code will mislead you into)

- The `runner` table (`backend` VMs, `github_runner_id`, `workflow_job_id`,
  `intent ∈ {spawn, active, destroy}`, a pool reconciler) is **CCC scaffolding**,
  CI-runner-shaped, and wrong even for CCC. It is **not** the Vixen executor model.
- The app.vixen.rs schema **conflates** CCC and Vixen build — treat it as a sketch
  built to get Stripe/integration working, not as a definitive data model.
- **"Runner" is an overloaded word.** In the current code it means an *ephemeral
  CI VM* (spin up, register with GitHub, run one job, tear down). In Vixen build it
  means a *long-lived capability-advertising daemon* that is always there so builds
  can be placed on it. **Same word, opposite lifecycle.**

---

## Open

- The Vixen control plane is **deployment-agnostic**: app.vixen.rs is its *cloud*
  deployment, but it also runs **local** (self-hosted) and **embedded** (in the
  CLI), with identity as a pluggable layer — GitHub/orgs in the cloud, LAN trust
  locally. See [`runners-and-control-plane.md`](runners-and-control-plane.md) §7.
  (Vixen build must be usable with **no** commercial cloud.) Still open: whether
  the *cloud* control plane is a distinct service or part of app.vixen.rs proper.
- How (if at all) CCC's content-addressed cache and Vixen build's cache/store
  relate as **shared storage** — same backend, or merely similar.
