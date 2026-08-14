//! The machine manifest's acceptance tests
//! (`vix-core/docs/content/spec/vixen/machine.md`, "Acceptance").
//!
//! Every refusal test pins the same three facts: the failure is TYPED (a
//! `CapabilityRefusal` naming both sides — what the program requires, what
//! the machine offers), it is PRE-EFFECT (zero spawns, by counter; no check
//! ran at all), and it agrees across the plain and chaos lanes. Every passing
//! test runs a real process through a fake tool, because the point of a
//! manifest that can refuse is that the same program RUNS when the machine's
//! word covers it.

#![cfg(unix)]

use std::sync::{Arc, Mutex};

use vix::runtime::{
    EventLog, ExecBackend, ExecEventSender, ExecInvocation, ExecWorkspace, PrimitiveServices,
    Runtime,
};
use vixen_primitives::capability_package::Target;
use vixen_runtime::manifest::{
    CapabilityOffer, MachineManifest, RefusalCause, TargetRequirement, ToolchainPin, host_target,
    static_requirements,
};
use vixen_runtime::ratchet::{
    RatchetReport, prepare_source_with_manifest, run_source_with_manifest,
};

/// `VIX_MACHINE_MANIFEST` is process-global, and `cargo test` runs a suite's
/// tests as threads in ONE process — so the two tests that certify env-declared
/// discovery must not overlap with each other. Every other test in this file
/// states its machine word in Rust and never reads the variable, which is what
/// keeps this lock down to two holders. Under nextest each test is its own
/// process and the lock is uncontended. See issue #14.
static MANIFEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `body` with `VIX_MACHINE_MANIFEST` declaring `path`, holding the lock
/// across set → run → remove so no other holder observes a half-state. The
/// removal is a `Drop`, so a failing assertion inside `body` unwinds without
/// leaving the variable set for whoever runs next. A poisoned lock is taken
/// anyway: the only state it guards is the variable this guard restores.
fn with_declared_manifest<T>(path: &str, body: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: the lock is held, so no other test thread is reading or
            // writing the variable.
            unsafe { std::env::remove_var(vixen_runtime::manifest::MANIFEST_ENV) };
        }
    }

    let _guard = MANIFEST_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: the lock is held, so no other test thread is reading or writing
    // the variable, and it is set before any runtime thread exists.
    unsafe { std::env::set_var(vixen_runtime::manifest::MANIFEST_ENV, path) };
    let _restore = Restore;
    body()
}

/// A manifest offering `capabilities` on an `x86_64-unknown-linux-gnu`-style
/// host — the design note's "Linux-only machine", spelled with the actual
/// host triple so diagnostics carry the runner's declared host fact.
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

/// The same offer, with the machine's word about which toolchain it is.
fn offer_stating(ty: &str, program: &str, toolchain: &str) -> CapabilityOffer {
    CapabilityOffer {
        toolchain: Some(toolchain.to_owned()),
        ..offer(ty, program, &[])
    }
}

#[derive(Default)]
struct RecordingExecBackend {
    invocations: Mutex<Vec<ExecInvocation>>,
}

impl ExecBackend for RecordingExecBackend {
    fn begin(
        &self,
        invocation: ExecInvocation,
        events: ExecEventSender,
    ) -> Result<ExecWorkspace, String> {
        self.invocations
            .lock()
            .expect("invocation recorder mutex poisoned")
            .push(invocation.clone());
        vixen_runtime::host_exec::HostExecBackend.begin(invocation, events)
    }
}

/// Write one executable fake tool into `dir` and return its absolute path.
/// The scripts stand in for rustc/go/gcc: the acceptance criteria are about
/// the binding check, not about real toolchains.
fn fake_tool(dir: &tempfile::TempDir, name: &str, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write fake tool");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("mark fake tool executable");
    path.to_str().expect("tool path is UTF-8").to_owned()
}

/// Assert the report is a pre-effect refusal: nothing ran, both lanes agree,
/// and exactly one typed refusal exists per lane.
fn assert_refused(report: &RatchetReport) -> &vixen_runtime::manifest::CapabilityRefusal {
    assert!(!report.passed(), "a refused program cannot pass");
    assert!(
        report.agrees(),
        "plain and chaos agree on the refusal: {report:#?}"
    );
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(
            lane.counters.effect_spawns, 0,
            "no process spawned before the refusal"
        );
        assert!(
            lane.checks.is_empty() && lane.values.is_empty(),
            "no island of the refused test was submitted"
        );
        assert_eq!(lane.refusals.len(), 1, "one refusal, typed: {lane:#?}");
    }
    &report.plain.refusals[0]
}

