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
No feasibility spike needed. bearcove/rust-rdr packaged the osiewicz `api-fingerprinting`
rustc+cargo branches as an installable rustup toolchain — LAST YEAR; little progress on
Piotr's side since. A private Discord exists for interested parties; the only other active
person was David Barsky, since departed to co-found East River (source-control company /
GitHub replacement — friendly with Amos but partially a competitor: they want CI, vixen
also solves CI). Piotr nearly joined; re-approach alongside Lukas once funding/customers
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
- **picante × fable × vix (the query plane)**: the invariants chosen for BUILD caching —
  purity, every value hashable and totally ordered, canonical content-addressed closure
  ASTs — are exactly salsa's preconditions, the ones Rust can't enforce and picante must
  demand by macro+discipline. Vix enforces them in the language, so a vix/fable function
  maps to a salsified query with NO annotation and no type-system fuckery: the language
  cannot express a non-query. Fable closes the data plane: the generated vix AST derives
  Facet, fable computes over facet values, picante persists memo tables via facet-postcard
  — one reflection layer from parse tree to memo table. Picante is the demand engine seen
  from Rust; fable is a query seen from inside. Bootstrap: (v0) interpret-only oracle in
  Rust — testing QUERY semantics and build semantics as the same thing; (v1) native demand
  engine implemented ON picante (Rust queries hosting vix thunks, cache persistence free,
  picante battle-tested by its hardest customer); (v2) self-hosted intelligence queries —
  resolve/type_of/hover WRITTEN IN vix, running incrementally on the engine; the Rust
  binder becomes the reference oracle for its own vix reimplementation. Stable-across-edit
  identity is not a new ItemId scheme — it's canonical-value hashing, the same content
  addressing that keys the build cache; early cutoff = hash equality = the conservative-
  caching pillar. Prerequisite exposed: self-hosted queries need vix's user type surface
  (enums/structs/match over them), which doesn't exist yet — a forcing function for the
  type-system design round.

**Grammar v0 SHIPPED** (facet monorepo, playgrounds/snark/src/bundled/vix/, commit
ce8e6d39e on snark-playground-rebased): full lua sketch parses clean (475 nodes, 0 errors),
highlighted live in the playground at 3.7ms via DirectStencil. Notable grammar decision:
flag atoms are legal ONLY as array elements — per-state lexing makes `a - b` vs `[-DFOO]`
unambiguous BY CONSTRUCTION (a state either expects a flag or a binary minus, never both).
Command blocks parse as token soup + {expr} splices; per-command grammars refine via
injection later. Broken input already yields structured errors with state_stack — the
derived-diagnostics engine is grammar-generic and ready to be pointed at vix.

**Typed AST SHIPPED** (vix crate, commit bc7db70b6): build.rs derives the ENTIRE typed AST
— types AND the resolved-CST lowering — from the grammar's `field()`s + cardinality (bare →
T, optional → Option<T>, repeat/sepBy → Vec<T>); hidden choices become enums, mixed
alternatives become annotated ad-hoc enums (Callee, ArrayElem, CommandPart). The ast()
annotation file carries only names + leaf decodes. Found & fixed en route: snark dropped
`field()`s written inside `repeat(...)` (hidden reductions discarded field events instead of
hoisting them tree-sitter-style, commit 329f5798e). Known gap: fields on anonymous TOKEN
steps never reach the resolved tree; codegen routes around it via token-set scan.

## 3½. LSP: rings, not a parallel universe

Why most LSPs are rewrites: the compiler's front-end isn't queryable, so editors rebuild it
from scratch and the two drift forever. Vix's front-end is a grammar + a generated AST + (
eventually) a demand engine — all queryable. So IDE features layer as rings, each ring paid
for once:

- **Ring 0 — free from the grammar** (any snark grammar, zero per-language work): syntax
  highlighting (queries — shipped), syntax diagnostics with construct context (state_stack —
  shipped), folding (block nodes), selection expansion (CST ancestry), outline (`field("name")`
  convention), and — the snark-special — **exact completion of legal tokens from the parse
  state**: per-state lexing means the table KNOWS what may appear at the cursor. The
  `[-DFOO]`-vs-`a - b` design decision becomes completion behavior automatically: inside an
  array you're offered flags, in expression position you aren't. Incremental reparse (parse
  sessions + node reuse) already exists.
