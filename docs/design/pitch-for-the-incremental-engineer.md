# The pitch, derived from the query system's failures
*(for the person who knows them best — no cosmology, every claim receipted)*

You already believe five things, because you discovered them at work:

1. **Rolling small IDs kill persistence.** DefIndex churn means nothing survives a process;
   no daemon, no sharing, no cross-machine anything.
2. **Retrofitted incrementality is invasive.** The query system lives INSIDE the thing it
   incrementalizes — 100k lines of interning, red-green, and unsafety threaded through a
   compiler that wasn't shaped for it.
3. **It's unsafe not because Rust failed** but because bolted-on incrementality costs that
   in any language.
4. **Caching layers above unstable identity miss.** sccache and its "better" descendants
   key on mushy inputs; they fight your war with worse weapons.
5. **The artifact half of RDR needs a decision half** or the patches never pay off.

Now take (1) seriously as a PRIMITIVE instead of a bug: identity must be stable,
content-derived, structural. That single commitment forces the rest:

- Structural identity for code and values → taxon (blake3 over canonical structure;
  SchemaId; AST item identity via snark). *Receipt: the identity epoch closed 2026-07-08;
  hashes are durable cross-machine cache keys.*
- Incrementality as an EXTERNAL substrate, not a retrofit → the demand machine: memoize
  everything, observe reads instead of trusting declarations, laziness as execution.
  Daemon-able BECAUSE identity doesn't roll. *Receipt: warm solves, zero-skew lock
  reproduction (145/145 members; closure rings clean), 4x perf in one evening of
  flame-guided levers.*
- Build logic as data + pure functions on that substrate, not baked into a binary → the
  vix language. It is ALREADY the downscoped artifact: five days old, denotation-only,
  because the machine supplies everything hard. Downscoping further = re-baking logic
  into Rust = recreating "bolted-on" one layer up. *Receipt: a CDCL resolver, cargo's
  manifest/feature/cfg/profile model, and unit derivation — all in it, all
  oracle-verified against cargo itself.*
- Processes as graph nodes with observed read-sets and capability-receipted toolchains →
  the exec seam. *Receipt: 2026-07-08, vixen built taxon end-to-end — real rustc, a real
  build.rs EXECUTING, its C compiler a declared capability with an identity receipt
  (Apple clang 21.0.0, hash a42725b2…), libtaxon.rlib hashed. Zero hand-authored graph.*

The alliance, concretely: your rustc-side RDR work is the artifact half. The decision
half — "did this change semantically reach that consumer" — shipped here this week
(item identity + rmeta-granular subfile dependencies). Each half is only valuable with
the other. Vixen is not a competitor to your summer; it is its customer.

Demos, in your pain order:
1. Your ForgeJo cargo-fork pipeline as STANDING DEMANDS — build the fork, run tests,
   publish results, notify on breakage/perf-miss — declarative, receipted, nothing to
   fight because there is no script, only demands. (The weeks you just lost are the spec.)
2. The daemon demo: resolver/build state warm across processes and days — the exact thing
   rolling IDs forbid, running.
3. RDR end-to-end: your patches + our decision half, relink instead of rebuild, measured.

The "better sccache" projects are not threats and neither are we to them — different
game: they accelerate an untrusted pipeline; this replaces trust-by-timestamp with
identity-by-construction. You are the person best equipped on earth to audit whether
that claim holds. That's the ask: audit it, don't believe it.