/// The exe-case program from the design note, adapted to the surface that
/// exists (`.text()` instead of `| exists`): the requirement IS the
/// invocation — nothing beside the code spells a target.
const EXE_CASE: &str = r#"
#[test]
fn build(rustc: Rustc) -> Stream<Check> {
    let out = exec rustc`--target x86_64-pc-windows-msvc main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;

/// Acceptance 1: **missing type.** A root requiring a capability type the
/// manifest lacks fails typed, pre-effect, naming both sides.
///
/// r[verify vixen.machine.manifest]
/// r[verify vixen.machine.binding-fails-before-effects]
#[test]
fn a_capability_type_absent_from_the_manifest_refuses_before_any_effect() {
    // The harness default offers Echo/Sh/ProgressiveSh — no Rustc.
    let report = run_source_with_manifest(EXE_CASE, MachineManifest::ratchet_default())
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(refusal.test, "build");
    assert_eq!(refusal.parameter, "rustc");
    assert_eq!(refusal.required_type, "Rustc");
    assert_eq!(
        refusal.offered, None,
        "the type is absent from the manifest entirely"
    );
    let diagnostic = refusal.to_string();
    assert!(
        diagnostic.contains("`build` demands Rustc")
            && diagnostic.contains("no effect was started"),
        "the diagnostic names the demanding side: {diagnostic}"
    );
}

/// Acceptance 2, refusal half: **the exe case** on a Linux-only manifest.
/// The type is present; the extracted `--target` capture is not among the
/// offered targets. Typed refusal before any process exists, and the
/// diagnostic names both sides in `Target` vocabulary.
///
/// r[verify vixen.machine.binding-fails-before-effects]
/// r[verify vixen.machine.requirements-from-use]
#[test]
fn the_exe_case_refuses_pre_effect_on_a_linux_only_manifest() {
    let linux_only = manifest(vec![offer(
        "Rustc",
        "rustc-must-never-spawn",
        &["x86_64-unknown-linux-gnu"],
    )]);
    let report = run_source_with_manifest(EXE_CASE, linux_only)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(refusal.required_type, "Rustc");
    assert_eq!(
        refusal.required_target(),
        Some("x86_64-pc-windows-msvc"),
        "the requirement was extracted from the invocation, normalized to a Target"
    );
    let offered = refusal.offered.as_deref().expect("the offer side is named");
    assert!(
        offered.contains("x86_64-unknown-linux-gnu"),
        "the diagnostic names what the machine offers: {offered}"
    );
}

/// Acceptance 2, passing half: the same program, a manifest whose `Rustc`
/// offers the target — it runs, through a fake rustc that produces the
/// artifact the program checks.
///
/// r[verify vixen.machine.manifest]
/// r[verify vixen.machine.requirements-from-use]
#[test]
fn the_exe_case_runs_when_the_manifest_offers_the_target() {
    let tools = tempfile::tempdir().expect("tool dir");
    let rustc = fake_tool(&tools, "rustc", "printf exe > main.exe");
    let offering = manifest(vec![offer(
        "Rustc",
        &rustc,
        &["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"],
    )]);
    let report = run_source_with_manifest(EXE_CASE, offering).expect("the exe case runs");
    assert!(report.passed(), "the offered target runs: {report:#?}");
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(
            lane.counters.effect_spawns, 1,
            "one process, really spawned"
        );
        assert!(lane.refusals.is_empty());
    }
}

/// The exec rail keys a plan under the capability value's identity, so every
/// manifest fact that defines the offered capability must participate in that
/// identity. Target order and duplication are not facts: containment treats
/// the target list as a set, so publication canonicalizes it.
#[test]
fn capability_identity_carries_toolchain_and_canonical_target_facts() {
    let capability_ty = vix::compiler::capability_type("Rustc");
    let mut runtime = Runtime::new(EventLog::default());
    let base = runtime.publish_capability(
        &capability_ty,
        "/toolchains/rustc",
        Some("1.89.0"),
        ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"],
    );
    let changed_toolchain = runtime.publish_capability(
        &capability_ty,
        "/toolchains/rustc",
        Some("1.90.0"),
        ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"],
    );
    let changed_targets = runtime.publish_capability(
        &capability_ty,
        "/toolchains/rustc",
        Some("1.89.0"),
        ["x86_64-unknown-linux-gnu"],
    );
    let reordered_targets = runtime.publish_capability(
        &capability_ty,
        "/toolchains/rustc",
        Some("1.89.0"),
        [
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
        ],
    );

    assert_ne!(base.identity, changed_toolchain.identity);
    assert_ne!(base.identity, changed_targets.identity);
    assert_eq!(base.identity, reordered_targets.identity);
}

/// The go-shaped program: the target rides declared environment roles, not a
/// flag. Same extraction, same vocabulary, same refusal — proving the
/// mechanism is not flag-shaped.
const ENV_CASE_CROSS: &str = r#"
#[test]
fn build(go: Go) -> Stream<Check> {
    let out = exec go`GOOS=windows GOARCH=amd64 build`;
    yield expect_eq((out.tree / "target.txt").text(), "windows/amd64");
}
"#;

/// Acceptance 3, refusal half: **the env-role case.** `GOOS=windows
/// GOARCH=amd64` normalizes to `x86_64-pc-windows-gnu` through the package's
/// grammar; a Linux-only `Go` offer refuses pre-effect.
///
/// r[verify vixen.machine.requirements-from-use]
/// r[verify vixen.machine.binding-fails-before-effects]
#[test]
fn an_env_role_target_refuses_pre_effect_on_a_linux_only_manifest() {
    let linux_only = manifest(vec![offer(
        "Go",
        "go-must-never-spawn",
        &["x86_64-unknown-linux-gnu"],
    )]);
    let report = run_source_with_manifest(ENV_CASE_CROSS, linux_only)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(refusal.required_type, "Go");
    assert_eq!(
        refusal.required_target(),
        Some("x86_64-pc-windows-gnu"),
        "the env roles normalized into Target vocabulary — never GOOS/GOARCH strings"
    );
}

/// Acceptance 3, passing half: the same shape with host-satisfiable roles
/// runs — and the capture is REAL: the fake tool reads `$GOOS`/`$GOARCH`
/// from its actual process environment and writes them into the tree the
/// program checks, so the declared env roles demonstrably reached the
/// spawned process.
///
/// r[verify vixen.machine.requirements-from-use]
#[test]
fn an_env_role_target_runs_and_the_roles_reach_the_process_environment() {
    const ENV_CASE_HOST: &str = r#"
#[test]
fn build(go: Go) -> Stream<Check> {
    let out = exec go`GOOS=linux GOARCH=amd64 build`;
    yield expect_eq((out.tree / "target.txt").text(), "linux/amd64");
}
"#;
    let tools = tempfile::tempdir().expect("tool dir");
    let go = fake_tool(
        &tools,
        "go",
        r#"printf '%s/%s' "$GOOS" "$GOARCH" > target.txt"#,
    );
    let offering = manifest(vec![offer("Go", &go, &["x86_64-unknown-linux-gnu"])]);
    let report = run_source_with_manifest(ENV_CASE_HOST, offering).expect("the env case runs");
    assert!(
        report.passed(),
        "the process saw GOOS/GOARCH in its environment: {report:#?}"
    );
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(lane.counters.effect_spawns, 1);
    }
}

