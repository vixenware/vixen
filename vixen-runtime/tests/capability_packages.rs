//! Capability packages are data (`vixen.capability.package-is-data`).
//!
//! The claim under test is narrow and load-bearing: a proprietary toolchain
//! this workspace has never heard of — the xcode/msvc/quartus case the whole
//! capability half of the design exists to serve — becomes nameable, bindable,
//! and runnable by writing a file. Not by editing `vixen-primitives` and
//! recompiling vix, which is what it used to take.
//!
//! Every test here declares its packages through `VIX_CAPABILITY_PACKAGES`,
//! the invoker-facing seam, rather than calling the registration API directly.
//! The API is reachable, but a design whose only entry is a Rust call has not
//! actually solved the problem.
//!
//! That seam is one process-wide variable, so unlike the manifest's environment
//! tests (issue #14) these hold [`declaration_lock`] for their whole body
//! rather than relying on nextest's process-per-test. `nix flake check` runs
//! nextest either way; the lock is what makes a plain `cargo test`, where the
//! whole file shares one process, mean the same thing rather than measure
//! which test set the variable last.

#![cfg(unix)]

use vixen_primitives::capability_package::Target;
use vixen_runtime::manifest::{
    CapabilityOffer, MachineManifest, PACKAGES_ENV, PackagesLoadError, host_target,
};
use vixen_runtime::ratchet::{RunError, run_source_with_manifest};

/// A tool nothing in this workspace has heard of, with a target grammar
/// nothing in this workspace has heard of either: `-for <triple>`.
const FOREIGN_PACKAGES: &str = r#"
[[package]]
name = "Quartus"
protocol = "exit-only"

[[package]]
name = "Xcodebuild"
protocol = "exit-only"
target = { kind = "argv-flag", flag = "-for", words = { phone = "aarch64-apple-ios" } }
"#;

fn packages_file(dir: &tempfile::TempDir, source: &str) -> String {
    let path = dir.path().join("packages.toml");
    std::fs::write(&path, source).expect("write packages file");
    path.to_str().expect("packages path is UTF-8").to_owned()
}

fn fake_tool(dir: &tempfile::TempDir, name: &str, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write fake tool");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("mark fake tool executable");
    path.to_str().expect("tool path is UTF-8").to_owned()
}

fn manifest(capabilities: Vec<CapabilityOffer>) -> MachineManifest {
    MachineManifest {
        host: host_target(),
        capabilities,
    }
}

fn offer(ty: &str, program: &str, targets: &[&str]) -> CapabilityOffer {
    CapabilityOffer {
        ty: ty.to_owned(),
        program: program.to_owned(),
        toolchain: None,
        targets: targets.iter().copied().map(Target::new).collect(),
    }
}

/// Held for the whole of any test that reads or writes the declaration
/// variable — one declaration at a time, whichever runner is driving.
static DECLARATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A test that panics while holding the lock has said everything it has to
/// say; poisoning the rest of the file on top of that reports one failure as
/// several.
fn declaration_lock() -> std::sync::MutexGuard<'static, ()> {
    DECLARATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Declare the packages for the rest of this test process.
///
/// # Safety
///
/// The caller holds [`declaration_lock`], so no other test is reading or
/// writing the variable, and this is called before any runtime thread exists —
/// the same discipline the manifest's environment tests use.
fn declare(path: &str) {
    unsafe { std::env::set_var(PACKAGES_ENV, path) };
}

/// Declare nothing, explicitly. Same safety argument as [`declare`].
///
/// A test whose claim is "nobody declared this" has to say so rather than
/// assume it: the variable outlives whichever sibling set it, including that
/// sibling's since-deleted tempdir path.
fn undeclare() {
    unsafe { std::env::remove_var(PACKAGES_ENV) };
}

