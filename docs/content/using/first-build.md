+++
title = "Your first build"
weight = 2
+++

> **Early access note.** Command syntax lands with the first wave; the walk
> below is the real pipeline as it executes today in vixen's test harness,
> narrated for a human.

Point vixen at a Cargo workspace and demand a build. Here is what actually
happens — and what you'll see in the [receipts](@/using/receipts.md):

**1. Manifests are parsed — in vix.** Your `Cargo.toml` files are read by build
logic written in the build language (`cargo_manifest.vix`), producing typed
values: packages, targets, dependency declarations. From here on, everything is
values with content identity.

**2. Resolution — rodin.** The dependency graph plus a pinned snapshot of the
package index go to [rodin](@/constellation/rodin.md), which selects versions
exactly as Cargo would (verified differentially against real `cargo` — the
current fixture score is on the [Status](@/status/_index.md) page). The
selection is a value; resolve again without changes and it's a memo hit.

**3. Fetch and extract.** Selected crates are fetched from the registry by
checksum — the index gave us the expected hash, so a download is verified
content, not trusted bytes — and extracted into the content store.

**4. The build walk.** Here is the part worth slowing down for: there is no
topological sort and no scheduler. Building the workspace *demands* the root
units; each unit demands its dependencies' interfaces; shared dependencies are
demanded twice and computed once (the memo is the coordinator). Compiler
invocations are pipelined the way Cargo pipelines them — metadata first, so
dependents start early.

**5. Execution — hermetically.** Each `rustc` runs with declared inputs
staged, under the sandbox: an undeclared read *fails* rather than silently
poisoning the cache. Outputs are harvested into the store; the unit's result
is now a content-addressed fact.

**6. The second build.** Run it again: every demand memo-hits; the answer is
the answer. Change one file: the parse changes, the affected items change,
exactly the dependent units recompile, and the wave stops at the first
unchanged interface. That behavior isn't an optimization pass — it's steps 1–5
doing nothing, observably, and the receipt proves it.
