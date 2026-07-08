# Capabilities & command grammars: live-tree survey (2026-07-08)

Read-only sweep of the current tree (branch `rodin-workspace-scope` @ `7083e47e`) against
`docs/design/capabilities-ambient-vs-materialized.md` (the baseline — three same-day
dictations: ambient-vs-materialized split, the ecosystem/grammar layer, the two-tier key +
geography correction). Covers `docs/design/**`, `docs/dev/**`, `docs/content/**`, and
`crates/**`. A separate agent covers git-history archaeology; this is the live tree only.

Method: four parallel Explore sweeps (top-level `docs/design/*.md`; the `briefs/ sketches/
redesign/ rodin-fact-validity/` subtrees; `docs/dev` + `docs/content`; `crates/`), synthesized
below. Line numbers are as reported by each sweep; re-check before citing externally, docs
churn fast in this tree.

**Headline finding**: most of the dictated design is not new invention — it restates content
already ratified elsewhere in this same tree, some of it as normative invariants (`vix-spec.md`
V9, V27–V29) and some as a decided design round (`vix-synthesis.md` §3⅞, "decided 2026-07-03").
The genuinely new material appears to be: the packages-resolve-via-rodin framing for
capabilities (2nd dictation), the specific "grammar computes tier-1 from input NAMES before
reading bytes" formalization (3rd dictation), and the geography/jurisdiction correction (3rd
dictation, correcting an earlier "lazy-remote-mount" consequence). Two claims in the baseline
doc about *already-shipped* code do not hold up against `crates/`: "capability advertisement
shipped recently... in the vx-runner protocol types" and "the vix command-role classifier is
the embryo" both point at code that is either an empty stub or doesn't exist.

---

## Comparison table

| # | Element (dictated) | Status | Where |
|---|---|---|---|
| 1 | Materializable (rustc-class) vs. ambient (Xcode/MSVC-class) toolchains; shrinking boundary | **PRESENT** (design), **PARTIALLY** (code), **CONTRADICTS** (one product doc) | see §1 |
| 2 | Daemon fingerprints + FS-watches + poisons in-flight builds on ambient-toolchain mutation | **PRESENT** (design, pre-existing, verbatim), **ABSENT** (code) | see §2 |
| 3 | Command grammars: summarization / normalization / code-intelligence / validation | **PRESENT but CONTRADICTS on shape** (design already has a *different*, five-item enumeration), **ABSENT** (code) | see §3 |
| 4 | Capabilities-as-packages; probe is a package parameterized by the grammar; resolves via rodin | **PARTIALLY** (capability-as-advertised-typed-value is old news; "resolves via rodin like any dependency" is new); **CONTRADICTS** (the one concrete "shipped" code claim) | see §4 |
| 5 | Two-tier cache key: Tier 1 = normalized command + capability identity + input names; Tier 2 = + content hashes over observed read-set | **PRESENT** (design, pre-existing, detailed), **ABSENT** (code), caveat: ceded as non-novel mechanism vs. BuildXL/Vesta | see §5 |
| 6 | Materializable ⇒ cheaper not free; geography/jurisdiction still constrains placement | **ABSENT as a connected argument** — the natural home document doesn't make this connection | see §6 |

---

## §1 — Materializable vs. ambient toolchains, shrinking boundary

**PRESENT, and pre-existing** — this is Amos's own original wording, already in the tree before
today:

