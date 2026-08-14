//! Capability packages as data: per-tool command-grammar knowledge that the
//! machine is banned from holding (`machine.capability.no-argv-dialect`) and
//! this crate is the 0.1 home for (`vixen.capability.packages-ship-in-vixen-primitives`).
//!
//! What a package contributes here is the requirement-bearing slice of its
//! command grammar: which roles of an invocation carry a *target* and how the
//! tool's dialect normalizes into the shared vocabulary — `Target` values,
//! never tool strings (`vixen.machine.requirements-from-use`). The manifest
//! and the binding check compare `Target`s; they never learn a dialect.
//!
//! # Data, not code
//!
//! A package is a name, an output protocol, and how the tool spells targets.
//! All three are config-shaped, so all three are *loaded* — [`REGISTRY`] is
//! filled at the entrypoint from [`DEFAULT_PACKAGES_TOML`] plus whatever the
//! invoker declares, and the builtins are expressed in the same document
//! format an invoker writes. There is no compiled-in table to edit: naming a
//! proprietary toolchain vix has never heard of is a file, not a recompile,
//! which is the entire point of the capability half of the design.
//!
//! Registration mirrors [`vix::schema::register_host_externs`] — a
//! process-lifetime registry, additive and idempotent, whose entries are
//! leaked so lookups hand back `&'static` data from deep inside effect
//! execution where no registry handle is threaded.
//!
//! r[impl vixen.capability.package-is-data]

use std::collections::BTreeMap;
use std::sync::Mutex;

use vix::runtime::ExecOutputProtocol;

/// A target triple as a first-class typed value.
///
/// HONEST STAND-IN: `machine.primitive.target-value` wants `Target` as a vix
/// value with schema, literal syntax (`t"..."`), and taxon-derived OS/arch.
/// None of that machinery exists on this branch, so this is the smallest
/// representation that keeps the checks typed: a newtype over the canonical
/// triple spelling. Every comparison in the manifest/binding path goes through
/// this type — upgrading it to the taxon-backed value changes its innards,
/// not the call sites.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Target(String);

impl Target {
    #[must_use]
    pub fn new(triple: impl Into<String>) -> Self {
        Self(triple.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Target {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One argv element of an exec plan as static analysis sees it: a literal the
/// program spelled, or a value computed at run time. Literal captures are
/// therefore checkable before anything executes; computed captures degrade
/// honestly (`vixen.machine.requirements-are-static`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanElement {
    Literal(String),
    Computed,
}

/// One extracted target capture: the requirement an invocation imposes through
/// the package's grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetCapture {
    /// The role's value is spelled literally — the requirement is static.
    Literal(Target),
    /// The role is present but its value is computed at run time: statically
    /// this is "target decided at run time", never silently no requirement.
    Computed,
}

/// A tool's target dialect as a declared word table, replacing the
/// `fn(&str) -> Target` pointer the compiled-in table used to hold. A pointer
/// cannot be written in a config file, and a package that cannot be written in
/// a config file cannot be added without recompiling vix.
///
/// A word the table does not map passes through unchanged. That is the honest
/// rule for an *unknown* dialect word: the package says what it knows and does
/// not invent a spelling for what it does not. A tool whose unknown words are
/// genuinely errors rejects them in command validation, which is the package's
/// own business and not this normalization's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordTable(BTreeMap<String, String>);

impl WordTable {
    /// The identity dialect: the tool already speaks the shared vocabulary, so
    /// every word maps to itself. rustc is this case.
    #[must_use]
    pub fn identity() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn normalize<'a>(&'a self, word: &'a str) -> &'a str {
        self.0.get(word).map_or(word, String::as_str)
    }
}

impl From<BTreeMap<String, String>> for WordTable {
    fn from(words: BTreeMap<String, String>) -> Self {
        Self(words)
    }
}

/// How a package's command grammar carries the target requirement — the
/// universal layer is the ROLE, the per-tool spelling is this data
/// (`vixen.machine.requirements-from-use`, the generality table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetDiscipline {
    /// No target role exists in this tool's grammar: a target-neutral
    /// invocation imposes no target requirement and runs wherever the tool
    /// exists. This is correct semantics, not a missing case.
    Neutral,
    /// rustc/clang-shaped: a target-role flag whose following argv element is
    /// the tool's target dialect, normalized through `words`.
    ArgvFlag { flag: String, words: WordTable },
    /// go-shaped: the target rides declared environment roles. This package
    /// family's command grammar spells environment assignments as leading
    /// `NAME=VALUE` argv elements (the `env(1)` convention); the named roles
    /// normalize through their own word tables and compose into one `Target`
    /// through `template`, which substitutes `{os}` and `{arch}`.
    EnvRoles {
        os_role: String,
        arch_role: String,
        os_words: WordTable,
        arch_words: WordTable,
        template: String,
    },
    /// mingw-gcc/`cl.exe`-shaped: the target is in no invocation at all — the
    /// binary/environment IS the target. The package supplies the fixed target
    /// demanded of the capability; it is not inferred from the runner's host.
    FixedTarget { target: Target },
}

/// One capability package's registered slice: its nominal type, its output
/// protocol (a command-package contract, `machine.primitive.command-package`),
/// and the requirement-bearing part of its command grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityPackage {
    pub name: String,
    pub protocol: ExecOutputProtocol,
    pub target_discipline: TargetDiscipline,
}