- **Ring 1 — declared semantics** (per grammar, still data not code): which fields are
  definitions, references, scope roots — tree-sitter's locals.scm/tags.scm convention, or an
  ast() annotation extension. Buys approximate document-highlight/rename for ANY grammar.
- **Ring 2 — the binder** (per language, small, over the GENERATED AST): scope tree + symbol
  table + reference resolution, written against the same typed tree the compiler consumes —
  when the grammar changes, the AST regenerates and the binder breaks loudly at compile time
  instead of drifting. For vix v0: fn names, params, lets, closure params, use leaves.
  Buys real go-to-def / references / rename / hover. **The binder is SHARED with the demand
  engine** — evaluation needs name resolution too. One resolver, two consumers; the LSP is
  not even an internal parallel universe.
- **Ring 3 — the demand engine IS the language server**: check+clippy generalized. Hover =
  a demand for a node's type/value/closure-hash; "why did this rebuild" = a demand against
  the journal, answered inline in the editor; go-to-def on a Tree value jumps to the
  producing rule. IDE features are demands over the same graph; diagnostics stay values.

Delivery vehicle: the playground drives the ring APIs directly through wasm (no JSON-RPC) —
CodeMirror hover/cmd-click/rename UI, Playwright-verifiable end to end. The protocol server
(tower-lsp, editor extensions) is a later, thin shell over the same core.

Sequencing: (1) spans into the generated AST (mechanical codegen addition — the one thing
the AST currently drops), (2) the vix binder (shared prerequisite of oracle AND LSP),
(3) demand-engine oracle on top (agreed slice), (4) playground IDE ops for vix, (5) Ring 0
generalization for all bundled grammars, (6) Ring 3 once the oracle answers demands.

**THE WIRE SHIPPED — vix-wire** (be907a803): the executor as a vox service over real
websockets, carrying the recovered intent on current seam types. Producing handles:
PathReady streams per output as the tool writes it; the canonical rmeta-before-rlib test
passes over the socket (consumer fetches lib.rmeta mid-run while gated "codegen" blocks —
no whole-tree hash exists yet). Observer closures: shipped vix closures evaluated ON the
executor against Run; `|run| run.out.glob("*.o")` crosses back as [main.o] — the
projection, never the world. Gravity: pull_from moves trees executor→executor; the
orchestrator holds hashes, never bytes. Still open, on purpose: producing-path caching
(the L1-before-hash question) and observer identity joining the wire-side key (local
ExecCache has it; the wire path doesn't cache yet); seatbelt lands runtime-side.

**ARCHEOLOGY RECOVERED (git history of vixen/, purge-lost, salvage-worthy)**. Standing
caveat from Amos: these are EARLIER, half-cooked iterations — recovered for their
INTENT/IMPETUS, not as corrections to apply wholesale. Where they disagree with the
current arc (surface syntax, ontology details, DI machinery), the current design wins
unless we deliberately re-decide. What we take: observer-generic-over-return-type,
observer identity in the key, per-path Producing handles, the seatbelt profile shape +
canonicalization trap, and the vox-wire precedent.
- **Seatbelt profile generation** (d3c2b328, June; earlier f4effa0a, Feb): SBPL
  allow-default base (dyld/libsystem must run) → deny network* → deny file-write* except
  output roots + /dev — bounds the write-set the VFS flushes to exactly the declared
  outputs. THE TRAP: canonicalize roots first; macOS resolves symlinks (/tmp →
  /private/tmp, the FSKit mount) BEFORE the seatbelt check, so an unresolved path never
  matches. Real host test (no VFS needed): allowed root writable, sibling denied, reads
  fine. Read confinement deliberately deferred = "design cache key + sandbox together".
- **Observer closures** (b02c042f, 6914d2ae, 2ed62906): exec is a BOUNDARY, not an
  intrinsic. An invocation is a backtick literal; `with { observer: (run) => … }`
  decorates it. The observer is GENERIC OVER ITS RETURN TYPE: it ships to the executor,
  evaluates THERE (same evaluator semantics), and only its RESULT (or store-backed
  handles) crosses back — the observer's return type IS the exec node's type. The
  observer's identity JOINS the cache key (proven: `(run) => run.ok` vs
  `(run) => id(run.ok)` are different bundles, must not alias in L1). `Run` is what the
  observer sees: `run.ok`, `run.stream.{stdout, stderr, out}` — fields poison if demanded
  before they land.