> `docs/design/vix-language-design.md:33-39`: "**Ambient toolchains** (can't legally
> redistribute): NOT an executor property ('that's what bazel does and I fucking hate it'). A
> **vixen daemon** runs on each executor host and ADVERTISES CAPABILITIES; local-toolchain
> access is a capability... the daemon is RESPONSIBLE for advertised invariance — e.g.
> fs-watches every file of an MSVC install and POISONS in-flight builds using that capability
> if anything changes."

Same-day companion (`package-manager-and-provenance.md`, dated 2026-07-08, likely written just
before or alongside the baseline doc):

> `L105-113`: "**ambient toolchains are RUNNER-ADVERTISED CAPABILITIES, not ad-hoc checksums.**
> A build-script unit DECLARES its C-toolchain requirement; the runner advertises the toolchain
> (path + probed identity) in its capability set; the receipt records the capability identity.
> rustc entered exec identity this way (-vV); cc follows."
>
> `L115-124`: "**Final**: there is no pkg-config, no ambient libcurl. **We build curl.** ...
> The ambient world shrinks to the pinned toolchain, then to nothing." — verbatim match for
> "deliberately shrinking boundary," pushed even further (to *nothing*, not just smaller).

`vix-synthesis.md:108-110` explicitly puts rustc at the *easy* end of the shrinking-ambient
spectrum ("of all toolchains to tame ... rust's is easiest"), and frames the underlying identity
model as a **spectrum**, not a crisp binary:

> `vix-language-design.md:95-98`: "Identity is TWO-TIER... capability inputs span a spectrum:
> (a) OPAQUE BLOBS (MSVC + Windows system libs): global fingerprint, reads NOT tracked... (b)
> TRACKED TREES: mounted, all reads/writes recorded, lazily materialized."

**PARTIALLY in code**: `crates/vx-vfsd/src/vfs.rs` implements a real, working CAS-mounted
`toolchain/` prefix under an action prefix, Blake3-hash-addressed (module doc lines 1-21,
`ActionPrefix::path_mappings` comment at line 78). This is the *materializable* half only —
content-addressed, hash-identified a-priori, exactly as the design describes. There is no
corresponding *ambient* half in code (see §2 — no fingerprint/watch/poison implementation
anywhere).

**CONTRADICTS** — a live, current product doc gives rustc the opposite treatment from the
dictation's canonical example:

> `docs/content/using/install.md:39-42`: "Toolchains (rustc, cc) are *discovered and pinned*,
> not bundled: the runner advertises what it found, and builds record exactly which toolchain
> produced what. Reproducibility comes from recording, not from pretending every machine is
> identical."

The dictated design (element 1) uses "rustc-class" as *the* paradigm materializable toolchain
(download/mount into CAS, a-priori hash identity). `install.md` describes rustc/cc as
discovered-and-pinned — a-posteriori identity via recording — which is precisely the *ambient*
contract, not the materializable one. Either the design intends to *change* how rustc is
handled (consistent with "the boundary shrinks over time" — rustc migrates from ambient to
materializable), or `install.md` is stale relative to today's ruling. Worth an explicit
reconciliation pass on `install.md` if the ruling stands.

---

## §2 — Fingerprint + FS-watch + poison contract

**PRESENT in design, pre-existing, and extensively cross-referenced.** Beyond the
`vix-language-design.md` quote in §1, the fullest statement is in the working record:

> `vix-synthesis.md:39-45`: "**Daemon** — the vixen agent on each executor host. Advertises
> capabilities, routes jobs, and ENFORCES advertised invariance (fs-watches a toolchain
> install; *poisons* in-flight builds if it mutates)."
>
> `vix-synthesis.md:50-52`: "**Fingerprint** — tier-1 coarse identity... For opaque-blob
> capabilities (MSVC + Windows system libs) reads are NOT tracked — the daemon only guarantees
> invariance. For mounted trees, reads ARE tracked."

Ratified as a normative invariant:

> `vix-spec.md:162-168`, V28: "Capability invariance is daemon-enforced: advertise ⇒ watch ⇒
> poison."

**ABSENT in code, confirmed two ways.** No `fingerprint`, no `FSEvents`/`inotify`, and no
capability type exist anywhere under `crates/` (repo-wide case-insensitive grep, zero hits).
`crates/fuse3/src/notify.rs` is a false friend — it's the FUSE *kernel*-notification channel
(`FUSE_NOTIFY_INVAL_ENTRY` etc., messages sent *to* the kernel driver), the opposite direction
from a daemon watching a toolchain tree for mutation. The sandbox-grant substrate exists
(`crates/seatbelt-exec`, `crates/landlock-exec` both have a `toolchain_root`/`.toolchain_root()`
builder that grants `process-exec`/`file-read*` on a declared toolchain root — including a test
literally walking Xcode's `XcodeDefault.xctoolchain`), but this is confinement of a single exec,
not the fingerprint-watch-poison contract for the *duration of a build*.

Confirming this is a real gap and not an oversight: `docs/dev/content/developing/holes.md:26-30`
states the daemon **does not currently exist** — three prior implementations were deliberately
removed, leaving "a visible gap where a daemon obviously belongs... so the next implementation
would be built from the intended architecture ... rather than pattern-matched from the last
wrong one." Element 2 is describing present-tense daemon behavior for a component that is
presently a deliberate hole.

Every "poison" hit actually found in `crates/` is unrelated: `std::sync::Mutex` poisoning
recovery (`vx-fskit/src/macos/support.rs:186-192`, used throughout the FSKit extension), an
FSKit mount-wedge metaphor (`vx-management/src/mount.rs:8,108`), and a vestigial
`Expectation::Poisons` test-harness variant meaning "expected to error" (`vx-services/src/
test_runner.rs`, from the deleted `.vx`-evaluator era). See the terminology section below —
"poison" is heavily overloaded across this corpus in senses unrelated to element 2.

---

## §3 — Command grammars (summarization / normalization / code-intelligence / validation)

**PRESENT, but CONTRADICTS the dictated shape.** The design record already has a settled,
different enumeration — five products, not four, decided before today:

> `vix-synthesis.md:671-717`, "Command grammars are snark grammars, decided 2026-07-03": five
> products — (1) **Roles / mount ceiling**, (2) **Identity via normalization** ("`gcc -c blah.c
> -O2` == `gcc -O2 -c blah.c` in tier-1... identity = canonicalized parse, everywhere"), (3)
> **Salience / description** (UI descriptions/icons per tool), (4) **Validation + conservative
> fallback** — "an argv item the grammar can't parse degrades to the WIDEST assumption...
> Unparseable is slow, never unsound," (5) **Highlighting**.
>
> Restated as normative invariant `vix-spec.md:41-53,162-168` (V29): same five products, same
> wording ("roles/ceiling, identity, salience, validation, highlighting").

Rough mapping to today's four dictated functions: roles ≈ code-intelligence, identity-via-
normalization ≈ normalization, salience ≈ summarization, validation ≈ validation — and
**highlighting has no home in the dictated four** (editor syntax-highlighting off the grammar,
an OTHER/possibly-dropped fifth product). The dictation's SUMMARIZATION framing ("200 args → the
semantic essence receipts/UIs show") is close to but not identical to "salience/description."
Worth deciding explicitly whether highlighting was dropped on purpose or just not re-mentioned
in this dictation.

Two further grammar responsibilities appear in the design record that aren't in either
enumeration:

> `vix-synthesis.md:102-106`: "**Grammar CI** — because invocations are typed and journaled,
> command-line compatibility is checkable WITHOUT running anything, in both directions" —
> a bidirectional compat-check function, not listed as one of the five or the four.
>
> `vix-synthesis.md:497-511`: command grammars carry "the per-tool remedy (inject
> `-fdebug-prefix-map`/`-ffile-prefix-map` style rewrites) for tools known to embed
> [cwd/paths]" — embedded-path normalization as a grammar responsibility.

**ABSENT in code, confirmed.** Zero hits for `grammar` anywhere under `crates/` case-
insensitively. No `CommandGrammar`/`Grammar` type exists. Per `crates/vx-services/src/
test_runner.rs:172`, a vestigial `Phase::CapabilityCheck` enum variant survives from a deleted
`.vx`-evaluator compiler pipeline — unconnected to anything (grep confirms it appears nowhere
else), a different sense of "capability" (language/compiler capability-tags, not toolchain
probes). Per `git log --all` (checked by the crate-sweep agent, corroborating the baseline
doc's own line 27 "recovery scout dispatched 2026-07-08"), a real grammar/capability-tag
implementation existed in `vx-ir`/`vx-runtime`/`vx-parser` and was **deliberately deleted**;
nothing in the current tree replaces it.

The dictation's specific claim — **"the vix command-role classifier is the embryo"** — implying
existing code assigns argument roles today — is **not corroborated**. Repo-wide grep for
`command.role|role.classif|ArgRole|arg_role|classifier` across `crates/` hits nothing relevant
(only an unrelated Cargo-dependency-fact classifier in `docs/design/rodin-fact-validity/`).

Also worth noting: `docs/design/taste-record.md:208-214` states "The command-grammar spec [is]
the one large design document still unwritten... Contents when written: annotation format for
roles/commutativity/salience, the conservative-fallback law (V29) stated operationally, and ONE
worked grammar (cc) end to end." Today's dictation, and/or the baseline doc itself, may be a
first pass at exactly that still-missing spec — worth flagging to Amos explicitly rather than
treating it as a restatement of settled fact.

---

## §4 — Capabilities-as-packages, probes parameterized by grammar, resolved via rodin

**PARTIALLY.** The capability-as-advertised-typed-value pattern is old news (§1/§2 quotes).
What's new in today's (2nd) dictation is specifically: grammar-as-a-vixen-package + probe-as-a-
separate-package-parameterized-by-the-grammar + **resolution via rodin like any other
dependency**. The design record already leans this direction without fully stating it:

> `taste-record.md:222-225`, "Who ships command grammars": "The capability does (synthesis
> already leans there; make it a ruling when convenient). A toolchain advertisement without its
> grammar is degraded per V29: usable, tier-1 only, loudly annotated."
>
> `artifact-intelligence.md:43-45,18`: "**PROBE AT ACQUIRE**: toolchains probed once ('this cc
> emits glibc floor X') — plan-time policy before the first run... Symmetry with C6: grammars
> structure tool INPUTS, probes structure tool OUTPUTS." — a clean, quotable inputs/outputs
> symmetry not spelled out in today's dictation.
>
> `vix-synthesis.md:480-489`: "Capabilities as DI: build code requests `Type where (predicate)`,
> executors probe + advertise providers, the control plane matches + injects; ROUTING FALLS OUT
> OF PROVENANCE." — a dependency-injection framing for capability *resolution*, distinct from
> (and not obviously reconciled with) "resolves via rodin like any dependency" in today's
> dictation. Both describe capability resolution, via different metaphors (DI container vs.
> package resolver) — worth deciding whether these are the same mechanism described twice or
> two different mechanisms that need reconciling.

But `vix-synthesis.md:791-793` itself still lists "who authors/ships/versions command grammars"
and "ambient-toolchain edge cases beyond the daemon answer" as **open** as of its own writing —
so today's dictation is making a ruling call on questions the design record had flagged as
unresolved, not just restating settled fact.

**CONTRADICTS — the one concrete "already shipped" claim in the baseline doc does not hold up.**
The baseline doc says: *"the vix-runner protocol types — capability advertisement shipped
recently"*. Checked against `crates/vx-services/src/runner.rs` (this is where "vx-runner"
actually lives — a module inside `vx-services`, not a standalone crate; there is no `vx-exec` or
`vx-runner` crate):

> `runner.rs:1-7`: "This crate defines the runner RPC surface only. During the evaluator purge,
> `vx-runner` is intentionally a minimal protocol shell. The request and result payloads here
> are placeholders, not a stable execution model."
>
> `runner.rs:25-33`: `DiscoverProbeRequest { /* TODO: a closure? */ }`, `DiscoverResult { /*
> TODO: design */ }` — literally empty stub structs with `// TODO` bodies, no fields.

Repo-wide grep for `Capability|CapabilitySet` inside `crates/vx-services` returns zero hits in
`runner.rs`. The only "Capability" occurrences in that crate are the unrelated `test_runner.rs`
phase-enum leftover noted in §3. So: **no capability advertisement type exists in vx-runner**,
contradicting the baseline doc's own "shipped recently" claim. This should be corrected or the
claim should point at wherever it actually landed (nowhere found in this sweep).

**Terminology note (see full list below):** "capability" is heavily overloaded across this
tree in senses unrelated to toolchains — worth reading before assuming a grep hit is on-topic.

---

## §5 — Two-tier cache key from the grammar

**PRESENT, pre-existing, and detailed** — this is the most thoroughly pre-worked element of the
six. The exec-seam description in the working record matches the dictation closely, down to a
named stress test:

> `vix-synthesis.md:555-575`, "THE EXEC SEAM": "Tier 1 = closure × plan × capability fingerprint
> × coarse mount fingerprints... Tier 2 = candidates indexed by plan × capability (capability
> INCLUDED: only the world may vary under read-set verification, never the computation)...
> Proven by probes: the ANTI-NIX test... the CROWN JEWEL (new header shadowing an old one
> earlier in the search path...)."
>
> Ratified: `vix-spec.md:90-91`, V9: "Identity is two-tier: fingerprint × read-set (the
> anti-Nix pillar; read-sets include directory enumerations and stat-misses)."
>
> `vx-store-as-vix-memo.md:154-162`: "For tier 1, the persistent key is the existing exact
> identity: closure or primitive identity, normalized plan, **capability fingerprint**, and
> coarse mounted tree identities" — "capability fingerprint" as a literal field name, matching
> the dictated Tier-1 definition closely.

Pre-dictation, this terminology was already live in two other corners of the tree: the cargo
sketch (`docs/design/sketches/cargo/cargo.vix:87-90`, "features are identity: they become
`--cfg` argv items, hence part of the normalized plan, hence part of BOTH cache tiers") and
`decisions.md` ("resolution gets tier-2-style reuse (its read-set = pins + deps_of keys
consulted)"), and the trace-UI's serving-class vocabulary
(`docs/design/briefs/lanes-view.md:27`: `"tier-1=--ok, tier-2=--teal"`) — **which has two more
classes, "ran" and "joined," not covered by a two-tier hit/miss model.** Worth checking whether
real cache outcomes need more than two categories.

**Caveat, not contradiction**: `prior-art-adjudication.md:23` explicitly cedes the two-tier
*mechanism itself* as non-novel: "C4 two-tier + negative lookups — **KILLED on mechanism** —
BuildXL: Weak fingerprint × observed pathset incl. absent probes + dir enumerations is BuildXL,
verbatim. Survives: the soundness posture ... and call granularity." Today's dictation presents
Tier 1/Tier 2 as the design, without the prior-art caveat that the split itself descends from
BuildXL/Vesta (lineage detail also in `L104-111`: "Vesta → BuildXL").

**ABSENT in code.** No grammar-derived two-tier key exists in `crates/`. The generic CAS/tree
store substrate (`crates/vx-services/src/store.rs`, `cas_tree.rs`) is real and Blake3-hashed,
but toolchain-agnostic — one doc-comment references a hypothetical `finalize_rust_toolchain`
(`store.rs:562`) that doesn't exist as a function anywhere in the tree.

**Status-page caveat, directly bears on implementability today**: per
`docs/dev/content/concepts/read-sets.md:40-50` and `docs/content/using/limitations.md:30-34`,
observed read-sets for *external processes* (i.e., toolchain invocations, which is what Tier 2
needs) are explicitly **staged/future**, not present — "Until observation lands, read-sets for
external processes are declared-and-enforced rather than observed." Also worth noting: the
*general* vix demand-memo key described in `docs/dev/content/concepts/demand.md:26-29` and
`content.md:15-17` is single-tier (function identity + full content-hash of args) — the
two-tier split is specific to command-grammar-driven external invocations, not vix's general
memoization model. Not a contradiction, but a scope clarification worth stating explicitly in
whatever doc formalizes this.

---

## §6 — Geography/jurisdiction correction

**ABSENT as a connected argument.** The one document that reads as the natural home for this —
`docs/design/eu-sovereignty-positioning.md` — was read in full by the sweep and has **zero**
hits for capability/grammar/fingerprint/probe/poison/ambient/toolchain/two-tier/advertis/
materializ. Its closest approach:

> `L36`: "**SecNumCloud / cloud de confiance** (FR procurement) | Qualified EU-jurisdiction
> hosting | Registry + cache operated by vixenware (EU corp) on EU infra."

This frames EU residency as a property of *registry/cache hosting*, not as a constraint on
*materializable-toolchain build placement/scheduling* — the connection the dictation makes
("an EU-sovereign build must not page toolchain blocks from a US store") is not made anywhere
in the current tree.

The closest structurally-parallel mechanism found is independent and doesn't mention toolchains:

> `docs/design/sketches/infra/runner-autoscaler.vix:14-15,77-85`: "Rodin-as-scheduler:
> placement is constraint solving over probed node facts" — `solve([Fact::observed(cluster_
> facts.nodes), ..., Fact::constraints(pool.placement), Fact::cost_cap(...)])` with
> geography-flavored labels (`"us"`, `hcloud-us-ubuntu-24.04`).

This is placement-constraint solving for CI-runner cloud nodes, not toolchain-bound build work
— but it's the same underlying idea (Rodin-as-scheduler over placement constraints) arrived at
independently. Worth checking whether it should unify with the toolchain-geography story rather
than staying a separate mechanism.

---

## Things in the tree the dictation didn't mention

**1. The capability/probe/fingerprint/advertise/poison pattern is already generalized well
beyond toolchains**, in multiple documents, to:
- Cloud infrastructure providers: `docs/design/infra-as-demand.md:29-31`: "**PROVIDERS =
  CAPABILITIES** (Aws::acquire fingerprints creds + API surface); **INVENTORY = PROBED HOSTS**
  (a machine is an artifact: arch/libc/kernel probed at acquire)."
- Package registries: `docs/design/taste-record.md:190-198`: "Registry layout probing. Capability
  business entirely: index URL templates, sparse layout, auth — config data inside the registry
  capability's closure, fingerprinted like everything else." Also `sketches/cargo/cargo.vix:96`:
  `Registry::acquire("crates-io")` with a doc comment "a capability: index rules, URL templates,
  fingerprint."
- Secrets/KMS: `docs/design/secrets-as-sealed-values.md:50`: "widening only via explicit
  **capability-gated** journaled DECLASSIFY."
- The Rodin solver kernel itself, wrapped as an opaque capability: `docs/design/
  agent-resurrection-note.md:667-676` ("kernel as capability"), `docs/design/rodin-in-vix.md:16-
  17` ("Kernel stays a Rust capability").
- Bare-metal hosts and infra-drift detection: `docs/design/infra-port-recon.md:176-245`
  ("provider capability," "capability fingerprint"), including a distinct capability *subtype*
  — "Bootstrap roots are capabilities that must exist before demand evaluation can fetch/decrypt
  its own graph" (L242-245) — with catastrophic-loss properties not addressed by any of the six
  dictated elements.

  If the capability system is meant to be domain-general (which this evidence strongly
  suggests it already is, in practice), the dictation's toolchain-first framing may be
  undercutting how far the pattern has already spread.

**2. Heavy terminology overload across the corpus** — same words, different (unrelated)
referents, worth disambiguating before this design doc ships more widely:
- **"capability"**: VM host-op dispatch table in `docs/design/redesign/1-host-registration.md:
  75-76` and `6-parsers-snark.md:420-429` ("Once V4's capability registry exists..."); privileged
  payload-less OS actions in `crates/vx-management` (`Button::{Mount,Unmount,...}` called
  "capabilities" throughout `main.rs`/`lib.rs`); FUSE wire-protocol feature flags
  (`crates/fuse3/src/raw/abi.rs:5`, `FUSE_CAP_*`); product/feature-access tiers
  (`docs/design/open-proprietary-division.md:183`, `docs/content/using/_index.md:23-25`,
  `docs/content/status/_index.md:40-44` — "capabilities land in tiers... nothing is tagged yet
  because wave one hasn't shipped").
- **"probe"**: format/ELF probes (`docs/dev/content/vix/builtins.md:21-26`); infra reconciliation
  probes (`docs/design/sketches/infra/*.vix` — `Probe::oci/dns/host/cluster/hcloud/github`); a
  VFS mount smoke-test (`crates/vx-management/src/probe.rs`, fully implemented, unrelated);
  lazy-parse coercion (`docs/design/secrets-as-sealed-values.md:7-9`, `sops()` as "a PROBE");
  host-tool test-fixture detection (`docs/dev/content/developing/fixtures.md:34-36`).
- **"poison"**: at least five distinct senses found — Rust `Mutex` poisoning-on-panic
  (`crates/vx-fskit`), FSKit mount-wedge (`crates/vx-management/src/mount.rs`), supply-chain/
  malicious-artifact poisoning (`docs/design/open-proprietary-division.md:63`,
  `store-architecture.md:45-49`), solver-fact-namespace poisoning
  (`docs/design/warm-facts-spec.md:255-259`), resolver-dominance-order poisoning
  (`docs/design/march-solver-recovered.md:260-269`), and a test-harness `#[test(poisons)]`
  meaning "expected to error" (`crates/vx-services/src/test_runner.rs`). None of these is the
  daemon-fingerprint-mutation-poisons-in-flight-build sense from element 2.
- **"watch"**: CDCL SAT-solver watch-lists (`docs/design/rodin-in-vix.md:65-70`) vs. FS-watch —
  a clear false-friend homonym in a directly-adjacent design doc.
- **"ambient"**: daemon-provided always-on services (`docs/dev/content/concepts/services.md:10-
  15`, "ambient capabilities as services: mounting... launching... watching") vs. ambient
  *toolchains* (element 1) vs. "vix makes those properties ambient" (`docs/dev/content/vix/
  _index.md:12`, meaning "pervasive by default") — three referents for one adjective.
- **"grammar"**: snark's tree-sitter-style *language*-parsing grammars
  (`docs/dev/content/constellation/snark.md:6-16`) vs. command grammars — same word, load-
  bearing in both, worth an explicit disambiguation or distinct term. Also: grammars generalize
  to templates (`docs/design/secrets-as-sealed-values.md:40-47`: "a template IS a grammar...
  rendering IS evaluation with holes as demand edges"), beyond command invocations.
- **"normalize"**: path/string canonicalization in `crates/vx-vfs/src/windows.rs`,
  `crates/vx-management/src/extension.rs`, `crates/vx-fskit/src/macos/logging.rs` — unrelated to
  command-line normalization (element 3).

**3. Capabilities-as-DI framing** (`vix-synthesis.md:480-489`) — a specific resolution
mechanism ("build code requests `Type where (predicate)`, executors probe + advertise
providers, the control plane matches + injects") that reads as a different metaphor from
"resolves via rodin like any dependency" and may need reconciling with it.

**4. Open/proprietary split on capabilities**: the capability type system itself is open, but
org-level capability *ACL management* is a paid/proprietary component
(`docs/design/open-proprietary-division.md:53`). The dictation doesn't address this boundary.

**5. "The command-grammar spec [is] the one large design document still unwritten"**
(`docs/design/taste-record.md:208-214`, dated before today) — today's dictation, or the baseline
doc it produced, may effectively be a first pass at that missing spec. Worth stating that
explicitly rather than treating either as settled restatement.

**6. A whole prior implementation was deleted.** Per `git log --all` (corroborating the
baseline doc's own "recovery scout dispatched 2026-07-08" line): commits like `db7c54bc "zoo/
c.vx: the cc capability builds a real Command (grammar value + binary + fingerprint +
exemptions)"`, `c6fecafd "wiki(exec): capability IS its command(s); @[ {cmd} args ] syntax"`,
and `f1787a0e "ir: remove the capability-tag subsystem (#fs/#sandbox/#discover/#host)"` show a
real grammar+capability-tag system existed and was deliberately removed along with the entire
`vx-ir`/`vx-runtime`/`vx-parser` crates. Nothing in the current tree replaces it — this sweep
covers only the live tree, so full recovery of what that system looked like is the
history-archaeology agent's job, not this one's.

**7. Placement-constraint solving for CI/cloud runners already exists independently**
(`docs/design/sketches/infra/runner-autoscaler.vix`, "Rodin-as-scheduler... placement is
constraint solving over probed node facts," with `"us"`/region labels) — structurally the same
shape as element 6's geography argument, but for cloud nodes rather than toolchain-bound build
work, and not connected to it anywhere in the tree.

**8. Trace-UI already has more than two cache-outcome categories**
(`docs/design/briefs/lanes-view.md:27`: serving classes `ran`, `tier-1`, `tier-2`, `joined`) —
worth checking whether the two-tier *key* design needs to account for outcomes beyond simple
tier-1-hit/tier-2-hit/tier-2-miss.

---

## Files read with no material hits (checked, ruled out)

`docs/design/briefs/engine-chunk-1.md`, `engine-chunk-2.md`, `rebase-onto-main.md`,
`docs/design/redesign/0,2,3,4,5,7,8,9-*.md` + `_index.md`/`synthesis.md` (beyond the flagged
capability-registry excerpts), `docs/design/rodin-fact-validity/*` (adjudication.md, claude.html,
gemini.rtf, gpt.txt), `docs/design/rodin-block-forensics.md`, `docs/design/rodin-c-evidence.md`
(only coincidental `openssl-probe` crate-name hits), `docs/design/eu-sovereignty-positioning.md`
(see §6).
