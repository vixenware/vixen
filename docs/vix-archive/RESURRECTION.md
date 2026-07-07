# Resurrection note — vix machine redesign + the vertical slice (session handoff)

Updated 2026-07-07 (Fable 5, the bonus day). A fresh context reads this +
`vix/docs/content/machine-redesign.md` + `vix/docs/content/redesign/*.md` (ALL of them, esp.
`synthesis.md`, `2-hashing-flesh.md`, `4-typed-schemas.md`) + `vix/docs/content/where-values-are-declared.md`
(Amos's constitutional criterion doc — READ IT, it predates and settles the "two ABIs" question).

## Who / how to work
- Amos (they/them, fasterthanlime). Repo: `~/oss/facet-cc` (github.com/facet-rs/facet, public).
- **Recurring model failure Amos keeps correcting: reductively scoping down / asserting without
  grounding. FIX: ground every claim against the actual artifact; never reduce load-bearing structure;
  call things what they are; read the write-path.** Also: models keep rediscovering what's already in
  the markdown docs — READ THE DOCS FIRST, they are load-bearing and nobody else reads them.
- Method: spawn Paseo agents (`mcp__paseo__create_agent`, NEVER built-in Agent tool; NEVER `[1m]`
  models; `codex/gpt-5.5` auto-review/high for mechanical, `claude/claude-sonnet-5` for mapping,
  opus-4-8/fable for committee). I am "the gate": review diffs skeptically (base-not-stale,
  rename purity, scope, clippy `-D warnings` + `cargo nextest run` green — NEVER `cargo test`,
  never pipe build output into tail/head). Fold via fast-forward push to `origin/rodin`
  (updates PR #2463; push explicitly authorized by Amos; the auto-mode classifier may block —
  it did once; Amos authorized). Keep origin/rodin pushed-current or paseo agents branch stale.
- Impl-agent gotcha learned twice: overly-strict acceptance criteria make agents CONTORT
  (one vendored a patched blake3 to satisfy "no cc in build path" — rejected at gate). Write
  acceptance criteria that state the GOAL, and invite "stop and report" over workarounds.

## TODAY'S GOAL (Amos, 2026-07-07): the vertical slice
Build a non-trivial cargo workspace END TO END in vix: resolve versions (Rodin) → download crates →
extract → virtual file system + sandbox → run the real rustc via vix's `exec` primitive → whole cargo
build. `cargo.vix` exists (shape unknown to Amos). Two scouts mapped the terrain (check their reports).

- **RODIN: the complete Rust implementation is the REFERENCE/ORACLE, not the main line.** An agent
  recently found it and mistook it for the goal. NO vix port exists — only feasibility studies across
  worktrees/branches. The port is the goal; expected near-trivial because scouting is done; target
  ~97% cargo match, deliberately ignoring the last 3% of cargo features for the proof workspace.
  Tell for confusion: the Rust impl is 10× slower in debug builds; vix-through-weavy-JIT is not.
- The vertical slice does NOT need the hash-break train: O(N²)/SHA-256 is a *scale* wall (~15ms at
  proof scale, measured). Slice on the main thread; structural train in background lanes.

## The load-bearing invariant (unchanged)
**Demand-driven evaluation.** Nothing computed unless transitively demanded; memo/read-sets/
incremental depend on it; CANNOT be retrofitted. Tripwire trace-assertions: `vix/tests/demand_driven.rs`.
Corpus-green does NOT prove laziness; only the DriveEvent trace does. Any eager fix must red a test.

## LANDED on rodin (== origin/rodin, PR #2463)
1. flesh → **molten** rename (Amos: "still hot and malleable before it solidifies into the store").
2. JIT default-on; demand tests as integration target; newtypes Handle/FnRef/ContentHash.
3. **taxon extracted** (`phon/rust/taxon`): the shared abstract type vocabulary — `Kind` (was
   phon's SchemaKind), `Primitive`, `Schema`, `SchemaRef`, `SchemaId` — lifted from phon-schema;
   phon-schema is now a transparent facade. Crate name reserved on crates.io (v0.0.0, owner
   fasterthanlime).
4. **SchemaId opaque**: `SchemaId(u64)` private field + `from_raw`/`as_u64`.
5. facet-reflect collapsible_match clippy fixes (workspace is -D warnings clean).

## DAY RESULTS (2026-07-07, the road-2 fork — SETTLED BY MEASUREMENT)
- Amos decided the fork: vix hot loops live IN VIX (no Python/C FFI-kernel future); FFI only for
  atomic pure fns + effects. Acceptance bar RELAXED to ~50× vs Rust ("probably conservative") —
  reuse/correctness-for-free justifies it.
- **Consuming-move landed** (folded, `let x = x.push(v)` rebind receivers skip MOLTEN_DUP): the
  CDCL trail loop went 1.6-3.7ms → 70.8µs (2048 pushes) — within 6% of the reuse ceiling,
  0.54-0.72× the Rust-reference number (caveat: ref does full solve; ladder is like-for-like).
  Zero SHA2 samples on the molten path — the molten thesis is now visible in a flame graph.
- Incremental append-hash technique PROVEN in spike C (7,300× → 16.8× SHA-256, 5.2× blake3-synthetic
  on the store-append path); productionize inside the V2 epoch. Residuals: SHA2 ~54% (→blake3),
  SipHash/HashMap ~20-25% (dedupe map hasher), memmove ~3%.
- **LINCHPIN #2 GROUNDED (spike D follow-up)**: every source-level vix call IS an INVOKE demand
  boundary (INVOKE_HOST interns molten args for the memo key, always; no task-local Op::Call for
  vix→vix; no loop form within a demand region). So NO natural vix loop can keep an accumulator
  molten today — structural gap. Unrolled straight-line control: SHA-256 GONE from profile,
  ~878ns/action ≈ 217× Rust (above the 50× bar; residual = host-call/burst + alloc). MISSING
  LANGUAGE FEATURE (design pending Amos): intra-demand iteration — likely self-tail-call → weavy
  backward jump (interior iteration is NOT demand, aligns with "solve interior is molten" doctrine;
  non-tail recursion stays INVOKE/memoized). Also: unrolled shape stack-overflows lowering >~576 terms.
- **JIT anomaly ROOT-CAUSED**: Driver::spawn → JitProgram::compile = 60.6% of JIT-lane active time —
  recompiles per spawn, no cache. Fix mission fired (jit-compile-cache). Explains JIT 2.58× slower
  than interp.
- rodin 40-search FOLDED (860830cd8): vix-native solve loop (propagation, singleton forcing, clause
  application, candidate selection, chronological backtracking); 4 of 5 differential fixtures pass
  vs real `cargo tree`; weak-optional-feature fixture stays ignored (60-features stage). Language
  awkwardness logged by the mission: no sort primitive; aggregates-in-containers hit
  Realized/Pending/molten barriers (workaround: Int ids + flat row maps); Array.pop tuple surface;
  [String] returns unstable; appended fixtures can't call imported std helpers.
- Committee (2×, converged): fully-inferred surface (NO annotations, NO molten-region construct) +
  a "reuse declined at site S" perf diagnostic; keep pure-dynamic refcount core; MOLTEN_DROP via
  backward last-use = next unlock (risk=aliasing-corruption not eagerness); corpus-wide
  VIX_FORCE_MOLTEN_COPY differential as standing guard; MoltenValue layout moves in lockstep with V1.
- RDR (relink-don't-rebuild): championed by Piotr Osiewicz + David Barsky (NOT Lattimore — he's Wild,
  the fast linker). Amos's map repo: github.com/bearcove/rust-rdr → links to osiewicz rust+cargo
  forks; scout studying the actual fork diffs. Our angle: we can compute span-normalized item
  identity OUTSIDE rustc (our content-addressed AST) — decision half solved; artifact half (SVH/
  DefIndex instability) may need the patches. Test selection: relocation-walk over .o link inputs
  (gc-sections/ICF machinery) = predicted read-sets; observed read-sets later. rmeta cutoff = the
  compile-wave killer; span noise needs rmeta projection-hashing.

## END-OF-DAY STATE (2026-07-07, ~14 folds on rodin)
FOLDED, in order: taxon extract → SchemaId opaque → facet-reflect lints → taxon collapse →
version-semver (+String ordering) → facet-json lint → molten consuming-move → rodin 40-search
(4/5 fixtures vs cargo) → weavy DWARF type DIEs → crates.io round trip (real network) → JIT
compile cache (per-program not per-spawn) → generic build walk (THE SLICE: manifest → rodin solve
→ demand-driven walk → real rustc → matches cargo unit-graph oracle) → facet-core taxon_bridge
(Shape→Kind classifier) → TAIL LOOPS (self-tail-call → Op::Jump; 100k-iteration molten
accumulators; await ready-token protocol in weavy interp+JIT+stencils; 9 tripwires).
SPIKE BRANCHES (evidence, no fold): spike-c (CDCL: named-rebind 70.8µs ≈ reuse ceiling, sub-bar),
spike-d (LR: recursion=INVOKE finding → led to tail loops; JIT recompile finding → led to cache),
spike-e (rmeta: naive hash useless, external surgery not shippable — needs rustc-side projection,
i.e. the Osiewicz patches), spike-f (reloc-walk test selection WORKS; panic-Location rodata is the
maskable false-positive source).
DECISIONS: exec composition = (b) vox RPC protocol (public protocol types, private impls; never a
vixenware Cargo dep on vix). V3 hash epoch FIRED (branch v3-hash-epoch): the staged
V3+V1+V2 train — typed schemas → kill string dispatch → containers as declared descriptors
(MoltenValue lockstep!) → blake3 + carried incremental hasher + ReadObservation newtypes.
Vixenware stage-1 sandboxed executor re-aimed at protocol-service shape (vx-services).

## IN FLIGHT (check `mcp__paseo__list_agents`)
- **taxon collapse**: taxon-id merged INTO taxon (one crate, no_std+alloc, BTreeMap in resolve_ids,
  workspace blake3 default-features=false — vendored-blake3 was REJECTED at gate). Branch `taxon-collapse`.
- **weavy DWARF type DIEs** (branch `weavy-type-dwarf`): extend `weavy/src/jit/dwarf.rs` from
  code-only to code+type debuginfo, lowered from `Descriptor` (the (b)-ABI observability obligation).
- Committee agents (codex a3d765be + opus ca3948ce) alive for diff review.
- Scouts: rodin terrain (e4fb5353), slice terrain (e6129c6c).

## The ratified design (committee 2×, Amos-ratified — do NOT relitigate)
- **taxon** = one bottom crate: Kind + Primitive + Schema/SchemaRef/SchemaId + identity algorithm
  (blake3; SchemaId = first 8 LE bytes of blake3 of canonical structural encoding — the canonical
  encoding is a FIXED spec, `r[schema-identity.canonical-encoding]`).
- **Two ABIs, both realizing a taxon::Kind** (per `where-values-are-declared.md`, constitutional):
  (a) facet-discovered (rustc dictates layout, we align; Facet-derived defined-repr types only;
  repr(Rust) out of scope — wrappers) and (b) weavy-declared (WE control: NRVO, placement-new,
  custom niches — paid for by observability: runtime introspection via Descriptor + DWARF type DIEs).
  vix programs DO construct Rust values → the facet bridge needs facet-reflect (Poke), lives in a
  standalone crate (facet-reflect + weavy + taxon, no phon), post-V10, additive, hash-neutral.
- **facet Type/Def stay as siblings**; `Shape → Kind` is a lossy projection (Vec/SmallVec/VecDeque
  all collapse to Kind::List — params-erasure nuance Amos flagged; audit dispatch sites on migration).
- **DeclId: deprecate, don't delete** (#[deprecated]); introduce SchemaId alongside. DeclId is
  const + params-erased; SchemaId is computed + structural. 15 dispatch sites migrate at leisure.
- **vix KEEPS phon for serialization** (memoization/on-disk need the codec) — it stops using phon
  for TYPES only. "Zero phon dep" was wrong.
- **Pending on the Handle (HandleTier), not in SchemaRef.** Result stays Kind::Enum. SchemaId stays u64.
  weavy stays generic (no hard taxon dep).
- **ONE hash break**: V3 (typed schemas: Descriptor<String>→Descriptor<SchemaRef>, SchemaId as the
  frame word, delete schema_refs + lower.rs:118 assert, string dispatch → match Kind) + V1
  (containers as real declared descriptors) + V2 (blake3 + incremental: carried blake3::Hasher on
  molten ArrayWords — THE measured 50×; maps deferred, trail is array-shaped). No compat shims —
  no cross-process persistence exists yet, so intermediate breaks on the branch are free.
- Then V10 (View pull accessors; ~24 of 57 host ops collapse) → facet bridge → V4 (registry for the
  ~33 survivors; HostCtx reentrancy audit gates) → V5 (parsers out: AST first — already Snark;
  VersionSet ports with semver crate as differential oracle; ELF → registered capability INTERIM —
  Amos wants binary-snark eventually: a Kaitai-shaped second declarative dialect beside the grammar
  dialect, sharing lower-to-weavy/query/viz. ALSO kill the `call()` keyword special-forms
  lower.rs:3155 — the V8 language-level leak, the ELF/OCI/version builtins were Fable's own debt).

## The bigger vision (Amos confirmed shape)
Self-describing content-addressed computational substrate: taxon under facet (Rust realization) /
weavy (owned ABI: layout+VM+JIT+debuginfo) / phon (wire), snark (declarative parsing, text now,
binary later), vix (demand machine + language), vx-store (Blake3 CAS, private), Vixen (build system,
above the language, private vixenware), dodeca (docs/annotations), gingembre (templating), fable
(imperative proto-vix, easier lowering, possible stage-0). Bootstrap cycles resolved by serialized
weavy bytecode artifacts (stage-0), eventual self-hosting. Highlight of the week: vix keeps solving
problems hit while implementing vix itself (memoization/hashing/structural-sharing hand-written in
Rust that vix gives free).

## Deliberate holes (METHOD — read this before "helpfully" reconstructing anything)
Amos DELETES wrong-shaped implementations on purpose so agents can't absorb a mistaken direction
(existing code recruits agents; documents don't). Two known instances: the Rust rodin resolver
(retired — "no more code, contribute only documents") and the vixen DAEMON (THREE prior
implementations deleted; the hole in vixenware where a daemon should be is intentional).
RULE: when you find a conspicuous hole, do NOT excavate git history and resurrect what was there —
ASK Amos, or read this note. The architecture answers live below and supersede anything in history.

## Vixen service architecture (Amos, 2026-07-07 — do not re-derive)
- Vixen DAEMONS run on many computers; a daemon provides ambient CAPABILITY SERVICES on every node
  (VFS mounting via vx-vfsd, sandboxed process launch via seatbelt/landlock, local-disk watching) —
  useful even on nodes that never build. The RUNNER is a separate service that ADVERTISES
  capabilities (ambient toolchains etc.); work is dispatched TO it; it composes the daemon's
  VFS+sandbox services over RPC. Single-node: CLI → daemon → dispatch to runner → runner asks
  daemon for mounts + sandboxed launches. Runner implements/hosts vix's MachineExecBackend.
- vfsd prefixes are VIRTUAL subdirs under ONE real mount; isolation enforced by the sandbox
  (a process can only read its granted prefixes) — per-memoized-exec prefixes are cheap.
- seatbelt/landlock = confine-only (no audit trail); vx-vfsd = the observer, and its
  `tracked_observations` RPC already exports File/Directory/LookupMiss ≅ vix's ReadObservation.
  Stage 1 = confine-only (undeclared reads become loud failures); stage 2 = observe via prefixes.
- ReadObservation's naked u64 hashes → NEWTYPES + Blake3 (Amos: naked u64/String where a newtype
  belongs is the V6 disease — always fix on contact). Windows sandbox/VFS: explicitly deferred
  (may need kernel driver or paid FUSE license; no demand; early research done — don't spend time).

## Gotchas
- dodeca annotations: inline `<!-- note … -->` in content .md SURVIVE targeted edits, NOT wholesale
  Write. Use dodeca-annotations skill. (RESURRECTION.md is mine, no annotations, safe to rewrite.)
- Paseo baseBranch resolves origin/rodin. cargo nextest not cargo test. No println — tracing.
- facet-cc has a parallel SWIFT phon implementation (phon/swift/) — greps for Rust symbols will hit it.
- vox-codegen emits "SchemaId(...)" as TEXT in generated Swift/TS templates — not Rust constructions.