- **How this solves early rmeta**: the output tree is a PRODUCING handle — per-path
  progressive: `tree[path]` resolves as the producer writes it, blocks if not yet
  written, poisons if never; the whole-tree hash exists only at flush. So a consumer
  demanding `out / "libA.rmeta"` unblocks the moment rustc writes the rmeta, while the
  rlib is still in flight — before the atomic tree hash CAN exist. Cargo pipelining
  falls out of demand granularity instead of being a special case. Canonical test
  existed: cargo_pipelining_serves_rmeta_before_rlib. Granularity per kind: tree→path,
  stdout→chunk, observer→field, scalar→atomic. Tree value model settled: Resolved (CAS
  hash, immutable) | Producing (progressive); merge = right-wins (tentative; log-clobber
  open). LIVE OPEN QUESTION carried forward: producing-handle vs L1-key collision —
  pipelining starts before the output hash exists, so what does the cache entry point at
  mid-flight?
- **The vox precedent** (747305f8, f8e50cde): the remote-handle harness already ran over
  vox once (transport-local), and strands parked on cross-executor parts. "Don't skip
  the wire" has a proven prior.
- **Exec ontology** (2ed62906): Command = the grammar, a static CONSTANT (gcc and clang
  are different commands; "any C compiler" is a layer above). Tool = command + executor
  identity + binary + fingerprint + probed params (mixed static/runtime, filled on
  injection). Invocation = the backtick literal; forcing runs it. Two-tier validation:
  compile-time const-eval of tool.command validates tokens offline; exec-time, the
  injected provider's version parameterises the predicates (version-aware diagnostics).
  Capabilities as DI: build code requests `Type where (predicate)`, executors probe +
  advertise providers, the control plane matches + injects; ROUTING FALLS OUT OF
  PROVENANCE. (Note: vix-the-open-language currently spells invocations `cc! { … }`
  rather than backticks — surface question to reconcile, semantics identical.)

Convergence note: oracle exec step 1 (canonical closure shipping) + this recovered
design = observer closures are implementable in the oracle NOW — an observer is a
shipped vix closure evaluated executor-side against a Run value; its result is what
crosses. The vox executor slice should implement observer + Producing handles from day
one rather than atomic Outcome bytes.

