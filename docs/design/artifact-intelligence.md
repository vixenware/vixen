# Artifact intelligence: vixen KNOWS file formats (Amos, 2026-07-04)

Outputs are not opaque bytes — they are lazily-parseable structured
values. OCI image → explore layers/files. Executable → ELF/Mach-O/PE:
arch, dynamic deps, linker metadata strings, sections+sizes, symbols
(DIFFABLE), required glibc version. Archives, anything: probing an
output into structure (LAZILY) so it can be an input to another step.

Architecture fit (why this is cheap for us):
- Same move as toml()/json(): format probes return lazy structural
  values; projections parse exactly the question asked (pending-value
  machinery; demand law unchanged).
- Probes are pure fns of content hash → memoized fleet-wide forever;
  symbol diffs are store ops over canonically-ordered exports.
- Policy is vix code: "image ships glibc 2.24, exec needs 2.35" =
  unbakeable at build time, no execution needed. Claims machinery
  applies (probe results are claims-with-evidence).
- Symmetry with C6: grammars structure tool INPUTS, probes structure
  tool OUTPUTS.
Slice order: elf() first (fixtures independent of real-process lane),
then archive formats, then oci() when image production exists.

## Yes-and round (Amos: "bank it all", 2026-07-04)

1. OCI ≅ MERGED TREES — recognition, not integration: layers are a
   merge with shadowing, whiteouts are absence, file→layer
   attribution IS the fallback contract. Baking = collect of pending
   layer trees. CONSEQUENCE: OCI moves EARLIER in slice order (tree
   machinery done) — slice 2 after elf().
2. DIFFS ARE BUILD PRODUCTS → MEASURED SEMVER: probe-memoized
   artifacts make historical diffs free (rediff over store values —
   "another bet paying off"); exported-ABI comparison between
   versions = observed-equivalence claims feeding Rodin (R2's move
   applied to binaries): resolve against probed ABI facts, not
   declared semver.
3. PROBE WHERE THE FLESH LIVES: ship the probe query executor-side
   (vix-wire observer closures), get back small facts. Fleet query:
   "every artifact linking the poisoned soname" — zero bytes moved.
   The xz incident-response screenshot.
4. JOURNAL × PROBE CORROBORATION: what claims to have produced it
   (journal) vs what it admits (.comment/ident/embedded paths).
   Reproducibility linting (leaked /home paths) as ordinary policy.
5. PROBE AT ACQUIRE: toolchains probed once ("this cc emits glibc
   floor X") — plan-time policy before the first run. Hosts are
   artifacts too (arch/libc/kernel probed at acquire) — see infra doc.
THROUGH-LINE: probes turn the store from a warehouse into a
database. Demo ladder: glibc assert → release diff report → fleet
soname query.
