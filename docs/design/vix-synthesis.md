# Vix — synthesis v0: vocabulary, invariants, welds

Purpose: fix the VOCABULARY and show how the subsystems weld together, so the design is
"clear and implementable through and through" (Amos's mandate). Names are proposals — Amos
renames at will; the *referents* are the decided design. Companion: vix-language-design.md
(the decision log, same directory). Status: post-round-5 of the design dialogue. Public
vix-the-language notes get drip-fed to ~/oss/facet separately; this is the private working set.

## 1. Vocabulary

**Vix** — the language. **Vixen** — the build technology around it. **Vixenware** — the company;
proprietary runtime + cloud.

**Demand** — the act of forcing evaluation. Enters only at the edge (CLI/API *selection*) and
propagates backward through the graph. Demand is the await: there is no async surface in vix.

**Projection** — a composed access path (`.field[0]`) over an undemanded value. Free to
construct; evaluates nothing; composes like a lens. Selection = a projection demanded at the
edge.

**Thunk** — the finest-grained deferred computation (demand reaches *into* function bodies).

**Node** — the unit of memoization AND execution: one or more thunks fused by *aggregation*.

**Aggregation** — the compiler's cost-model fusing of thunks into nodes. Changes both execution
and caching granularity, on purpose ("we don't memo `2 + x`"). Always sound because pure.

**Closure** — the canonical, content-addressed AST of a computation plus everything it captures.
Its hash joins every memo key (edit code ⇒ its results invalidate). The unit shipped to
executors.

**Observer** — the closure that accompanies an exec on the executor: holds the process handle,
returns values *including streams* (e.g. ferrying rustc's .rmeta mid-compile to downstream
steps that fire immediately).

**Primitive** — a runtime-supplied effectful operation (exec, fetch, VFS, solver steps). The
ONLY carriers of effect. Interface open; implementations proprietary (bring-your-own-runtime).

**Capability** — a typed VALUE granting use of a primitive resource (a toolchain, an exec lane,
fetch). Advertised by daemons, discovered within your access level, passed as ordinary
arguments (effects Option B). Types carry *kinds* (`Toolchain<Msvc>`); ACLs carry *instances*.

**Daemon** — the vixen agent on each executor host. Advertises capabilities, routes jobs,
and ENFORCES advertised invariance (fs-watches a toolchain install; *poisons* in-flight builds
if it mutates).

**Poisoning** — daemon-initiated invalidation of in-flight work whose capability changed
underneath it.

**Fingerprint** — tier-1 coarse identity: "this tree on that machine," "this MSVC install,"
one global hash. For opaque-blob capabilities (MSVC + Windows system libs) reads are NOT
tracked — the daemon only guarantees invariance. For mounted trees, reads ARE tracked.

**Read-set** — tier-2 observed identity: exactly what a command actually read from a mounted
tree — including DIRECTORY ENUMERATIONS (a globber's read-set contains the listing; adding a
file invalidates globbers but not fixed-path readers; deletion is an observable event) and
NEGATIVE LOOKUPS (stat-misses: a compiler probing include search paths read the ABSENCE of
a file — creating that file later must invalidate; the miss is part of the read-set). Keyed
per (closure, fingerprint). Recorded by the VFS (FSKit on macOS / libfuse3 on Linux / NT
driver or paid fuse-for-windows on Windows — ProjFS can't track enumeration).

**Two-tier key** — memo key = closure hash + captured fingerprints, *refined by* read-sets:
a tree change whose delta misses the read-set is a cache hit without re-hashing the workspace
and without bothering the executor. The deliberate anti-Nix pillar: identity = coarse
fingerprint × observed relevance, not "hash 100% of inputs, period." Also legitimizes
overly-broad inputs: hand a whole-workspace handle; only what's read matters or transfers.

**Tree** — the universal artifact currency: Merkle tree of files, hashed recursively, lazily
materialized (executors PULL files on demand; optional statistical prewarm — "PGO for
prefetching").

**Stream** — progressive output as a growing tree: pull-based, immediate delivery when someone
is pulling, backpressured to the producer when not, replayed in order from the store on warm
hits.

**Observation** — any fact learned from outside pure computation: registry snapshot, toolchain
fingerprint, capability ACQUISITION, discovered read-set, directory listing. Every observation
is **pinned**.

**Pin** — a journaled observation value. Addressable; *upgrade* = re-eval "while ignoring this
observation," forcing re-discovery.

**Journal** — the store of pins. Pluggable storage backends; **Cargo.lock is a source of
truth** backend (bidirectional; external edits by cargo/hand/bots are just new observations),
not a render target. The journal doubles as the provenance/attestation chain (below).

**Provenance** — graph-side only (sub-value taint is an explicit NON-GOAL): the memo journal's
(closure hash, input hashes → output hash) records ARE the supply-chain attestation, the cache
explanation, and the error attribution.

**Command grammar** — a snark grammar per external command (rustc, cl.exe, tar…): execs are
TYPED INVOCATIONS, parsed/validated/completed against the grammar. The grammar marks argument
ROLES (input path / output path / behavior flag), so input declaration falls out of the
invocation itself — feeding sandbox ceilings and read-set prediction — plus full LSP inside
command lines, structural invocation diffing, and arg-order-normalized closure hashes. "The
payoff of this entire deranged machinery."

**wtf** — the debugging surface (`vix wtf`): fully interactive browser UI over the journal +
demand traces; time-travel (pure + pinned ⇒ replayable); fable REPL against live graph state.
Headless builds: captured, available in vixen cloud. This is why no-print is a non-issue.

**Grammar CI** — because invocations are typed and journaled, command-line compatibility is
checkable WITHOUT running anything, in both directions: "tell me if the new GCC breaks my
invocations" (validate pinned invocations against the new grammar) and the reverse crater-run —
"I maintain GCC: will this flag change break any open-source vixen build?" (validate the
ecosystem's journaled invocations against a candidate grammar).

## 2. Invariants (quotable)

1. All vix code is pure. Effects exist only in primitives.
2. No async/await surface, ever. Demand is the await. (Painted into a corner ⇒ redesign, not
   syntax.)
3. `fetch` is pure: it requires a checksum. Un-checksummed download is a different, not
   directly exposed thing.
4. Caching is conservative: false negatives always acceptable, false positives never.
   ("I'd much rather rebuild the universe one too many times.")
5. Identity is two-tier: fingerprint × read-set. (The anti-"nix fixes this" bullet.)
6. Every discovery is an observation; every observation is pinned; every pin is addressable
   and refreshable.
7. The closure hash joins every memo key.
8. Aggregation unit = memo unit.
9. Types carry capability KINDS; ACLs carry INSTANCES. Permissions never appear in types.
10. Provenance is graph-side; sub-value taint is a non-goal.
11. Cargo.lock is a source of truth (pluggable journal storage; cargo compat is the wedge —
    100% incl. lockfiles; target/ exempt; no CMake-tier compat for other tools).
12. Capability invariance is daemon-enforced (advertise ⇒ watch ⇒ poison).
13. Every vix value is hashable AND totally ordered. `collect` over parallel fan-out yields
    the CANONICAL total order — never arrival order. Scheduling nondeterminism is
    unobservable by construction. (Non-obvious coming from Rust; load-bearing for caching.)
14. Command grammars are toolchain-versioned and RIDE WITH the capability: the daemon that
    advertises `Toolchain<Rustc>@fp` serves the matching grammar; the grammar version is a
    pinned observation.
15. Floats have a defined TOTAL order: exactly ordered-float semantics (play fast and loose
    with IEEE754 where it conflicts with invariant 13).
16. Syntax spends the FEWEST innovation points: boring, Rust-ish. Explicitly rejected from
    earlier private sketches: application by juxtaposition, pipeline operator, parens as
    function bodies, "weird pattern matching." Kept from them: keyword arguments with
    defaults; DISTINCT literal types for paths vs strings (no string->path coercion).

**Tests & benchmarks (decided)**: a test run is a pure node (closure, inputs) -> verdict;
test SELECTION = cache semantics (rerun only when read-sets intersect the delta) — and that
includes the test binary's DATA files, enumeration changes, and file DELETION (the classic
false-"still passing" in other systems: a deleted fixture must invalidate the verdict).
Benchmarks are OBSERVATIONS, never pure nodes: journaled with executor fingerprint +
timestamps (ALL observations carry timestamps), refreshed deliberately; perf CI = queries
over journaled measurements. Flakiness = a hidden input; retry policy lives in the closure,
quarantine lists are observations. Test stdout/stderr = captured streams, content-addressed.

**RDR — Relink, Don't Rebuild (the cargo-fork phase's centerpiece)**: FEASIBILITY ALREADY
DEMONSTRATED — Piotr Osiewicz has a cargo+rustc fork proving it possible (and very hard).
No feasibility spike needed. Moreover bearcove/rust-rdr ALREADY packages the osiewicz
`api-fingerprinting` rustc+cargo branches as an installable rustup toolchain — the wedge
exists in binary form. Piotr nearly joined; re-approach alongside Lukas once funding/customers
land. RDR is NOT the current focus — the language is. GO decisions: vix grammar v0 (snark,
open, facet monorepo, lua sketch as first corpus) then demand-engine-as-design-oracle
(interpret-only: thunks/projections/memo/total-order collect). Original notes: dependents' compiles
read only upstream rmeta; content-hash cutoff on rmeta makes body-only edits skip every
downstream recompile — only the link re-runs. Requires a (TEMPORARY) rustc fork whose main
job is making rmeta byte-stable under body-only edits (spans embedded in metadata are the
chief noise source; body edits shift later items' spans). Degradation is SOUND by
construction: #[inline]/generic bodies legitimately live in rmeta, so their edits correctly
propagate — content addressing gets the semantics right automatically; the fork only removes
noise, never signal. rustc's incremental dep-graph: IGNORED COMPLETELY (fingerprint-trust,
path-entangled, ICE-prone — a local mutable cache, the opposite of the journal). Patched-
toolchain appetite: OBSCENE (we ship toolchains anyway via rustup subsumption). AST-level
interface hashing (snark rust grammar) can later provide cutoff decisions without splitting
crates.

**check + clippy = selections over the SAME graph**: `check` demands only the metadata
(rmeta) nodes; `build` demands rlib/link nodes which REUSE the very same rmeta nodes as memo
hits; `clippy` is a sibling node (keyed by the clippy capability riding with the toolchain +
lint config) whose OUTPUT is diagnostics-as-values — cached, replayed, invalidated by
read-set intersection exactly like test verdicts. Today's cargo pain (check/build/clippy
thrashing each other's fingerprints in one mutable target dir; check work thrown away by
build; rust-analyzer flycheck fighting the CLI) dissolves structurally: they were never
different MODES, they're different DEMANDS. Fork job addition: make check-rmeta ≡ build-rmeta
byte-wise so check's work always feeds builds. RDR applies to check even better than to
build: body-only edit upstream ⇒ dependents' checks all cutoff with NO link step to pay at
all. Editor diagnostics (flycheck) become graph queries — consistent with CI by construction.

## 3. The welds (how subsystems interlock)

- **Capabilities × memo × journal**: a capability value carries its fingerprint; captured in a
  closure ⇒ joins the key. Acquisition IS discovery ⇒ pinned in the journal ⇒ upgradeable via
  ignore-and-refresh. This is what stops two machines with different MSVC patch levels from
  silently forking the cache — the pin, not ambient state, is the input.
- **Command grammars × two-tier identity**: grammar-extracted roles seed the sandbox mount
  ceiling (what you MAY touch) and predict the read-set (what you'll LIKELY touch); the VFS
  observes the actual read-set (what you DID touch), which becomes the pin that refines the
  key. Declared ceiling / predicted set / observed set — three artifacts, one grammar.
- **Streams × trees × cutoff**: progressive outputs are trees materializing chunk by chunk;
  each subtree hash enables downstream cutoff BEFORE the producer finishes; warm cache =
  ordered replay of the same chunks.
- **Purity × wtf**: because code can't print, the runtime records everything; because
  everything pure is keyed and every impurity is pinned, any past build replays exactly —
  time-travel is a query, not a feature.
- **Structural iteration × scheduling**: map-shaped loops become parallel fan-out discovered
  as the collection's spine materializes; fold-shaped loops are visibly sequential in source.
  The cost model is legible in the code.
- **Effects-B × editor**: daemons advertise → ACL filters → the editor autocompletes the
  capability INSTANCES you actually have ("no more fat-fingering ubuntu:25.04"). The type
  system stays first-order; the tooling does the discovery.
- **fable × vix**: fable goes Rust-style async (a weavy goal anyway — file/network I/O) ⇒
  fable can express suspension ⇒ vix lowers through fable (boring, imperative, debuggable
  middle IR), with the demand/graph layer vix-native above it and weavy stencils below. fable
  doubles as the wtf REPL's poking language.
- **snark × vix**: grammar-as-source-of-truth is the frontend story proven end-to-end on
  gingembre: vix surface syntax = snark grammar ⇒ generated AST ⇒ derived diagnostics
  (state-stack construct context) ⇒ LSP (~assembly given the existing pieces). Command
  grammars reuse the identical pipeline.

## 4. Still open (carried forward)

- Streams: the forgotten design details (chunking, replay granularity, backpressure
  specifics).
- Solver primitive decomposition (PubGrub-ish, parallel; solver calls back into vix for node
  metadata — it doesn't know a manifest is a manifest). Registry layout/list/search/extract
  written IN vix.
- Aggregation heuristics (cost-model inputs exist from the weavy stencil measurements).
- Read-set representation ("a different kind of tree") — concrete structure.
- Windows VFS route: NT kernel driver vs paid fuse-for-windows (ProjFS lacks dir-enum
  tracking). macOS = FSKit, Linux = libfuse3. (Proprietary side, but the language assumes
  read-set observability exists.)
- Capability-threading sugar (Option D implicits) — only if pain materializes during the
  cargo reimplementation.
- Who authors/ships/versions command grammars (per-rustc-release? bundled? community?).
- Ambient-toolchain edge cases beyond the daemon answer ("other aspects not covered").
- Rustup subsumption scope (rust toolchains as CAS-deduped capability artifacts).

**Team context**: Amos's cofounder is a clippy contributor — rustc-dev fluency in-house.
Bears directly on: the temp fork's EXIT strategy (upstreaming rmeta span-normalization needs
someone who speaks the review culture), diagnostics-as-values design (lint-pass internals,
what's per-lint cacheable), and machine-applicable suggestions as demandable artifacts.
Amos himself: built proc-macro support in rust-analyzer for nightly rustc (deep proc-macro
ABI knowledge — relevant to caching expansions under RDR), speaks at the European Rust confs,
and the network includes Esteban Kuber (rustc diagnostics), Oli Scherer (rustc), Ralf Jung
(opsem/miri — reportedly a fan), Predrag Gruevski (cargo-semver-checks — NOTE: semver-checks
computes exactly RDR's question, "did the visible interface change," from rustdoc JSON; a
technically load-bearing friendship), and Lukas Wirth (rust-analyzer lead; nearly joined,
revisit when funding/customers land — flycheck-as-graph-query is his home turf).

**Upstreaming strategy**: rustc changes — upstream, yes. Cargo — NOT in the business of
fixing it: cargo will never grow sandbox/VFS-shaped anything, nor should it. Vixen REPLACES
cargo (drop-in), it does not contribute to it.

## 5. Target programs (sketch-and-critique protocol)

Amos will NOT show prior sketches; the language agent drafts programs, Amos critiques.
Agreed first targets: **Lua 5.x** (clean cross-compilation exercise), then libpng/zlib.
Sketches live in docs/design/sketches/.

## 6. Working rules

Two agents (language ↔ primitives) communicating only through Amos (enforces review +
separation). The language agent does NOT read the old vixen code/notes (anchoring risk on
reversed decisions) — new notes live here going forward. Small slices; constraints written
before code; Amos stays legible as architect. First users are vixenware themselves
(reimplementing cargo); design for experts first, sugar later.