**CWD IN A FULLY SANDBOXED EXEC (answer to Amos's question)**: there is no cwd. Ambient
cwd is three bugs wearing one trenchcoat: an identity leak (relative argv paths resolve
against it, so it must join the plan hash or two different computations share a key), a
determinism leak (tools EMBED cwd in outputs — debug info, __FILE__, depfiles — so a
fresh-ULID prefix per run poisons the cache with path noise), and a capability leak (a
cwd outside the mounts is authority the ceiling never granted). The resolution vixen's
runner shell already gestures at: every plan path is PREFIX-RELATIVE, and the process's
cwd IS the action prefix root, always, by construction — scratch, empty at start,
writable. Consequences: (1) the prefix path must be derived from the ACTION IDENTITY
(hash of the plan), not a fresh ULID, so embedded-path leaks are at least deterministic;
(2) command grammars carry the per-tool remedy (inject -fdebug-prefix-map/-ffile-prefix-
map style rewrites) for tools known to embed; (3) read-after-write inside the prefix is
NOT an observation — the read-set is reads-from-the-WORLD, not reads of your own scratch;
the VFS must distinguish; (4) `..`/getcwd above the prefix: deny-default seatbelt makes
the parent opaque; sibling action prefixes must never enumerate.

**REMOTE HANDLES + STREAMING (the vox wire, next implementation slice)**: Outcome stops
carrying bytes; it carries TreeHandle { hash, home: executor, size }. Bytes live in the
producing executor's local CAS. The orchestrator moves HANDLES and PLANS (small); trees
move EXECUTOR→EXECUTOR on demand (pull by hash, chunk stream, verify-on-arrival,
have()-negotiated dedup so only missing blobs travel). Canonical/central CAS ingestion is
ASYNC (durability/sharing), never on the critical path. vox service surface v0:
exec(request) -> stream { Started, Stdout(chunk), OutputTree(handle), Finished(result) };
fetch_tree(hash) -> chunk stream; have(hashes) -> present-set. Tests go over the REAL
wire (vox websocket/tcp on localhost), two executor processes, orchestrator pulling
nothing but handles — "don't skip the wire".

**SCHEDULER, FIRST POLICY (gravity-first, latency-aware)**: placement must never affect
outputs (purity) — so the scheduler is FREE to be clever. Policy v1: run an action where
its heaviest missing input already lives; stream the lighter inputs across executors;
cost(action, host) = max over inputs of (missing_bytes / bandwidth(home→host) + latency)
+ queue_wait(host) + estimated_run (benchmarks are observations — reuse them as the
estimator). The orchestrator is a control plane only: routing trees through it (or
through the canonical CAS) systematically is the anti-pattern; executor↔executor
transfer is the default data path. Requires: handles carry sizes; the fleet learns a
bandwidth/latency matrix from actual transfers (observations, journaled like everything).

**VIXEN RUNTIME RECON** (real sandbox/VFS state, checked 2026-07): vx-vfsd BUILDS clean
(41s cold) — FUSE/FSKit-backed VFS daemon with per-action prefix dirs; vx-fskit +
Vixen.app (Swift package, apps/mac) present with FSKit checkouts; vx-services/runner.rs
is a protocol SHELL (post-purge, v21) whose shapes converged with the oracle's seam
types (ExecMount = tree-hash mounted at prefix-relative path; mount → run → flush spine);
seatbelt PROFILE GENERATION is referenced ("joins next") but not yet implemented —
that's real derisk surface, not just wiring. Exercising Vixen.app/FSKit end-to-end needs
a keyboard session (extension approval dialogs).

**COMPOSITION SHIPPED — lua.vix BUILDS inside the oracle** (de6d85e0e): cc!/ar! evaluate
through the two-tier cache; command names resolve to capability VALUES in scope; Tree
splices become mounts at ROLE-STABLE paths (/m/0 by splice order — content-derived paths
would leak the world into the identity and kill tier 2); collect over trees = canonical
merge (outputs-as-mounts chains execs). End to end: fetch → extract → subtree →
glob/filter → 3 compiles + archive + link; warm rebuild = ONE event (Hit{lua}). The seam
composes: fn-memo MISS (arg tree changed) resolving to exec Tier2Cutoff (nothing read
changed) — each cache doing its half. TWO SKETCH BUGS THE ORACLE CAUGHT: {src / unit}
alone compiles a unit without its headers (object() gains -I {src}); ar! with
{objs.glob("*.o")} splices bare relative paths with no anchoring mount (the TREE is the
right splice). The oracle is doing its job: running programs falsifies sketches.

**THE EXEC SEAM (two-tier keys), designed and derisked** (94897f9ea, vix::exec): the
runtime ANSWERS QUESTIONS ("what happened when I ran this" = the observed read-set; "what
is the world now" = Snapshot point queries) and enforces the ceiling; the language OWNS
CORRECTNESS (what a key is, when reuse is legal, what an observation means). The placement
that matters: `verify()` — does this read-set still hold — is language-side, so
false-positive-never cannot be implemented wrong by a runtime; a buggy VFS causes false
negatives (safe), never false positives. What crosses the seam is facet data: ExecPlan
(argv + grammar-assigned Roles — the ceiling/predicted/observed triple's input), Mount,
ReadSet (content reads + dir listings + NEGATIVE lookups, BTreeMap = canonical/diffable),
Outcome. Tier 1 = closure × plan × capability fingerprint × coarse mount fingerprints
(pre-run, exact over the declared, over-invalidates). Tier 2 = candidates indexed by
plan × capability (capability INCLUDED: only the world may vary under read-set
verification, never the computation); verified read-set ⇒ reuse without running ⇒ re-pin
under the new coarse key. Proven by probes: the ANTI-NIX test (unread file edit ⇒ tier-2
cutoff, no rerun); the CROWN JEWEL (new header shadowing an old one earlier in the search
path — the case every mtime/depfile system misses — diverges the pinned Absent ⇒ rerun);
capability-change isolation; sandbox-as-world (reads outside mounts fail inside the tool);
listing divergence on additions AND deletions (the jade.fyi case). Still to derisk:
outputs-as-mounts composition (chained execs), command-grammar role extraction via snark
injections (toy roles stand in), wiring `cc!` blocks in the oracle to this cache, env/cwd
in the plan, and the observation-upgrade flow ("eval while ignoring this pin").

**Exec sequencing decided** (ship → advertise → exec): capability = ADDRESS, shipped
closure = PAYLOAD, exec = demand evaluated where its captured capabilities live. Step 1
SHIPPED (30f801296): canonical form = generated strip_spans() (spans zeroed) + facet-
postcard bytes — ONE form for identity and transport; Value derives Facet; ship()/receive()
move closures between oracles; test proves exec = eval elsewhere (same value by purity,
same canon_hash across the wire, same hash from differently-formatted identical source).
Step 2 next: fleet registry — hosts advertise capability instances (kind, fingerprint,
versions, command-grammar ref); acquire resolves against it; fingerprint mutation between
calls = poisoning, testable as memo misses. Step 3: exec stub — command block + splices
resolve against the capability's command grammar into a plan (argv, declared inputs),
shipped to the advertising host, stub-executed, result = journal observation with declared
read-set. The interface (payload format, advertisement schema, plan shape, observation
shape) is the executable contract for the proprietary executor to implement behind.

**(3) ORACLE SHIPPED** (commit d15c85a65, vix::oracle): interpret-only executable model.
What it makes testable TODAY: memo keys = canonical-AST-hash × args-hash where identity is
the AST modulo spans (whitespace/comment edits anywhere preserve a fn's hash; real edits
change exactly that fn's); aggregation unit = named fn call; observations pinned in a
journal (fetch replays by checksum even across memo misses; acquire fingerprints
capabilities); partial application as first-class Partial values; collect = canonical total
order; Miss/Hit/Observation{replayed} event stream. Runs the whole corpus: eval.vix
demo() = 42.0, types.vix toolchain() end-to-end. Deliberately NOT modeled yet (next design
probes): laziness/demand-into-bodies, projections-as-lenses, streams, command blocks/exec.

**(1)+(2) SHIPPED** (commits 97ccef52e, 05d98b101): every AST node carries its span, leaf
payloads are Spanned<T> (occurrence-level spans — the rename unit). Binder decisions made
and testable (critique welcome): command names are VALUE references (`cc!` invokes the
capability `cc` in scope — renaming the binding renames the invocation); identifier
patterns (`Linux => …`) are constructor-like with no enum surface yet → recorded in
`unresolved` alongside prelude-less primitives (fetch, extract) and unimported types (Flag)
— unresolved references are VALUES for the future type/prelude layers, not errors; kwarg
names name the callee's parameter; lets are sequential (`let x = f(x)` sees the outer x);
one namespace for values/types v0. bind() → symbol_at/references/occurrences/rename_edits.

## 4. Still open (carried forward)

- Streams: the forgotten design details (chunking, replay granularity, backpressure
  specifics).
- Solver primitive decomposition (PubGrub-ish, parallel; solver calls back into vix for node
  metadata — it doesn't know a manifest is a manifest). Registry layout/list/search/extract
  written IN vix.
- User type surface (enums/structs + match over them): required before intelligence
  queries can self-host in vix (you can't write `type_of` with only Tree/Path/Flag).
  Doubles as the forcing function for the type-system design round.
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