/// A package-declared environment role is also an ambient-environment deny
/// list. When the plan omits GOOS/GOARCH, inherited variables must not decide a
/// target behind the manifest binder's back.
#[test]
fn env_target_roles_are_removed_from_the_ambient_process_environment() {
    const ENV_DEFAULTS: &str = r#"
#[test]
fn build(go: Go) -> Stream<Check> {
    let out = exec go`build`;
    yield expect_eq((out.tree / "target.txt").text(), "clean");
}
"#;
    let tools = tempfile::tempdir().expect("tool dir");
    let go = fake_tool(&tools, "go", "printf clean > target.txt");
    let backend = Arc::new(RecordingExecBackend::default());
    let report = prepare_source_with_manifest(ENV_DEFAULTS, manifest(vec![offer("Go", &go, &[])]))
        .expect("the env-default case prepares")
        .execute_with_primitive_services(
            PrimitiveServices::default().with_exec_backend(backend.clone()),
        )
        .expect("the env-default case runs");
    assert!(report.passed(), "the env-default case passes: {report:#?}");

    let invocations = backend
        .invocations
        .lock()
        .expect("invocation recorder mutex poisoned");
    assert_eq!(invocations.len(), 2, "plain and chaos each spawn once");
    for invocation in invocations.iter() {
        assert_eq!(invocation.env_remove, ["GOOS", "GOARCH"]);
        assert!(
            invocation.env.is_empty(),
            "the plan assigned no environment"
        );
    }
}

/// Acceptance 4: **neutral tools run anywhere.** The argv contains a literal
/// `--target` element and a triple — but `Sh`'s grammar declares no target
/// role, so no requirement is imposed and the program runs on a manifest
/// offering no targets at all. Roles come from the grammar, never from
/// string-sniffing the plan (`machine.capability.no-argv-dialect`).
///
/// r[verify vixen.machine.requirements-from-use]
#[test]
fn a_target_neutral_invocation_imposes_no_target_requirement() {
    const NEUTRAL: &str = r#"
#[test]
fn neutral(sh: Sh) -> Stream<Check> {
    let out = exec sh`-c "printf done" ignored --target x86_64-pc-windows-msvc`;
    yield expect_eq(out.stdout.lines(), ["done"]);
}
"#;
    let report = run_source_with_manifest(NEUTRAL, MachineManifest::ratchet_default())
        .expect("a neutral invocation runs anywhere its tool exists");
    assert!(
        report.passed(),
        "no target requirement was invented: {report:#?}"
    );
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(lane.counters.effect_spawns, 1);
        assert!(lane.refusals.is_empty());
    }
}

/// Acceptance 5: **the static report.** The requirement set — capability
/// parameter types plus literal role captures — is readable off the compiled
/// module without executing anything, in exactly the "needs `Rustc`
/// producing `x86_64-pc-windows-msvc`" shape.
///
/// r[verify vixen.machine.requirements-are-static]
#[test]
fn the_requirement_set_is_reported_without_executing() {
    let module = vixen_runtime::default_compiler()
        .compile(EXE_CASE)
        .expect("the exe case compiles")
        .module;
    let requirements = static_requirements(&module).expect("the report needs no execution");
    let [test] = requirements.as_slice() else {
        panic!("one test, one requirement set: {requirements:#?}");
    };
    assert_eq!(test.test, "build");
    let [rustc] = test.capabilities.as_slice() else {
        panic!("one capability requirement: {test:#?}");
    };
    assert_eq!(rustc.parameter, "rustc");
    assert_eq!(rustc.ty, "Rustc");
    assert_eq!(
        rustc.targets,
        vec![TargetRequirement::Literal(Target::new(
            "x86_64-pc-windows-msvc"
        ))],
        "needs Rustc producing x86_64-pc-windows-msvc, statically"
    );
}

