+++
title = "The build graph is also a provenance graph"

[extra]
type = "ArchitectureNote"
topic = "build-graph"
created = "2026-05-26"
tags = ["tier-2", "provenance", "compliance"]
+++

A Vixen build is a graph of typed nodes. Each node represents a unit of computation — a rustc invocation, a clang compile, a tarball extraction, a protobuf codegen step, a linker run — with explicit inputs, explicit outputs, and a deterministic mapping from one to the other.

Because the graph is the *only* description of the build, it carries every property the build system needs to answer questions about what got produced. The build graph is, *for free*, a provenance graph.

## What every node knows

By construction, each node in the graph has:

- **Identity.** What kind of step this is (rustc, clang, fetch, extract, sign, …).
- **Inputs.** Content-addressed handles to every byte fed into the step — source files, dependencies' outputs, declared toolchain identities (see [[Declared identity collapses the read-set under trust]]).
- **Outputs.** Content-addressed handles to every byte produced.
- **Origin.** Where the inputs came from — a registry, a git commit, a local checkout, a customer-private mirror.
- **Licence and SPDX metadata** of every dependency that contributed to this node, propagated from the inputs.
- **Executor identity.** Which executor class ran the node, optionally which specific machine, optionally a TEE attestation chain.
- **Time, duration, resource shape.** Wall-clock, CPU, RSS, network bytes — observed during execution and attached to the node.

None of this is bolted on. It is what a node *is* in the Vixen runtime.

## What that enables, for free

Several customer-facing capabilities reduce to "query the graph":

- **SLSA-grade provenance attestation.** The signed chain of inputs that produced a given artifact is the path through the graph from a leaf to that artifact. Generating an attestation is a graph traversal, not a separate pipeline.
- **License/origin policy at build time.** "Reject this build if any contributing node has a GPL licence" is a predicate over the graph. Failure is at build time, not at audit time. See feature 5 in [[Enterprise feature candidates]].
- **Time-travel build replay.** Re-running the build that produced a given artifact is "re-evaluate this subgraph against the same input handles." The graph *is* the replay specification.
- **Bisect-as-a-service.** Across commits, the graph for each commit is materialised in the cache (or recoverable from it). Binary-searching for a regression is graph diffing + targeted re-evaluation, not rebuilding.
- **Per-team cost attribution.** Every node has an owning crate/target/team. Summing wall-clock by owner gives cost-per-team without an external spreadsheet.
- **Build-graph diffing across commits.** "What changed in the build from PR to main" is a structural diff over two graphs — different question from "what changed in the source."
- **Sub-file incremental invalidation.** The sandbox's observed read-set on a node tells us which *byte ranges* of which inputs that node actually depended on. A change outside the read-set doesn't invalidate the node. See feature 15 in [[Enterprise feature candidates]].
- **Affected-only test execution.** The graph knows which tests depend on which code via the same edges as the build itself. "Only run affected tests" is a graph reachability query, not a separate test-impact analyser.

## What this is *not*

- It is not a property bag bolted onto cargo. CCC ([[Tier 1 — CCC]]) sees rustc invocations one at a time; it does not have the graph. Provenance capabilities listed above are [[Tier 2 — Vixen Build]] only.
- It is not the same thing as a "telemetry pipeline." Telemetry observes what happened; the build graph *is* what happened, by being the runtime's only model of the build.
- It is not optional metadata that customers can disable. The graph is the build. Disabling provenance would mean disabling builds.

## Pattern

Vixen does not "add provenance" as a feature. It is what falls out of describing builds as graphs with content-addressed edges. The enterprise features in [[Enterprise feature candidates]] are not separate products — they are predicates and traversals over the graph the runtime already maintains.

## Related

- [[Tier 2 — Vixen Build]]
- [[Enterprise feature candidates]]
- [[Declared identity collapses the read-set under trust]]
- [[Demand-warmed cache]]
