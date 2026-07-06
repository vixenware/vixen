//! The rodin resolver fixture corpus: one isolated cargo-vs-rodin differential
//! per resolver behavior. Every fixture is a tiny offline path-dep workspace, so
//! each runs in milliseconds and pinpoints exactly one failure mode.

use eyre::Result;
use rodin_fixtures::{DepKind, Fixture, FixtureCrate, FixtureDep};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const WINDOWS: &str = "x86_64-pc-windows-msvc";

/// The jiff-static shape: an optional dep referenced only by a non-default
/// feature (`static-tz -> dep:helper`) and a weak feature
/// (`tz-fat -> helper?/tz-fat`), plus a mandatory always-false `cfg(any())`
/// edge (the version-locking hack). None of these may pull `helper` in — on any
/// target. This is the over-activation bug the corpus was built to catch.
#[test]
fn cfg_any_and_weak_feature_never_pull_optional_dep() -> Result<()> {
    let fixture = Fixture::new("cfg-any-never-dep", "app")
        .krate(FixtureCrate::new("helper").feature("tz-fat", &[]))
        .krate(
            FixtureCrate::new("lib")
                .feature("default", &["tz-fat"])
                .feature("tz-fat", &["helper?/tz-fat"])
                .feature("static-tz", &["dep:helper"])
                .dep(FixtureDep::new("helper").optional())
                .dep(FixtureDep::new("helper").target("cfg(any())")),
        )
        .krate(FixtureCrate::new("app").dep(FixtureDep::new("lib")));

    for target in [LINUX, WINDOWS] {
        let selected = fixture.assert_selection_matches(target)?;
        assert!(
            !selected.contains("helper"),
            "helper must never be selected on {target}: {selected:?}"
        );
    }
    Ok(())
}

/// A feature that is on by default (`bundle-platform`) references an optional dep
/// that is declared only for a target (`cfg(windows)`). cargo activates it only
/// on windows; rodin's gate must match.
#[test]
fn feature_activated_target_conditional_optional_dep() -> Result<()> {
    let fixture = Fixture::new("feature-target-optional", "app")
        .krate(FixtureCrate::new("platform"))
        .krate(
            FixtureCrate::new("lib")
                .feature("default", &["bundle-platform"])
                .feature("bundle-platform", &["dep:platform"])
                .dep(
                    FixtureDep::new("platform")
                        .optional()
                        .target("cfg(windows)"),
                ),
        )
        .krate(FixtureCrate::new("app").dep(FixtureDep::new("lib")));

    let windows = fixture.assert_selection_matches(WINDOWS)?;
    assert!(windows.contains("platform"), "platform on windows: {windows:?}");
    let linux = fixture.assert_selection_matches(LINUX)?;
    assert!(!linux.contains("platform"), "no platform on linux: {linux:?}");
    Ok(())
}

/// A plain `cfg(windows)` dependency edge (winapi shape): present on windows,
/// absent on linux.
#[test]
fn direct_target_conditional_edge() -> Result<()> {
    let fixture = Fixture::new("direct-cfg-windows", "app")
        .krate(FixtureCrate::new("winthing"))
        .krate(
            FixtureCrate::new("app")
                .dep(FixtureDep::new("winthing").target("cfg(windows)")),
        );

    assert!(fixture.assert_selection_matches(WINDOWS)?.contains("winthing"));
    assert!(!fixture.assert_selection_matches(LINUX)?.contains("winthing"));
    Ok(())
}

/// A build-dependency is part of the host graph and consumed on every target.
#[test]
fn build_dependency_is_consumed() -> Result<()> {
    let fixture = Fixture::new("build-dep", "app")
        .krate(FixtureCrate::new("gen"))
        .krate(
            FixtureCrate::new("app")
                .dep(FixtureDep::new("gen").kind(DepKind::Build)),
        );

    assert!(fixture.assert_selection_matches(LINUX)?.contains("gen"));
    Ok(())
}

/// A dev-dependency of a non-root crate is not consumed by a normal build of the
/// root (cargo excludes it; the gate must too).
#[test]
fn transitive_dev_dependency_is_not_consumed() -> Result<()> {
    let fixture = Fixture::new("dev-dep", "app")
        .krate(FixtureCrate::new("testonly"))
        .krate(FixtureCrate::new("lib").dep(FixtureDep::new("testonly").kind(DepKind::Dev)))
        .krate(FixtureCrate::new("app").dep(FixtureDep::new("lib")));

    let selected = fixture.assert_selection_matches(LINUX)?;
    assert!(!selected.contains("testonly"), "dev dep of lib not built: {selected:?}");
    Ok(())
}