/// The static report's honest degradation: a target spliced from a computed
/// value is reported as "decided at run time", never silently dropped and
/// never guessed.
///
/// r[verify vixen.machine.requirements-are-static]
#[test]
fn a_computed_capture_degrades_honestly_in_the_static_report() {
    const COMPUTED: &str = r#"
fn os(windows: Bool) -> String {
    if windows { "pc-windows-msvc" } else { "unknown-linux-gnu" }
}
#[test]
fn build(rustc: Rustc) -> Stream<Check> {
    let os = os(1 == 1);
    let out = exec rustc`--target x86_64-{os} main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;
    let module = vixen_runtime::default_compiler()
        .compile(COMPUTED)
        .expect("the computed-capture case compiles")
        .module;
    let requirements = static_requirements(&module).expect("the report needs no execution");
    let rustc = requirements[0]
        .capabilities
        .iter()
        .find(|capability| capability.ty == "Rustc")
        .expect("the Rustc requirement row exists");
    assert_eq!(
        rustc.targets,
        vec![TargetRequirement::Computed],
        "a computed capture is 'target decided at run time': {requirements:#?}"
    );
}

/// The fact-shaped end of the generality table: `MingwGcc` spells no target
/// anywhere in an invocation — the capability's own target facts are the
/// claim, checked against the package's fixed Windows target. Both ways: a
/// native-only offer refuses pre-effect; the cross-target offer runs on a
/// Linux host.
///
/// r[verify vixen.machine.requirements-from-use]
/// r[verify vixen.machine.facts-are-fields]
#[test]
fn a_fact_shaped_capability_is_checked_against_its_own_target_facts() {
    const FACT_CASE: &str = r#"
#[test]
fn compile(gcc: MingwGcc) -> Stream<Check> {
    let out = exec gcc`main.c`;
    yield expect_eq((out.tree / "a.out").text(), "obj");
}
"#;
    // The package says this binary targets Windows. A native-only offer cannot
    // satisfy that fact, regardless of the runner's host.
    let native_only = manifest(vec![offer(
        "MingwGcc",
        "gcc-must-never-spawn",
        &[host_target().as_str()],
    )]);
    let report = run_source_with_manifest(FACT_CASE, native_only)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(refusal.required_type, "MingwGcc");
    assert_eq!(
        refusal.required_target(),
        Some("x86_64-pc-windows-gnu"),
        "no capture exists; the package supplies its fixed target"
    );

    // A Windows-targeting MinGW capability is legitimate on the Linux runner:
    // the capability's target and the machine's host are different facts.
    let tools = tempfile::tempdir().expect("tool dir");
    let gcc = fake_tool(&tools, "gcc", "printf obj > a.out");
    let cross = manifest(vec![offer("MingwGcc", &gcc, &["x86_64-pc-windows-gnu"])]);
    let report = run_source_with_manifest(FACT_CASE, cross).expect("the cross gcc runs");
    assert!(report.passed(), "the fixed target is offered: {report:#?}");
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(lane.counters.effect_spawns, 1);
    }
}

/// The pin case: the same program shape, now saying out loud which toolchain
/// it needs. The demand rides the PARAMETER rather than the invocation because
/// a version is a fact about the tool itself — no argv spells it, which is
/// exactly the gap `require(…)` used to have to cover.
const PINNED_CASE: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain_range: ">=1.89, <1.90" }) -> Stream<Check> {
    let out = exec rustc`main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;

/// A pin the machine's word falls outside of refuses pre-effect, and the
/// diagnostic carries the attribution: what was asked for, what was stated.
///
/// r[verify vixen.machine.version-pin]
/// r[verify vixen.machine.binding-fails-before-effects]
#[test]
fn a_toolchain_outside_the_pin_refuses_pre_effect() {
    let stale = manifest(vec![offer_stating(
        "Rustc",
        "rustc-must-never-spawn",
        "1.88.0",
    )]);
    let report = run_source_with_manifest(PINNED_CASE, stale)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(refusal.required_type, "Rustc");
    assert_eq!(
        refusal.cause,
        RefusalCause::Toolchain {
            pin: ">=1.89, <1.90".to_owned(),
            stated: "1.88.0".to_owned(),
        },
        "both sides of the comparison are on the record"
    );
    let diagnostic = refusal.to_string();
    assert!(
        diagnostic.contains("demands Rustc toolchain >=1.89, <1.90")
            && diagnostic.contains("machine states toolchain 1.88.0"),
        "the refusal reads as attribution: {diagnostic}"
    );
}

/// The passing half: the machine's word inside the pin runs the program, and
/// the pin is a real SET — `1.89.3` satisfies `>=1.89, <1.90` where string
/// equality against `1.89` would not.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn a_toolchain_inside_the_pin_runs() {
    let tools = tempfile::tempdir().expect("tool dir");
    let rustc = fake_tool(&tools, "rustc", "printf exe > main.exe");
    let current = manifest(vec![offer_stating("Rustc", &rustc, "1.89.3")]);
    let report = run_source_with_manifest(PINNED_CASE, current).expect("the pinned case runs");
    assert!(report.passed(), "1.89.3 is inside the pin: {report:#?}");
    for lane in [&report.plain, &report.chaos] {
        assert_eq!(lane.counters.effect_spawns, 1);
        assert!(lane.refusals.is_empty());
    }
}

/// A machine that states no toolchain cannot satisfy a pin. Silently matching
/// would turn "we asked for 1.89" into a claim nobody ever made — the refusal
/// says the attribution is missing, which is the honest verdict.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn an_unstated_toolchain_cannot_satisfy_a_pin() {
    let silent = manifest(vec![offer("Rustc", "rustc-must-never-spawn", &[])]);
    let report = run_source_with_manifest(PINNED_CASE, silent)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(
        refusal.cause,
        RefusalCause::ToolchainUnstated {
            pin: ">=1.89, <1.90".to_owned(),
        }
    );
    assert!(
        refusal.to_string().contains("states no toolchain"),
        "the diagnostic names the missing side: {refusal}"
    );
}