/// The packages vix ships with, in the same document format an invoker writes.
///
/// The first three are the ratchet corpus's v1 packages (target-neutral
/// shells); the last three are the machine-manifest design's generality stress
/// tests (`vixen.machine.requirements-from-use`) — one per ugly end of the
/// spelling table. 0.1 has no package distribution, so a test package is
/// spelled exactly like a production one
/// (`vixen.capability.packages-ship-in-vixen-primitives`).
///
/// These being a document rather than a Rust table is load-bearing, not
/// tidiness: it is the proof that the format can say everything the compiled-in
/// table said, so a package the format cannot express is a bug visible here
/// rather than a wall an invoker hits alone.
pub const DEFAULT_PACKAGES_TOML: &str = r#"
[[package]]
name = "Echo"
protocol = "exit-only"

[[package]]
name = "Sh"
protocol = "exit-only"

[[package]]
name = "ProgressiveSh"
protocol = "progressive-lines-v1"

# rustc's target dialect is already the shared triple vocabulary, so its word
# table is empty: the normalization is the identity injection into `Target`.
[[package]]
name = "Rustc"
protocol = "exit-only"
target = { kind = "argv-flag", flag = "--target" }

[[package]]
name = "Go"
protocol = "exit-only"
[package.target]
kind = "env-roles"
os_role = "GOOS"
arch_role = "GOARCH"
template = "{arch}-{os}"
os_words = { linux = "unknown-linux-gnu", windows = "pc-windows-gnu", darwin = "apple-darwin" }
arch_words = { amd64 = "x86_64", arm64 = "aarch64", "386" = "i686" }

[[package]]
name = "MingwGcc"
protocol = "exit-only"
target = { kind = "fixed", target = "x86_64-pc-windows-gnu" }
"#;

/// The TOML document shape, kept separate from the domain types exactly as
/// the machine manifest's is: the file is a flat tagged spelling, and turning
/// it into the typed discipline is a validation step with named failures
/// rather than a derive that admits half-filled shapes.
mod doc {
    #[derive(facet::Facet)]
    pub struct Packages {
        pub package: Vec<Package>,
    }

    #[derive(facet::Facet)]
    pub struct Package {
        pub name: String,
        pub protocol: String,
        pub target: Option<Target>,
    }

    #[derive(facet::Facet)]
    pub struct Target {
        pub kind: String,
        pub flag: Option<String>,
        pub words: Option<std::collections::BTreeMap<String, String>>,
        pub os_role: Option<String>,
        pub arch_role: Option<String>,
        pub os_words: Option<std::collections::BTreeMap<String, String>>,
        pub arch_words: Option<std::collections::BTreeMap<String, String>>,
        pub template: Option<String>,
        pub target: Option<String>,
    }
}

/// Parse a package document. Every failure names the package and the field, so
/// a hand-written package file fails the way a manifest does — loudly, at the
/// entrypoint, before anything runs.
pub fn packages_from_toml(source: &str) -> Result<Vec<CapabilityPackage>, String> {
    let parsed: doc::Packages = facet_toml::from_str(source).map_err(|error| error.to_string())?;
    parsed.package.into_iter().map(package_from_doc).collect()
}

