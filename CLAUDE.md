# Rodin: what is production, what is a reference, what is the oracle

Read this before touching anything named `rodin-*` or `vix-solve`. Several
agents have independently mistaken the Rust reference resolvers for the product
and sunk hours extending, benchmarking, and optimizing them. Do not.

## The one production target

**Rodin's only production implementation is the vix-language one** — the CDCL /
resolution kernel written in vix, running on the vix demand machine (memoized,
demand-driven, incremental, content-addressed for free). That is the thing we
ship and the thing worth making correct and fast.

## The oracle is cargo

**cargo is THE oracle — the ground truth for resolution correctness. Period.**
Not rodin-core. When you need to know "what is the right answer," you ask cargo
(`cargo tree --target <triple>`, `cargo generate-lockfile`), not a Rust resolver.

## Reference / oracle-era scaffolding — do NOT treat as production

These are Rust scaffolding that predate the vix implementation. Experiments ran
here because the vix version was not ready yet; everything learned here was
always meant to be **ported to vix**, and these crates **retired fastest-first**:

- `crates/vix-solve` — redundant PubGrub resolver. Slated for deletion.
- `crates/rodin-core` — CSP-style reference resolver. Port semantics from, then retire.
- `crates/rodin-facts`, `crates/rodin-survey` — support for the above.

For these crates: do **not** optimize, profile, benchmark, or micro-tune them,
and do **not** add production features to them "because it's faster in Rust."
Their runtime does not matter. Their job is to be a differential reference while
the vix implementation catches up to cargo, then to disappear.

What DOES transfer is **knowledge** — how cargo actually resolves (rustc-cfg
predicates per target, feature activation pulls packages in only via edges not
feature-enables, `cfg(any())` never-deps, weak `dep?/feat`, build/normal/dev
feature non-unification, workspace unification). Transfer that to vix.

## Doing new resolver work

New resolution capability goes in the **vix** implementation, validated directly
against the **cargo** oracle (e.g. the `rodin-fixtures` tiny-offline-workspace
differential corpus, pointed at vix). If the vix implementation cannot yet host
it, that is a signal to advance the vix implementation — not to build it in Rust.

The full working-state record is `docs/design/agent-resurrection-note.md` and
`docs/design/rodin-in-vix.md`.