/// An unconstrained parameter constrains nothing: the machine's stated
/// toolchain is a fact the program never asked about, and the program runs.
/// This is the same "absent constraint matches any offer" rule the offered
/// targets already follow.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn an_unpinned_parameter_ignores_the_stated_toolchain() {
    const UNPINNED: &str = r#"
#[test]
fn build(rustc: Rustc) -> Stream<Check> {
    let out = exec rustc`main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;
    let tools = tempfile::tempdir().expect("tool dir");
    let rustc = fake_tool(&tools, "rustc", "printf exe > main.exe");
    let ancient = manifest(vec![offer_stating("Rustc", &rustc, "0.9.0")]);
    let report = run_source_with_manifest(UNPINNED, ancient).expect("the unpinned case runs");
    assert!(
        report.passed(),
        "no pin, no comparison, no refusal: {report:#?}"
    );
}

/// Neither side may be waved through unread. A pin that is not a version
/// requirement refuses instead of matching everything — the failure mode a
/// string comparison would have hidden.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn a_pin_that_is_not_a_version_requirement_refuses() {
    const NONSENSE_PIN: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain_range: "newest please" }) -> Stream<Check> {
    let out = exec rustc`main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;
    let report = run_source_with_manifest(
        NONSENSE_PIN,
        manifest(vec![offer_stating(
            "Rustc",
            "rustc-must-never-spawn",
            "1.89.0",
        )]),
    )
    .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    let RefusalCause::UnreadablePin { pin, detail } = &refusal.cause else {
        panic!("an unreadable pin is its own cause: {refusal:#?}");
    };
    assert_eq!(pin, "newest please");
    assert!(!detail.is_empty(), "the parse failure is carried");
    // An unreadable pin is wrong on EVERY machine, so the diagnostic must not
    // implicate this one: naming what it offers would read as though a
    // different machine might have satisfied the pin. None could.
    let rendered = refusal.to_string();
    assert!(
        rendered.contains("this is the source, not the machine"),
        "the diagnostic places the fault: {rendered}"
    );
    assert!(
        !rendered.contains("machine offers"),
        "and does not cite an offer as though it were evidence: {rendered}"
    );
}

/// The other side of the same rule: a machine that states something that is
/// not a version refuses a pin rather than being compared by string luck.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn a_stated_toolchain_that_is_not_a_version_refuses_a_pin() {
    let vague = manifest(vec![offer_stating(
        "Rustc",
        "rustc-must-never-spawn",
        "host",
    )]);
    let report = run_source_with_manifest(PINNED_CASE, vague)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    let RefusalCause::UnorderableToolchain { pin, stated, .. } = &refusal.cause else {
        panic!("an unreadable machine word is its own cause: {refusal:#?}");
    };
    assert_eq!(stated, "host");
    assert_eq!(pin, ">=1.89, <1.90", "the demand stays on the record");
    assert!(
        refusal.to_string().contains("which has no ordering"),
        "the diagnostic says which side could not be read: {refusal}"
    );
    assert!(
        refusal.to_string().contains(r#"toolchain: "host""#),
        "and names the exact pin that WOULD reach this machine: {refusal}"
    );
}

/// The case the whole vocabulary exists for: a tool whose version cannot be
/// put on a number line. Quartus states `22.1std`; strict semver rejects it and
/// so does any range. An EXACT pin parses neither side and reaches it.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn an_exact_pin_reaches_a_tool_whose_version_has_no_ordering() {
    const SOURCE: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain: "22.1std" }) -> Stream<Check> {
    let out = exec rustc`--version`;
    yield expect_eq(out.stdout.text().trim(), "unreachable");
}
"#;
    // Asked of the binder directly: a pin that BINDS lets the run proceed to
    // the spawn, and the placeholder program is not a thing on disk. What is
    // under test is the refusal set, so that is what gets read.
    assert!(
        refusals_for(SOURCE, "22.1std").is_empty(),
        "an exact pin matching the machine's word binds"
    );

    let refusals = refusals_for(SOURCE, "22.1pro");
    let [refusal] = refusals.as_slice() else {
        panic!("a differing exact pin is one refusal: {refusals:#?}");
    };
    let RefusalCause::Toolchain { pin, stated } = &refusal.cause else {
        panic!("a differing exact pin is a toolchain refusal: {refusal:#?}");
    };
    assert_eq!(pin, "22.1std");
    assert_eq!(stated, "22.1pro");
}

/// Bind one source against a machine stating `toolchain`, without running it.
/// A satisfied pin would otherwise carry on into a spawn, and what is under
/// test here is the binding, not the process.
fn refusals_for(source: &str, stated: &str) -> Vec<vixen_runtime::manifest::CapabilityRefusal> {
    let module = vixen_runtime::default_compiler()
        .compile(source)
        .expect("the pinned case compiles")
        .module;
    let requirements = static_requirements(&module).expect("the report needs no execution");
    let machine = manifest(vec![offer_stating(
        "Rustc",
        "rustc-must-never-spawn",
        stated,
    )]);
    requirements
        .iter()
        .flat_map(|test| machine.bind(test))
        .collect()
}