/// The whole point, end to end: `Quartus` is a name no Rust in this workspace
/// spells. A file declares its package, and from that moment a program may
/// take it as a parameter, a manifest may offer it, and it runs — with no
/// recompile of vix anywhere in the story.
///
/// r[verify vixen.capability.package-is-data]
#[test]
fn a_tool_this_workspace_never_heard_of_runs_because_a_file_declared_it() {
    const PROGRAM: &str = r#"
#[test]
fn synthesize(q: Quartus) -> Stream<Check> {
    let out = exec q`compile`;
    yield expect_eq((out.tree / "bitstream.txt").text(), "fitted");
}
"#;
    let _declaration = declaration_lock();
    let dir = tempfile::tempdir().expect("temp dir");
    declare(&packages_file(&dir, FOREIGN_PACKAGES));
    let quartus = fake_tool(&dir, "quartus_sh", "printf fitted > bitstream.txt");
    let report = run_source_with_manifest(PROGRAM, manifest(vec![offer("Quartus", &quartus, &[])]))
        .expect("a declared package compiles and runs");
    assert!(
        report.passed(),
        "the declared tool ran and produced its tree: {report:#?}"
    );
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(lane.counters.effect_spawns, 1);
    }
}

/// The counterfactual that keeps the test above honest: a tool no file
/// declared is not a name at all. If this ever starts passing by accident — a
/// package quietly shipped, the registry pre-seeded — the acceptance test above
/// would still pass while proving nothing.
///
/// The undeclared tool is `Nonesuch` rather than `Quartus` on purpose. The
/// package registry is process-global and only grows, so under plain `cargo
/// test` a sibling that declared `Quartus` would have made it nameable for the
/// rest of the process and this test would be measuring test order. The name
/// under test has to be one nothing in this file ever declares.
///
/// r[verify vixen.capability.package-is-data]
#[test]
fn an_undeclared_tool_is_not_nameable() {
    const PROGRAM: &str = r#"
#[test]
fn synthesize(q: Nonesuch) -> Stream<Check> {
    yield expect_eq(1, 1);
}
"#;
    let _declaration = declaration_lock();
    undeclare();
    let result = vixen_runtime::ratchet::run_source(PROGRAM);
    let Err(RunError::Diagnostics(diagnostics)) = result else {
        panic!("an undeclared capability type does not resolve, got {result:?}");
    };
    assert!(
        format!("{diagnostics:?}").contains("Nonesuch"),
        "the diagnostic names the unresolved type: {diagnostics:?}"
    );
}

/// A declared package's target grammar is real, not decoration: the foreign
/// `-for phone` spelling extracts a requirement through the declared word
/// table, and a machine that does not offer that target refuses pre-effect —
/// the same binding check the shipped packages get.
///
/// r[verify vixen.capability.package-is-data]
/// r[verify vixen.machine.requirements-from-use]
#[test]
fn a_declared_packages_target_grammar_binds_like_a_shipped_ones() {
    const PROGRAM: &str = r#"
#[test]
fn build(xc: Xcodebuild) -> Stream<Check> {
    let out = exec xc`-for phone archive`;
    yield expect_eq((out.tree / "app.txt").text(), "built");
}
"#;
    let _declaration = declaration_lock();
    let dir = tempfile::tempdir().expect("temp dir");
    declare(&packages_file(&dir, FOREIGN_PACKAGES));
    let xcodebuild = fake_tool(&dir, "xcodebuild", "printf built > app.txt");
    let linux_only = manifest(vec![offer(
        "Xcodebuild",
        &xcodebuild,
        &["x86_64-unknown-linux-gnu"],
    )]);
    let report = run_source_with_manifest(PROGRAM, linux_only)
        .expect("a binding refusal is a report verdict, never a runner error");
    assert!(!report.passed(), "a refused program cannot pass");
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(
            lane.counters.effect_spawns, 0,
            "no process spawned before the refusal"
        );
        assert_eq!(lane.refusals.len(), 1, "one refusal, typed: {lane:#?}");
    }
    let refusal = &report.plain.refusals[0];
    assert_eq!(refusal.required_type, "Xcodebuild");
    assert_eq!(
        refusal.required_target(),
        Some("aarch64-apple-ios"),
        "the declared word table normalized `phone` into Target vocabulary"
    );
}

