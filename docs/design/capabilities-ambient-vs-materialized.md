# Capabilities: the ambient residue (Amos, 2026-07-08)

The load-bearing distinction (supersedes the "capabilities are dependencies" sketch's
emphasis, which survives only as the runner-matching layer):

- **Materializable toolchains (rustc-class)**: download/extract into the local CAS, mount
  via VFS prefix — or mount from a REMOTE store without downloading at all. Identity is
  a-priori (we hash what we mount); immutability by construction. These are NOT deeply
  capabilities — they are ordinary content-addressed inputs, and the set grows over time.
- **Ambient toolchains (Xcode/MSVC-class)**: technically and LEGALLY impossible to
  materialize (redistribution forbidden; entangled with the OS). These are the real
  capabilities. Contract: the DAEMON identifies, fingerprints, and FS-WATCHES
  (FSEvents/inotify) the entire toolchain tree, and POISONS ANY IN-FLIGHT BUILD if the
  toolchain is modified in place. Identity is a-posteriori and continuously re-verified
  for the duration of every build that uses it.

Consequences:
- Two trust postures: mount-what-you-hashed vs fingerprint-what-you-must-trust-and-watch.
  The poison-on-mutation contract is what keeps receipts honest over toolchains we can
  only observe.
- The capability system is a deliberately SHRINKING boundary: things migrate out of it
  and into the store as materialization becomes possible. rustc already crossed; host cc
  (= Xcode clang on macOS) has not and may never.
- The demo's C-toolchain handling should follow the fingerprint discipline now and gain
  daemon-watch when composed with the runner (the capability advertisement carries the
  fingerprint; the watch is the daemon's ambient service).
- Prior design work on this exists in git history (deleted-daemon era) — recovery scout
  dispatched 2026-07-08 (documents/design only, never code resurrection).

## The ecosystem layer (Amos, 2026-07-08, second dictation)

**All command invocations are typed via a grammar**, whose functions include (list open —
"et..."): SUMMARIZATION (200 args → the semantic essence receipts/UIs show), NORMALIZATION
(canonical form: order-insensitivity, path normalization, flag folding → semantically
identical invocations get identical IDENTITY — cache hits survive argv noise; the grammar
version participates in identity provenance), CODE INTELLIGENCE (role assignment:
input/output/codegen-flag/cosmetic — the vix command-role classifier is the embryo; also
yields EXPECTED read-sets that observation confirms, divergence = finding), VALIDATION
(unknown flags are loud — the 3% policy at flag granularity).

**Capabilities are defined by PACKAGES**: the command grammar is one vixen package; the
capability PROBE is another, PARAMETERIZED BY the grammar (one probe engine per toolchain
family: locate, interrogate, fingerprint). Runner daemon config ENABLES a capability mix,
defaulted by host OS. Consequences: toolchain support = versioned, content-addressed,
registry-distributed, overlay-patchable content (enterprises ship internal-compiler
packages; CUDA grammar shippable by anyone); probes run under the machine so probing is
observed/receipted; capability packages resolve via rodin like any dependency — one
resolver, everywhere; the build system's knowledge of the world is itself built and
audited by the build system.

## Corrections + the two-tier key (Amos, 2026-07-08, third dictation)

CORRECTION to the lazy-remote-mount consequence: materializable toolchains make scheduling
CHEAPER, not free — fiber does not erase geography or geopolitics. Latency, bandwidth,
partitions, and JURISDICTION persist; data residency is a placement constraint, and the
sovereignty product REQUIRES geographic constraints to be enforceable (an EU-sovereign
build must not page toolchain blocks from a US store). Ambient capabilities pin work to
MACHINES; materializable toolchains still bind work to REGIONS/NETWORKS.

**Command grammars compute the TWO-TIER CACHE KEY** — the grammar knows which arguments
are inputs, which makes the split computable:
- Tier 1 (computation identity): normalized command + tool identity (capability
  fingerprint / store hash) + input NAMES — computable before reading any input byte.
  Cheap stable lookup: "which computation is this."
- Tier 2 (execution identity): tier 1 + input CONTENT hashes, closed over by the observed
  read-set for inputs not present in argv (build-script dynamic reads). Declared shape
  from the grammar; observed content from the VFS.
Properties: lookup precedes input I/O; normalization prevents path/flag-order cache
shatter; tier-1-hit + tier-2-miss IS the incremental question, asked by the key itself.