/// The rename's trap, caught at compile time. `toolchain:` is exact, so a value
/// that is plainly a range would compare unequal to every version forever and
/// blame the machine for it.
#[test]
fn an_exact_pin_that_is_plainly_a_range_is_refused_at_compile_time() {
    const SOURCE: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain: ">=1.89" }) -> Stream<Check> {
    let out = exec rustc`--version`;
    yield expect_eq(out.stdout.text().trim(), "unreachable");
}
"#;
    let diagnostics = vixen_runtime::default_compiler()
        .compile(SOURCE)
        .expect_err("a range under the exact key is refused");
    let rendered = format!("{diagnostics:?}");
    assert!(
        rendered.contains("toolchain_range"),
        "the diagnostic names the key that WOULD work: {rendered}"
    );
}

/// One question per pin: a parameter cannot ask both.
#[test]
fn a_parameter_pins_exactly_or_by_range_never_both() {
    const SOURCE: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain: "1.89.0", toolchain_range: ">=1.89" }) -> Stream<Check> {
    let out = exec rustc`--version`;
    yield expect_eq(out.stdout.text().trim(), "unreachable");
}
"#;
    let diagnostics = vixen_runtime::default_compiler()
        .compile(SOURCE)
        .expect_err("two pins on one parameter are refused");
    let rendered = format!("{diagnostics:?}");
    assert!(
        rendered.contains("never both"),
        "the diagnostic says why: {rendered}"
    );
}

/// Arity is free on both sides. MSVC states four components and Xcode states
/// two; a range written with two bounds either one.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn a_range_spans_versions_of_any_arity() {
    const SOURCE: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain_range: ">=19.38" }) -> Stream<Check> {
    let out = exec rustc`--version`;
    yield expect_eq(out.stdout.text().trim(), "unreachable");
}
"#;
    // The MSVC shape: `19.38.33130.0` against a two-component bound.
    assert!(
        refusals_for(SOURCE, "19.38.33130.0").is_empty(),
        "four stated components satisfy a two-component bound"
    );
    assert!(
        !refusals_for(SOURCE, "19.37.99999.0").is_empty(),
        "and the comparison is on the numbers, not the text"
    );
}

/// A prerelease toolchain — `1.99.0-nightly` — does not slip through a plain
/// range. This is Cargo's rule, kept deliberately: a nightly is a materially
/// different tool from the stable release whose number it carries, and letting
/// it satisfy `>=1.89` silently is exactly the pretending the whole model
/// refuses. Someone who means it says so, by pinning the line
/// (`>=1.99.0-nightly`) or dropping the pin.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn a_prerelease_toolchain_does_not_satisfy_a_plain_range() {
    let nightly = manifest(vec![offer_stating(
        "Rustc",
        "rustc-must-never-spawn",
        "1.99.0-nightly",
    )]);
    let report = run_source_with_manifest(PINNED_CASE, nightly)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(
        refusal.cause,
        RefusalCause::Toolchain {
            pin: ">=1.89, <1.90".to_owned(),
            stated: "1.99.0-nightly".to_owned(),
        },
        "the refusal names the prerelease it declined, not a parse failure"
    );
}

/// The opt-in half: a pin that names the prerelease line admits it, so the
/// rule above is a default and not a wall.
///
/// r[verify vixen.machine.version-pin]
#[test]
fn a_pin_naming_the_prerelease_line_admits_it() {
    const NIGHTLY_PIN: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain_range: ">=1.99.0-nightly" }) -> Stream<Check> {
    let out = exec rustc`main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;
    let tools = tempfile::tempdir().expect("tool dir");
    let rustc = fake_tool(&tools, "rustc", "printf exe > main.exe");
    let nightly = manifest(vec![offer_stating("Rustc", &rustc, "1.99.0-nightly")]);
    let report = run_source_with_manifest(NIGHTLY_PIN, nightly).expect("the nightly case runs");
    assert!(report.passed(), "the pin named the line: {report:#?}");
}

/// The pin joins the static report: "needs `Rustc` at `>=1.89, <1.90`" is
/// readable off the compiled module without executing anything, exactly as the
/// target requirements are.
///
/// r[verify vixen.machine.requirements-are-static]
/// r[verify vixen.machine.version-pin]
#[test]
fn the_static_report_carries_the_version_pin() {
    let module = vixen_runtime::default_compiler()
        .compile(PINNED_CASE)
        .expect("the pinned case compiles")
        .module;
    let requirements = static_requirements(&module).expect("the report needs no execution");
    let [test] = requirements.as_slice() else {
        panic!("one test, one requirement set: {requirements:#?}");
    };
    let [rustc] = test.capabilities.as_slice() else {
        panic!("one capability requirement: {test:#?}");
    };
    assert_eq!(
        rustc.toolchain_pins,
        vec![ToolchainPin::Range(">=1.89, <1.90".to_owned())]
    );
    assert!(
        rustc.targets.is_empty(),
        "this plan spells no target: {rustc:#?}"
    );
}

/// A `where` clause is refused where nothing could ever compare it. An
/// ordinary value parameter has no offer behind it, so a constraint there would
/// read like a guarantee and enforce nothing.
#[test]
fn only_a_capability_parameter_takes_a_where_clause() {
    const ON_A_VALUE: &str = r#"
fn double(n: Int where { toolchain: ">=1" }) -> Int {
    n + n
}
#[test]
fn t(sh: Sh) -> Stream<Check> {
    yield expect_eq(double(2), 4);
}
"#;
    vixen_runtime::default_compiler()
        .compile(ON_A_VALUE)
        .expect_err("a constraint nothing can check is a compile error");
}

/// An unrecognized key is refused rather than dropped, for the same reason
/// `exec`'s `where` clause refuses one: a demand the author wrote and nothing
/// reads is indistinguishable from a satisfied demand.
#[test]
fn an_unknown_capability_constraint_is_refused() {
    const UNKNOWN_KEY: &str = r#"
#[test]
fn build(rustc: Rustc where { version: ">=1.89" }) -> Stream<Check> {
    let out = exec rustc`main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;
    vixen_runtime::default_compiler()
        .compile(UNKNOWN_KEY)
        .expect_err("`version` is not a capability constraint");
}