fn package_from_doc(package: doc::Package) -> Result<CapabilityPackage, String> {
    let name = package.name;
    let fail = |detail: String| format!("capability package `{name}`: {detail}");
    let protocol = match package.protocol.as_str() {
        "exit-only" => ExecOutputProtocol::ExitOnly,
        "progressive-lines-v1" => ExecOutputProtocol::ProgressiveLinesV1,
        other => {
            return Err(fail(format!(
                "unknown output protocol `{other}` (known: exit-only, progressive-lines-v1)"
            )));
        }
    };
    // No `target` table at all is the target-neutral package, which is the
    // common shape for a proprietary tool and deserves to be the terse one.
    let Some(target) = package.target else {
        return Ok(CapabilityPackage {
            name,
            protocol,
            target_discipline: TargetDiscipline::Neutral,
        });
    };
    let required = |field: &str, value: Option<String>| {
        value.ok_or_else(|| fail(format!("target kind `{}` needs `{field}`", target.kind)))
    };
    let target_discipline = match target.kind.as_str() {
        "neutral" => TargetDiscipline::Neutral,
        "argv-flag" => TargetDiscipline::ArgvFlag {
            flag: required("flag", target.flag)?,
            words: target.words.unwrap_or_default().into(),
        },
        "env-roles" => TargetDiscipline::EnvRoles {
            os_role: required("os_role", target.os_role)?,
            arch_role: required("arch_role", target.arch_role)?,
            os_words: target.os_words.unwrap_or_default().into(),
            arch_words: target.arch_words.unwrap_or_default().into(),
            template: required("template", target.template)?,
        },
        "fixed" => TargetDiscipline::FixedTarget {
            target: Target::new(required("target", target.target)?),
        },
        other => {
            return Err(fail(format!(
                "unknown target kind `{other}` (known: neutral, argv-flag, env-roles, fixed)"
            )));
        }
    };
    Ok(CapabilityPackage {
        name,
        protocol,
        target_discipline,
    })
}

/// The registered packages, filled at the entrypoint. Entries are leaked
/// because a lookup happens inside effect execution, where the exec primitive
/// holds no registry handle — the same shape as the core's host-extern
/// registry, and bounded by the number of distinct package names a process
/// ever registers.
static REGISTRY: Mutex<Vec<&'static CapabilityPackage>> = Mutex::new(Vec::new());

/// A package registration that contradicts one already registered under the
/// same name. Two files disagreeing about what `Xcode` means is exactly the
/// silent-wrong-tool failure the whole design exists to prevent, so it is a
/// refusal rather than a last-writer-wins merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageConflict {
    pub name: String,
}

impl core::fmt::Display for PackageConflict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "capability package `{}` is already registered with a different grammar",
            self.name
        )
    }
}

/// Register packages for this process. Additive and idempotent: registering a
/// package byte-identical to one already registered is a no-op, and any other
/// redefinition of a registered name is a [`PackageConflict`].
///
/// All-or-nothing: the whole batch is checked before any of it is registered.
/// A file whose fourth package contradicts a shipped one must not leave its
/// first three registered — "a load that failed registered nothing" is the
/// invariant that lets the compiler config drop this error and still be sure
/// the nameable set was only ever narrowed, never half-widened.
///
/// May be called at any point before the name is first looked up, exactly like
/// the host-extern registration it mirrors.
pub fn register_packages(packages: Vec<CapabilityPackage>) -> Result<(), PackageConflict> {
    let mut registry = REGISTRY.lock().expect("package registry is not poisoned");
    let mut accepted: Vec<CapabilityPackage> = Vec::new();
    for package in packages {
        // A batch is checked against itself as well as the registry: two
        // `[[package]]` entries in one file disagreeing under one name is the
        // same contradiction as two files disagreeing.
        let agrees = registry
            .iter()
            .map(|known| &**known)
            .chain(accepted.iter())
            .find(|known| known.name == package.name)
            .map(|known| *known == package);
        match agrees {
            Some(true) => continue,
            Some(false) => return Err(PackageConflict { name: package.name }),
            None => accepted.push(package),
        }
    }
    for package in accepted {
        registry.push(Box::leak(Box::new(package)));
    }
    Ok(())
}

/// Register [`DEFAULT_PACKAGES_TOML`]. Idempotent, so every entrypoint that
/// needs a registry can call it without coordinating with the others.
///
/// The conflict an embedder can cause — registering its own `Sh` with a
/// different grammar before the shipped document loads — is returned, not
/// panicked: it is the ordinary two-sources-disagree case the design already
/// has a refusal for, and the caller that can name a path is the one that
/// should report it.
///
/// # Panics
///
/// If the shipped document does not parse, which is a bug in this crate rather
/// than anything an invoker can cause.
pub fn register_default_packages() -> Result<(), PackageConflict> {
    let packages =
        packages_from_toml(DEFAULT_PACKAGES_TOML).expect("shipped package document parses");
    register_packages(packages)
}

