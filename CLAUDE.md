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

## Retired reference / oracle-era scaffolding — do NOT recreate as production

The Rust resolver scaffolding that predated the Vix implementation has been
pruned from this repository: `crates/vix-solve`, `crates/rodin-core`,
`crates/rodin-facts`, and `crates/rodin-survey`. Those crates were differential
stepping stones, not a second production topology.

Do **not** restore, optimize, profile, benchmark, or add production features to
that retired Rust resolver path. New resolver work belongs in Vix, and Cargo
remains the behavioral oracle.

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