/// A requirement is static, so a pin cannot be an expression the program
/// computes: the value must be a literal the report can read off the source.
#[test]
fn a_computed_pin_is_refused() {
    const COMPUTED_PIN: &str = r#"
#[test]
fn build(rustc: Rustc where { toolchain: "1." + "89" }) -> Stream<Check> {
    let out = exec rustc`main.rs`;
    yield expect_eq((out.tree / "main.exe").text(), "exe");
}
"#;
    vixen_runtime::default_compiler()
        .compile(COMPUTED_PIN)
        .expect_err("a computed pin is not a static requirement");
}

/// The embedder-loads-config half of `vixen.machine.manifest`: the TOML
/// spelling round-trips into the same typed value the tests construct
/// directly.
///
/// r[verify vixen.machine.manifest]
#[test]
fn the_manifest_loads_from_its_toml_config_spelling() {
    let manifest = MachineManifest::from_toml(
        r#"
host = "x86_64-unknown-linux-gnu"

[[capability]]
ty = "Rustc"
program = "/toolchains/1.89.0/bin/rustc"
toolchain = "1.89.0"
targets = ["x86_64-unknown-linux-gnu"]

[[capability]]
ty = "Sh"
program = "/bin/sh"
"#,
    )
    .expect("the config spelling loads");
    assert_eq!(manifest.host, Target::new("x86_64-unknown-linux-gnu"));
    let rustc = manifest.offer("Rustc").expect("Rustc is offered");
    assert_eq!(rustc.program, "/toolchains/1.89.0/bin/rustc");
    assert_eq!(rustc.toolchain.as_deref(), Some("1.89.0"));
    assert_eq!(rustc.targets, vec![Target::new("x86_64-unknown-linux-gnu")]);
    let sh = manifest.offer("Sh").expect("Sh is offered");
    assert!(sh.targets.is_empty() && sh.toolchain.is_none());
}

/// Write a manifest TOML file into a fresh directory and return its path.
fn manifest_file(dir: &tempfile::TempDir, source: &str) -> String {
    let path = dir.path().join("machine.toml");
    std::fs::write(&path, source).expect("write manifest file");
    path.to_str().expect("manifest path is UTF-8").to_owned()
}

/// A Linux-only manifest document — the exe case's refusing machine word,
/// this time as a config file rather than a Rust value.
const LINUX_ONLY_TOML: &str = r#"
host = "x86_64-unknown-linux-gnu"

[[capability]]
ty = "Rustc"
program = "rustc-must-never-spawn"
targets = ["x86_64-unknown-linux-gnu"]
"#;

/// The file-loading path carries the machine's word with full force: a
/// manifest LOADED from a config file refuses the exe case exactly as the
/// directly-constructed value does — typed, pre-effect, naming both sides.
///
/// r[verify vixen.machine.manifest]
/// r[verify vixen.machine.binding-fails-before-effects]
#[test]
fn a_manifest_loaded_from_a_config_file_refuses_the_exe_case() {
    let dir = tempfile::tempdir().expect("manifest dir");
    let path = manifest_file(&dir, LINUX_ONLY_TOML);
    let manifest = vixen_runtime::manifest::load_manifest(&path).expect("the declared file loads");
    let report = run_source_with_manifest(EXE_CASE, manifest)
        .expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(refusal.required_type, "Rustc");
    assert_eq!(refusal.required_target(), Some("x86_64-pc-windows-msvc"));
    let offered = refusal.offered.as_deref().expect("the offer side is named");
    assert!(
        offered.contains("x86_64-unknown-linux-gnu"),
        "the config file's facts reach the diagnostic: {offered}"
    );
}

/// The runnable system reads the DECLARED manifest: with
/// `VIX_MACHINE_MANIFEST` naming a Linux-only manifest file, the ordinary
/// `run_source` entrypoint — no `with_manifest`, no Rust-side value — binds
/// against the file's machine word and refuses the exe case pre-effect.
///
/// r[verify vixen.machine.manifest]
#[test]
fn the_environment_declared_manifest_reaches_the_runnable_system() {
    let dir = tempfile::tempdir().expect("manifest dir");
    let path = manifest_file(&dir, LINUX_ONLY_TOML);
    let result = with_declared_manifest(&path, || vixen_runtime::ratchet::run_source(EXE_CASE));
    let report = result.expect("a binding refusal is a report verdict, never a runner error");
    let refusal = assert_refused(&report);
    assert_eq!(refusal.required_type, "Rustc");
    assert_eq!(
        refusal.required_target(),
        Some("x86_64-pc-windows-msvc"),
        "the declared file's word governed the run"
    );
}

