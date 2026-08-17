//! `cargo-vix/cargo.vix` runs here, so it cannot rot.
//!
//! The file is 28 checks of real work — manifest and lock decode, the Kahn walk
//! over `PackageId`, target discovery, the rustc argv plan — and until now the
//! only way to run any of it was `vx cargo-vix/cargo.vix` by hand. Nothing in
//! the suite touched it, so a change to decode, to the surface stdlib, or to the
//! origin seam could break every one of them and come back green.
//!
//! The pattern is `exec_tree_mounts.rs`' `hello.vix` test: the SHIPPED file is
//! the source under test, read from disk rather than copied here, so the example
//! and the proof are one artifact.
//!
//! The one thing this has to get right is the fixture root. `vx` roots the
//! harness [`FixtureStore`] at the `.vix` file's own directory — which is why
//! the crates under test live in `cargo-vix/trees/` — so a Rust-side runner has
//! to root it the same way or `fixture_tree("small-crate")` resolves against
//! `vixen-runtime/tests/fixtures/` and finds nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vix::runtime::PrimitiveServices;
use vixen_runtime::fixture::FixtureStore;
use vixen_runtime::ratchet::prepare_source;

/// `<workspace>/cargo-vix` — the directory `vx cargo-vix/cargo.vix` would pick
/// as its fixture root, derived from this crate's manifest rather than from the
/// process's working directory, which nextest does not promise.
fn cargo_vix_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("vixen-runtime sits in the workspace root")
        .join("cargo-vix")
}

/// No exec backend, deliberately: `cargo.vix` reads and PLANS, and installing
/// one would let a future edit start spawning processes without anyone noticing
/// the file had changed kind. When the walk really runs rustc, that is a
/// different test with a stated capability offer, as `hello.vix` already has.
fn cargo_vix_services() -> PrimitiveServices {
    PrimitiveServices::default()
        .with_origin(
            FixtureStore::origin_decl(),
            Arc::new(FixtureStore::with_root(cargo_vix_root())),
        )
        .expect("one origin adapter cannot overlap itself")
}

#[test]
fn the_shipped_cargo_vix_is_green() {
    let source = include_str!("../../cargo-vix/cargo.vix");
    let report = prepare_source(source)
        .and_then(|run| run.execute_with_primitive_services(cargo_vix_services()))
        .unwrap_or_else(|error| panic!("cargo.vix: {error:#?}"));

    let failed: Vec<_> = report
        .plain
        .checks
        .iter()
        .filter(|check| !check.passed)
        .collect();
    assert!(failed.is_empty(), "cargo.vix: {failed:#?}");
    assert!(
        report.plain.refusals.is_empty(),
        "cargo.vix: {:#?}",
        report.plain.refusals
    );
    assert!(report.passed(), "cargo.vix: {:#?}", report.plain.checks);

    // The count is the point of the assertion: `passed()` is vacuously true for
    // a file whose checks all stopped being reached, and a stream that yields
    // nothing is exactly what a broken decode looks like from out here.
    assert_eq!(
        report.plain.checks.len(),
        28,
        "every check in cargo.vix ran"
    );
}
