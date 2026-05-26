+++
title = "Enterprise feature candidates"

[extra]
type = "ArchitectureNote"
topic = "product"
created = "2026-05-26"
tags = ["tier-2", "enterprise", "features", "compliance"]
+++

Candidate features that fall out of the Tier 2 architecture (sandbox, executor classes, build graph with provenance, trust gates, streams) and would be hard or impossible to deliver from Tier 1 alone. Tracked here as the catalogue we draw from when pitching enterprise customers.

This list is unprioritised. It is a menu, not a roadmap.

## Direct consequences of Tier 2

1. **Cross-language hermetic builds** — C / C++ / protobuf / codegen / Rust in one DAG, one cache. Tier 1 only sees rustc.
2. **Remote build execution fleet** — executor classes (linux-x86_64, darwin-arm64, exotic targets); customer's laptop builds via our fleet.
3. **Multi-platform fan-out from one declaration** — build matrix at the language level, not in YAML. Linux × macOS × Windows × arm/x86 from one expression.
4. **SLSA-grade provenance attestation** — every artifact ships with the signed chain of inputs that produced it. Audit answer to "what built this binary?"
5. **License/origin policy at build time** — block GPL, require provenance on accepted deps, fail the build, not the audit. Build graph knows origin already.
6. **Self-hosted executor pool for on-prem / air-gapped customers** — cache coordination in our cloud, exec inside their VPC. Tier 1 cannot credibly offer this.
7. **Programmable version resolution / lockfile policy** — "no CVEs", "internal mirror only", "pinned only" expressed in Vixen, not in a side-tool wrapper.
8. **Build observability with structured traces** — every DAG node: who built it, where, how long, on what executor, against what inputs. Sellable as standalone perf / audit product.
9. **Dev-laptop cache sharing against the org's remote cache** — same trust gate as CI. No cold rebuilds after rebase. Sells per-seat in addition to per-CI-minute.
10. **Toolchain attestation** — "this build used clang-19.1.7, manifest hash X, attested by your org's signing key." Compliance answer to "did the right compiler produce this?"
11. **Affected-only test execution from the build graph** — the DAG knows the test → code dependency; rerun only impacted tests. Tier 1 sees rustc invocations, not test → code wiring.
12. **Reproducibility quorum as a premium trust tier** — paid mode where cache entries are only accepted after N independent executors produce bit-identical output. Max-assurance for regulated industries.

## Deeper architectural unlocks

13. **Time-travel build replay** — every build is deterministic from its key. "Reproduce the binary that shipped on 2025-03-14, bit-for-bit, against the linked-in deps as they were that day." Compliance gold.
14. **Bisect-as-a-service** — given a regression test, binary-search across cached historical builds to find the commit / dep-version that introduced it. Artifacts already exist in the cache; no rebuilds needed. Cuts a multi-hour debug session to seconds.
15. **Sub-file incremental invalidation via the sandbox read-set** — `cc` opened the header but only read the first 3K of it; if those 3K didn't change, no rebuild. Invalidation at byte-range granularity because the sandbox observed exactly what was read.
16. **Build-graph diffing across commits** — "what changed in the *DAG* between PR and main?" Not the source diff — the build diff. Hot-path regression detection, dep-churn visibility, surprise-rebuild root cause.
17. **Speculative pre-warming** — train on a customer's miss patterns. Pre-build the next likely state (after-merge, after-dep-bump). Customer experiences zero-miss builds because we materialised the cache before they asked.
18. **Closure-shipped one-off compute** — the same machinery that ships build closures also ships arbitrary Vixen closures. Sell "run this on a 64-core ARM box for an hour, attested, cached" as a general remote-exec primitive. Build is one customer; ML / data-eng pipelines are another.
19. **Sandbox-as-product, standalone** — the landlock-based deterministic sandbox sells on its own to teams that want hermeticity for their existing pipelines, no Vixen-language adoption required. Pure infra wedge.
20. **Turing-complete dep policy in the build language** — `accept_dep(d) = license_in(d.license, allowed) and origin_signed_by(d, trusted) and not has_known_cve(d)`. Real code, not YAML rules. Replaces a half-dozen GitHub Apps.
21. **Per-team build-cost attribution** — the DAG knows which node produced which output, owned by which team. Bills minute-by-minute against the team whose slow proc-macro keeps the critical path long. Replaces the cost-tracking spreadsheet enterprise CI ops live in.
22. **Per-input cache hit-rate analytics** — "if you pinned `tokio` and stopped chasing main, your hit rate goes from 31% to 78%." Actionable per-customer, drives upsell and improves fleet utilisation.
23. **JIT'd Vixen kernels on executors** — pure Vixen segments JIT-compiled to native at the executor. Customer writes a hot custom step in Vixen, gets near-native perf, never has to ship a C plugin.
24. **Cross-org cache federation with per-edge trust** — multi-org cache: "accept entries signed by Anthropic OR Google but not random OSS bots." Sellable to consortia: kernel orgs, Rust foundation working groups, etc.
25. **TTL / compliance caches with mandated re-attestation** — "no artifact older than 90 days is reused without re-signing." Critical for SOC2 / FedRAMP. Architecturally free for us, expensive for competitors to retrofit.
26. **Build-event streams piped to live dashboards** — `observe(build) -> Stream[BuildEvent]` falls out of streams-as-runtime-values. Real-time visibility across a 1000-machine fleet, no polling layer to build. Tier 1 has no streams primitive at all.

## Pattern

Tier 1 is "fast cargo." Tier 2 is "we own the build epistemology." Everything that requires knowing *what bytes produced what outputs, signed by whom, deterministically* is a Tier 2 feature.

## Related

- [[Tier 2 — Vixen Build]]
- [[Tier 3 — Vixen CI]]