/// A DECLARED manifest that is missing is a loud typed error at the
/// entrypoint — never a silent fall-back to the harness default. Under the
/// default this program would produce an ordinary refusal REPORT; the
/// distinction between `Err` and `Ok(refusal)` is exactly the loudness this
/// pins.
///
/// r[verify vixen.machine.manifest]
#[test]
fn a_missing_declared_manifest_is_a_loud_typed_error_never_a_silent_default() {
    let dir = tempfile::tempdir().expect("manifest dir");
    let missing = dir
        .path()
        .join("nowhere.toml")
        .to_str()
        .expect("path is UTF-8")
        .to_owned();
    let result = with_declared_manifest(&missing, || vixen_runtime::ratchet::run_source(EXE_CASE));
    let Err(vixen_runtime::ratchet::RunError::Manifest(
        vixen_runtime::manifest::ManifestLoadError::Unreadable { path, .. },
    )) = result
    else {
        panic!("a missing declared manifest errs typed, got {result:?}");
    };
    assert_eq!(path, missing, "the error names the declared path");
}

/// The explicit requirement fallback (`vixen.machine.requirements-from-use`):
/// a fact the command grammar cannot extract — here the tool's own runtime
/// self-report — is stated by the program itself through the stdlib
/// `require(condition) where { message }`, over the ordinary `fail`
/// mechanism. The failure is TYPED: a `RequirementFailure` record whose full
/// value identity — schema and message content — is pinned Rust-side, so the
/// author's message provably reaches the failure payload.
///
/// r[verify vixen.machine.requirements-from-use]
#[test]
fn an_unsatisfied_require_raises_the_typed_failure_with_the_message() {
    const SOURCE: &str = r#"
#[test]
fn guarded(sh: Sh) -> Stream<Check> {
    let probe = exec sh`-c "printf x86_64"`;
    let arch = probe.stdout.text();
    yield expect(require(arch == "aarch64") where { message: "this build step needs an aarch64 machine" });
}
"#;
    let report = run_source_with_manifest(SOURCE, MachineManifest::ratchet_default())
        .expect("an unsatisfied requirement is a check verdict, never a runner error");
    assert!(
        !report.passed(),
        "the requirement is unsatisfied: {report:#?}"
    );
    let message = "this build step needs an aarch64 machine";
    let string_schema = vix::vir::Type::String.schema_ref();
    let failure_ty = vix::vir::Type::Record(vix::vir::RecordType::new(
        "RequirementFailure",
        vec![vix::vir::RecordField {
            name: "message".to_owned(),
            ty: vix::vir::Type::String,
        }],
    ));
    let expected = vix::runtime::FramedNode::Variant {
        schema: failure_ty.schema_ref(),
        tag: 0,
        fields: vec![vix::runtime::FramedField {
            schema: string_schema.clone(),
            value: vix::runtime::FramedValue::Optional(Some(
                vix::runtime::FramedNode::leaf(string_schema, message.as_bytes().to_vec())
                    .identity(),
            )),
        }],
    }
    .identity();
    for lane in [&report.plain, &report.chaos] {
        let failure = lane
            .checks
            .first()
            .and_then(|check| check.failure.clone())
            .expect("the check records its typed failure");
        let vix::runtime::FailureValue::Raised { payload, .. } = failure else {
            panic!("require raises the typed RequirementFailure, got {failure:?}");
        };
        assert_eq!(
            payload, expected,
            "the payload is RequirementFailure with the author's message"
        );
    }
}

/// The satisfied half: the same guard over the fact the machine actually
/// reports is an ordinary `true` — the program runs and passes, and the
/// message wire is never demanded.
///
/// r[verify vixen.machine.requirements-from-use]
#[test]
fn a_satisfied_require_is_an_ordinary_passing_check() {
    const SOURCE: &str = r#"
#[test]
fn guarded(sh: Sh) -> Stream<Check> {
    let probe = exec sh`-c "printf x86_64"`;
    let arch = probe.stdout.text();
    yield expect(require(arch == "x86_64") where { message: "this build step needs an x86_64 machine" });
}
"#;
    let report = run_source_with_manifest(SOURCE, MachineManifest::ratchet_default())
        .expect("a satisfied requirement runs ordinarily");
    assert!(report.passed(), "the satisfied guard passes: {report:#?}");
    assert!(report.agrees(), "lanes agree: {report:#?}");
}

/// A declared file that reads but does not parse is the other loud half:
/// a typed `Malformed` error naming the path, with the parse detail carried,
/// and a `Display` rendering that says what happened.
///
/// r[verify vixen.machine.manifest]
#[test]
fn a_malformed_declared_manifest_is_a_loud_typed_error() {
    let dir = tempfile::tempdir().expect("manifest dir");
    let path = manifest_file(&dir, "host = 7\nnot a manifest at all [");
    let error = vixen_runtime::manifest::load_manifest(&path)
        .expect_err("a malformed manifest cannot load");
    let vixen_runtime::manifest::ManifestLoadError::Malformed {
        path: named,
        detail,
    } = &error
    else {
        panic!("a malformed manifest is Malformed, got {error:?}");
    };
    assert_eq!(named, &path);
    assert!(!detail.is_empty(), "the parse failure detail is carried");
    let rendered = error.to_string();
    assert!(
        rendered.contains("error[manifest]") && rendered.contains(&path),
        "the rendering names the declared path: {rendered}"
    );
}
