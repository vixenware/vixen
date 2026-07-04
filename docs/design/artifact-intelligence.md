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