/// The same program against a machine whose word covers the foreign target
/// runs. The refusal above is a real check, not a package that can only ever
/// say no.
///
/// r[verify vixen.capability.package-is-data]
#[test]
fn the_declared_target_requirement_is_satisfiable() {
    const PROGRAM: &str = r#"
#[test]
fn build(xc: Xcodebuild) -> Stream<Check> {
    let out = exec xc`-for phone archive`;
    yield expect_eq((out.tree / "app.txt").text(), "built");
}
"#;
    let _declaration = declaration_lock();
    let dir = tempfile::tempdir().expect("temp dir");
    declare(&packages_file(&dir, FOREIGN_PACKAGES));
    let xcodebuild = fake_tool(&dir, "xcodebuild", "printf built > app.txt");
    let mac = manifest(vec![offer(
        "Xcodebuild",
        &xcodebuild,
        &["aarch64-apple-ios"],
    )]);
    let report = run_source_with_manifest(PROGRAM, mac).expect("the covered case runs");
    assert!(report.passed(), "{report:#?}");
}

/// A declared file that is missing is a loud typed error at the entrypoint —
/// never a silent fall-back to "only the shipped packages", which would
/// resurface as the program naming an unknown type and read as the program's
/// fault rather than the invoker's.
///
/// r[verify vixen.capability.package-is-data]
#[test]
fn a_missing_declared_packages_file_is_a_loud_typed_error() {
    let _declaration = declaration_lock();
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir
        .path()
        .join("nowhere.toml")
        .to_str()
        .expect("path is UTF-8")
        .to_owned();
    declare(&missing);
    let result = run_source_with_manifest(
        "#[test]\nfn t(s: Sh) -> Stream<Check> { yield expect_eq(1, 1); }\n",
        manifest(vec![offer("Sh", "sh", &[])]),
    );
    let Err(RunError::Packages(PackagesLoadError::Unreadable { path, .. })) = result else {
        panic!("a missing declared packages file errs typed, got {result:?}");
    };
    assert_eq!(path, missing, "the error names the declared path");
}

/// A declared file that is not a package document is equally loud, and the
/// message carries the parser's complaint about the offending package rather
/// than a bare "parse error".
///
/// r[verify vixen.capability.package-is-data]
#[test]
fn a_malformed_declared_packages_file_is_a_loud_typed_error() {
    let _declaration = declaration_lock();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = packages_file(
        &dir,
        r#"
[[package]]
name = "Broken"
protocol = "telepathy"
"#,
    );
    declare(&path);
    let result = run_source_with_manifest(
        "#[test]\nfn t(s: Sh) -> Stream<Check> { yield expect_eq(1, 1); }\n",
        manifest(vec![offer("Sh", "sh", &[])]),
    );
    let Err(RunError::Packages(PackagesLoadError::Malformed {
        path: named,
        detail,
    })) = result
    else {
        panic!("a malformed declared packages file errs typed, got {result:?}");
    };
    assert_eq!(named, path, "the error names the declared path");
    assert!(
        detail.contains("Broken") && detail.contains("telepathy"),
        "the detail names the package and what was wrong with it: {detail}"
    );
}

/// A declared file that redefines a shipped package refuses. Two sources
/// disagreeing about what a tool name means is the silent-wrong-tool failure
/// the design exists to prevent, so it is a refusal rather than a merge in
/// whichever order registration happened to run.
///
/// r[verify vixen.capability.package-is-data]
#[test]
fn redefining_a_registered_package_refuses() {
    let _declaration = declaration_lock();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = packages_file(
        &dir,
        r#"
[[package]]
name = "Sh"
protocol = "progressive-lines-v1"
"#,
    );
    declare(&path);
    let result = run_source_with_manifest(
        "#[test]\nfn t(s: Sh) -> Stream<Check> { yield expect_eq(1, 1); }\n",
        manifest(vec![offer("Sh", "sh", &[])]),
    );
    let Err(RunError::Packages(PackagesLoadError::Conflict { path: named, name })) = result else {
        panic!("a contradicting redefinition errs typed, got {result:?}");
    };
    assert_eq!(named, path);
    assert_eq!(name, "Sh", "the error names the contested package");
}