/// Look one package up by its nominal capability type name.
#[must_use]
pub fn capability_package(name: &str) -> Option<&'static CapabilityPackage> {
    REGISTRY
        .lock()
        .expect("package registry is not poisoned")
        .iter()
        .find(|package| package.name == name)
        .copied()
}

/// Every registered package name, in registration order — the set of nominal
/// capability types a program may name as a parameter.
#[must_use]
pub fn registered_package_names() -> Vec<&'static str> {
    REGISTRY
        .lock()
        .expect("package registry is not poisoned")
        .iter()
        .map(|package| package.name.as_str())
        .collect()
}

/// Whether an argv element is an environment assignment under the env-role
/// grammar: `NAME=VALUE` with a nonempty `[A-Za-z_][A-Za-z0-9_]*` name.
fn parse_assignment(element: &str) -> Option<(&str, &str)> {
    let (name, value) = element.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    Some((name, value))
}

/// Substitute `{os}` and `{arch}` into a target template in one pass.
///
/// One pass rather than chained `str::replace` because a role's value is the
/// tool's word, not the package's: `GOOS={arch}` would otherwise be rewritten
/// by the second replace into a triple neither the program nor the package
/// spelled. A dialect word cannot be allowed to name a placeholder — an
/// invocation deciding its own target spelling is the fabrication the typed
/// `Target` exists to prevent. An unrecognized `{…}` is left alone: the
/// package's template said it, so it is a literal.
fn render_template(template: &str, os: &str, arch: &str) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        rendered.push_str(&rest[..open]);
        let placeholder = &rest[open..];
        if let Some(tail) = placeholder.strip_prefix("{os}") {
            rendered.push_str(os);
            rest = tail;
        } else if let Some(tail) = placeholder.strip_prefix("{arch}") {
            rendered.push_str(arch);
            rest = tail;
        } else {
            rendered.push('{');
            rest = &placeholder[1..];
        }
    }
    rendered.push_str(rest);
    rendered
}

impl CapabilityPackage {
    /// Split the materialized plan into the environment assignments this
    /// package's grammar declares and the remaining argv. Identity is
    /// untouched: the demand preimage hashes the full normalized plan; this
    /// split happens host-side, at value redemption, exactly like the program
    /// name itself.
    #[must_use]
    pub fn split_invocation(
        &self,
        argv: Vec<String>,
    ) -> (Vec<String>, Vec<(String, String)>, Vec<String>) {
        match &self.target_discipline {
            TargetDiscipline::EnvRoles {
                os_role, arch_role, ..
            } => {
                let mut env = Vec::new();
                let mut rest = Vec::new();
                let mut in_leading = true;
                for element in argv {
                    if in_leading && let Some((name, value)) = parse_assignment(&element) {
                        env.push((name.to_owned(), value.to_owned()));
                        continue;
                    }
                    in_leading = false;
                    rest.push(element);
                }
                (vec![os_role.clone(), arch_role.clone()], env, rest)
            }
            _ => (Vec::new(), Vec::new(), argv),
        }
    }

    /// Extract the target captures a plan imposes under this package's
    /// grammar. `plan` is the argv as static analysis sees it; the same
    /// extraction applies to a fully materialized argv by presenting every
    /// element as a literal.
    #[must_use]
    pub fn target_captures(&self, plan: &[PlanElement]) -> Vec<TargetCapture> {
        match &self.target_discipline {
            TargetDiscipline::Neutral => Vec::new(),
            TargetDiscipline::FixedTarget { target } => {
                vec![TargetCapture::Literal(target.clone())]
            }
            TargetDiscipline::ArgvFlag { flag, words } => {
                let mut captures = Vec::new();
                let mut elements = plan.iter().peekable();
                while let Some(element) = elements.next() {
                    if !matches!(element, PlanElement::Literal(text) if text == flag) {
                        continue;
                    }
                    match elements.peek() {
                        Some(PlanElement::Literal(value)) => {
                            captures
                                .push(TargetCapture::Literal(Target::new(words.normalize(value))));
                        }
                        // The role is present but its value is computed: the
                        // requirement exists, its target is decided at run
                        // time.
                        Some(PlanElement::Computed) => captures.push(TargetCapture::Computed),
                        // A trailing flag with no value is the tool's own
                        // command-validation failure, not a capture.
                        None => {}
                    }
                }
                captures
            }
            TargetDiscipline::EnvRoles {
                os_role,
                arch_role,
                os_words,
                arch_words,
                template,
            } => {
                // Scan the leading assignment region. A computed element in
                // that region could itself be an assignment to a role, so it
                // poisons static knowledge: the capture degrades to Computed
                // rather than silently vanishing.
                let mut os = None;
                let mut arch = None;
                let mut saw_computed = false;
                for element in plan {
                    match element {
                        PlanElement::Literal(text) => {
                            let Some((name, value)) = parse_assignment(text) else {
                                break;
                            };
                            if name == os_role {
                                os = Some(value.to_owned());
                            } else if name == arch_role {
                                arch = Some(value.to_owned());
                            }
                        }
                        PlanElement::Computed => {
                            saw_computed = true;
                            break;
                        }
                    }
                }
                match (os, arch) {
                    (Some(os), Some(arch)) => {
                        let target = render_template(
                            template,
                            os_words.normalize(&os),
                            arch_words.normalize(&arch),
                        );
                        vec![TargetCapture::Literal(Target::new(target))]
                    }
                    // A partial assignment (one role, the other defaulting to
                    // the tool's host detection) or a computed leading element
                    // is a target decided at run time.
                    (None, None) if !saw_computed => Vec::new(),
                    _ => vec![TargetCapture::Computed],
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Vec<CapabilityPackage> {
        packages_from_toml(DEFAULT_PACKAGES_TOML).expect("the shipped document parses")
    }

    fn shipped(name: &str) -> CapabilityPackage {
        defaults()
            .into_iter()
            .find(|package| package.name == name)
            .expect("the shipped document declares this package")
    }

    /// The format has to be able to say everything the retired compiled-in
    /// table said. This is that table, spelled out, so a regression in the
    /// document or the parser shows up as a disagreement here rather than as a
    /// tool that quietly stops imposing its requirement.
    #[test]
    fn the_shipped_document_says_what_the_compiled_in_table_said() {
        let packages = defaults();
        let names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["Echo", "Sh", "ProgressiveSh", "Rustc", "Go", "MingwGcc"]
        );
        assert_eq!(
            shipped("ProgressiveSh").protocol,
            ExecOutputProtocol::ProgressiveLinesV1
        );
        assert_eq!(shipped("Echo").target_discipline, TargetDiscipline::Neutral);
        assert_eq!(
            shipped("Rustc").target_discipline,
            TargetDiscipline::ArgvFlag {
                flag: "--target".to_owned(),
                words: WordTable::identity(),
            }
        );
        assert_eq!(
            shipped("MingwGcc").target_discipline,
            TargetDiscipline::FixedTarget {
                target: Target::new("x86_64-pc-windows-gnu"),
            }
        );
    }

    /// The dialects the retired `normalize` function pointers implemented,
    /// checked through the data that replaced them.
    #[test]
    fn the_word_tables_normalize_what_the_function_pointers_did() {
        let literal = |text: &str| PlanElement::Literal(text.to_owned());
        let rustc = shipped("Rustc");
        assert_eq!(
            rustc.target_captures(&[literal("--target"), literal("aarch64-apple-darwin")]),
            [TargetCapture::Literal(Target::new("aarch64-apple-darwin"))],
            "rustc already speaks the shared vocabulary: the empty table is identity"
        );
        let go = shipped("Go");
        for (os, arch, expected) in [
            ("linux", "amd64", "x86_64-unknown-linux-gnu"),
            ("windows", "arm64", "aarch64-pc-windows-gnu"),
            ("darwin", "386", "i686-apple-darwin"),
        ] {
            assert_eq!(
                go.target_captures(&[
                    literal(&format!("GOOS={os}")),
                    literal(&format!("GOARCH={arch}")),
                    literal("build"),
                ]),
                [TargetCapture::Literal(Target::new(expected))],
                "GOOS={os} GOARCH={arch}"
            );
        }
    }

    /// A word the table does not map passes through rather than acquiring an
    /// invented spelling — the package says what it knows and no more.
    #[test]
    fn an_unmapped_dialect_word_passes_through() {
        let go = shipped("Go");
        let captures = go.target_captures(&[
            PlanElement::Literal("GOOS=plan9".to_owned()),
            PlanElement::Literal("GOARCH=amd64".to_owned()),
        ]);
        assert_eq!(
            captures,
            [TargetCapture::Literal(Target::new("x86_64-plan9"))]
        );
    }

    /// A dialect word is the tool's, and it does not get to name a template
    /// placeholder. Chained replaces let `GOARCH` decide where the OS goes,
    /// which fabricates a triple the manifest then compares against.
    #[test]
    fn a_dialect_word_cannot_inject_a_placeholder() {
        let go = shipped("Go");
        let captures = go.target_captures(&[
            PlanElement::Literal("GOOS={arch}".to_owned()),
            PlanElement::Literal("GOARCH=amd64".to_owned()),
        ]);
        assert_eq!(
            captures,
            [TargetCapture::Literal(Target::new("x86_64-{arch}"))],
            "the unmapped word passes through as itself, placeholder-looking or not"
        );
    }

    /// The point of the whole exercise: a tool nothing in this crate has heard
    /// of is nameable, with a working grammar, without touching Rust.
    #[test]
    fn a_tool_this_crate_never_heard_of_is_a_document() {
        let packages = packages_from_toml(
            r#"
[[package]]
name = "Quartus"
protocol = "exit-only"

[[package]]
name = "Xcodebuild"
protocol = "exit-only"
target = { kind = "argv-flag", flag = "-destination", words = { "generic/platform=iOS" = "aarch64-apple-ios" } }
"#,
        )
        .expect("an invoker's document parses");
        assert_eq!(packages[0].target_discipline, TargetDiscipline::Neutral);
        assert_eq!(
            packages[1].target_captures(&[
                PlanElement::Literal("-destination".to_owned()),
                PlanElement::Literal("generic/platform=iOS".to_owned()),
            ]),
            [TargetCapture::Literal(Target::new("aarch64-apple-ios"))]
        );
    }

    #[test]
    fn a_malformed_package_names_the_package_and_the_field() {
        let missing_flag = packages_from_toml(
            r#"
[[package]]
name = "Weird"
protocol = "exit-only"
target = { kind = "argv-flag" }
"#,
        )
        .expect_err("a flag-shaped package without a flag is not a package");
        assert!(
            missing_flag.contains("Weird") && missing_flag.contains("flag"),
            "{missing_flag}"
        );
        let bad_protocol = packages_from_toml(
            r#"
[[package]]
name = "Weird"
protocol = "telepathy"
"#,
        )
        .expect_err("an unknown protocol is not a protocol");
        assert!(
            bad_protocol.contains("Weird") && bad_protocol.contains("telepathy"),
            "{bad_protocol}"
        );
    }

    /// Registration is additive and idempotent, and a contradicting
    /// redefinition refuses rather than silently winning.
    #[test]
    fn registration_is_idempotent_and_refuses_contradictions() {
        let package = |protocol| CapabilityPackage {
            name: "RegistryProbe".to_owned(),
            protocol,
            target_discipline: TargetDiscipline::Neutral,
        };
        register_packages(vec![package(ExecOutputProtocol::ExitOnly)]).expect("first registration");
        register_packages(vec![package(ExecOutputProtocol::ExitOnly)])
            .expect("an identical registration is a no-op");
        let conflict = register_packages(vec![package(ExecOutputProtocol::ProgressiveLinesV1)])
            .expect_err("a contradicting redefinition refuses");
        assert_eq!(conflict.name, "RegistryProbe");
        assert_eq!(
            capability_package("RegistryProbe")
                .expect("registered")
                .protocol,
            ExecOutputProtocol::ExitOnly,
            "the refused registration did not overwrite the registered grammar"
        );
    }

    /// The nameable-type list and the package registry are two spellings of
    /// one set: a type a program can name must have a package (grammar,
    /// protocol), and a registered package must be nameable.
    ///
    /// The nameable side is read through [`crate::capability_types`] — the list
    /// the compiler is actually handed. Reading both sides off the registry
    /// would be a tautology, and this test exists to hold the two lists in
    /// agreement the way the retired hand-maintained pair was held.
    #[test]
    fn every_registered_package_is_nameable() {
        register_default_packages().expect("the shipped packages register");
        let nameable: Vec<&str> = crate::capability_types()
            .iter()
            .map(|decl| decl.name)
            .collect();
        for name in nameable.iter().copied() {
            assert!(
                capability_package(name).is_some(),
                "`{name}` is nameable but has no package"
            );
        }
        for shipped in defaults() {
            assert!(
                nameable.contains(&shipped.name.as_str()),
                "shipped package `{}` is not nameable",
                shipped.name
            );
        }
    }
}
