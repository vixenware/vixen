//! The machine manifest: what THIS machine offers, as declared typed data —
//! and the binding check that lets a program fail before anything runs
//! (`vix-core/docs/content/spec/vixen/machine.md`).
//!
//! The manifest replaces the runner's capability *conjuring*: until now the
//! demand root minted a capability value for whatever type a test named
//! (`publish_capability`, unconditionally), so nothing could fail. Now the
//! manifest is the single source of the machine's word — the host `Target`
//! plus one offer per capability type, each carrying the tool's program and
//! its facts as ordinary typed fields — and root capability parameters bind
//! against it by declared type before any island of the test is submitted.
//!
//! The requirement side is never spelled beside the code that implies it: it
//! is extracted from use through the capability package's command grammar
//! ([`vixen_primitives::capability_package`]) and normalized to [`Target`]
//! values — the manifest never learns a tool's dialect.
//!
//! r[impl vixen.machine.manifest]

use std::collections::BTreeMap;

use vix::vir::{Island, Module, Node, Op, PartitionedTest, Type};

use crate::version::{OrderedVersion, VersionRange};
use vixen_primitives::capability_package::{
    PlanElement, Target, TargetCapture, capability_package,
};

/// The triple this build of the runner is running on — the manifest's default
/// `host` fact. cfg-derived because no taxon-backed `Target` machinery exists
/// yet (see [`Target`]'s honest-stand-in note); the mapping covers the
/// platforms this workspace builds for.
#[must_use]
pub fn host_target() -> Target {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };
    let os = if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "windows") {
        "pc-windows-msvc"
    } else {
        "unknown-unknown"
    };
    Target::new(format!("{arch}-{os}"))
}

/// One capability offer: the machine's word for one capability type. The
/// tool's reference is a program path (host-trusting exactly as the exec
/// backend is, for 0.1); the rest are the capability's *facts* as typed
/// fields — machine-ness is never a set of booleans
/// (`vixen.machine.facts-are-fields`).
///
/// r[impl vixen.machine.facts-are-fields]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityOffer {
    /// The capability type's nominal name (`Sh`, `Rustc`, …).
    pub ty: String,
    /// The tool closure's reference: for 0.1 a program path/name.
    pub program: String,
    /// The toolchain version fact, when the tool has one.
    pub toolchain: Option<String>,
    /// The targets this toolchain can produce. Empty for a target-neutral
    /// tool, whose plans never impose a target requirement anyway.
    pub targets: Vec<Target>,
}

impl CapabilityOffer {
    /// Render the offer's facts for a diagnostic — the "machine offers" side.
    #[must_use]
    pub fn rendered(&self) -> String {
        let mut facts = Vec::new();
        if let Some(toolchain) = &self.toolchain {
            facts.push(format!("toolchain {toolchain}"));
        }
        if !self.targets.is_empty() {
            let targets = self
                .targets
                .iter()
                .map(Target::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            facts.push(format!("targets [{targets}]"));
        }
        if facts.is_empty() {
            self.ty.clone()
        } else {
            format!("{} {{ {} }}", self.ty, facts.join(", "))
        }
    }
}

/// A machine's capability set as a declared, typed value: the host `Target`
/// plus one offer per capability type. Nothing is discovered ambiently and
/// nothing is probed to mint identity — the embedder states this value (in
/// tests as a plain Rust value, or loaded as config via [`Self::from_toml`]),
/// and it is the single thing root capability parameters bind against.
///
/// r[impl vixen.machine.manifest]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineManifest {
    pub host: Target,
    pub capabilities: Vec<CapabilityOffer>,
}

impl MachineManifest {
    /// The manifest the ratchet harness runs under by default: the offers the
    /// runner used to conjure, now stated as data. The programs (`echo`,
    /// `sh`) are byte-identical to the parameter names the retired conjuring
    /// path used, so every existing capability value identity is preserved.
    #[must_use]
    pub fn ratchet_default() -> Self {
        let neutral = |ty: &str, program: &str| CapabilityOffer {
            ty: ty.to_owned(),
            program: program.to_owned(),
            toolchain: None,
            targets: Vec::new(),
        };
        Self {
            host: host_target(),
            capabilities: vec![
                neutral("Echo", "echo"),
                neutral("Sh", "sh"),
                neutral("ProgressiveSh", "sh"),
            ],
        }
    }

    /// The machine's offer for one capability type, by nominal name.
    #[must_use]
    pub fn offer(&self, ty: &str) -> Option<&CapabilityOffer> {
        self.capabilities.iter().find(|offer| offer.ty == ty)
    }

    /// Load a manifest from its TOML config spelling — the embedder-facing
    /// form of "the embedder loads the manifest as config". The document
    /// shape:
    ///
    /// ```toml
    /// host = "x86_64-unknown-linux-gnu"
    ///
    /// [[capability]]
    /// ty = "Rustc"
    /// program = "/toolchains/1.89.0/bin/rustc"
    /// toolchain = "1.89.0"
    /// targets = ["x86_64-unknown-linux-gnu"]
    /// ```
    pub fn from_toml(source: &str) -> Result<Self, String> {
        #[derive(facet::Facet)]
        struct OfferDoc {
            ty: String,
            program: String,
            toolchain: Option<String>,
            targets: Option<Vec<String>>,
        }
        #[derive(facet::Facet)]
        struct ManifestDoc {
            host: String,
            capability: Vec<OfferDoc>,
        }
        let doc: ManifestDoc = facet_toml::from_str(source).map_err(|error| error.to_string())?;
        Ok(Self {
            host: Target::new(doc.host),
            capabilities: doc
                .capability
                .into_iter()
                .map(|offer| CapabilityOffer {
                    ty: offer.ty,
                    program: offer.program,
                    toolchain: offer.toolchain,
                    targets: offer
                        .targets
                        .unwrap_or_default()
                        .into_iter()
                        .map(Target::new)
                        .collect(),
                })
                .collect(),
        })
    }

    /// Bind one test's requirement set against this manifest. Every
    /// unsatisfiable requirement — the type absent, a required `Target` the
    /// offered value's facts lack, or a toolchain the offer's stated version
    /// falls outside — is returned as a typed refusal naming both sides. The
    /// caller raises them BEFORE submitting any island, so no process spawns
    /// and no demand parks. A [`TargetRequirement::Computed`] requirement is
    /// not checkable at bind time and imposes nothing here — the static report
    /// already degraded it honestly.
    ///
    /// r[impl vixen.machine.binding-fails-before-effects]
    /// r[impl vixen.machine.version-pin]
    #[must_use]
    pub fn bind(&self, requirements: &TestRequirements) -> Vec<CapabilityRefusal> {
        let mut refusals = Vec::new();
        for capability in &requirements.capabilities {
            let refuse =
                |cause: RefusalCause, offered: Option<&CapabilityOffer>| CapabilityRefusal {
                    test: requirements.test.clone(),
                    parameter: capability.parameter.clone(),
                    required_type: capability.ty.clone(),
                    cause,
                    offered: offered.map(CapabilityOffer::rendered),
                };
            let Some(offer) = self.offer(&capability.ty) else {
                refusals.push(refuse(RefusalCause::TypeAbsent, None));
                continue;
            };
            for requirement in &capability.targets {
                let required = match requirement {
                    TargetRequirement::Literal(target) => target,
                    TargetRequirement::Computed => continue,
                };
                if !offer.targets.contains(required) {
                    refusals.push(refuse(
                        RefusalCause::Target {
                            required: required.as_str().to_owned(),
                        },
                        Some(offer),
                    ));
                }
            }
            for pin in &capability.toolchain_pins {
                if let Some(cause) = toolchain_refusal(pin, offer.toolchain.as_deref()) {
                    refusals.push(refuse(cause, Some(offer)));
                }
            }
        }
        refusals
    }
}

/// The environment variable through which an invoker DECLARES the machine
/// manifest file: an explicit path, read once at the embedder entrypoint.
/// This is a declaration, not discovery — no path is probed, no directory
/// walked, no fallback location tried (`vixen.machine.manifest`: a machine's
/// word is stated, never conjured from its surroundings).
pub const MANIFEST_ENV: &str = "VIX_MACHINE_MANIFEST";

/// A typed manifest-loading failure. A declared file that cannot be read or
/// parsed is THIS error at the entrypoint — never a silent fall-back to the
/// harness default, which would run the program under a machine word the
/// invoker explicitly replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestLoadError {
    /// The declared path could not be read.
    Unreadable { path: String, detail: String },
    /// The declared file read, but is not a valid manifest document.
    Malformed { path: String, detail: String },
}

impl core::fmt::Display for ManifestLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable { path, detail } => write!(
                f,
                "error[manifest]: declared machine manifest `{path}` cannot be read: {detail}"
            ),
            Self::Malformed { path, detail } => write!(
                f,
                "error[manifest]: declared machine manifest `{path}` is not a manifest document: {detail}"
            ),
        }
    }
}

/// Load the manifest an explicit path declares: read the file, parse it as
/// the [`MachineManifest::from_toml`] document. Both failure sides are loud
/// and typed, naming the declared path.
pub fn load_manifest(path: &str) -> Result<MachineManifest, ManifestLoadError> {
    let source = std::fs::read_to_string(path).map_err(|error| ManifestLoadError::Unreadable {
        path: path.to_owned(),
        detail: error.to_string(),
    })?;
    MachineManifest::from_toml(&source).map_err(|detail| ManifestLoadError::Malformed {
        path: path.to_owned(),
        detail,
    })
}

/// Resolve this process's machine word: the manifest file
/// [`MANIFEST_ENV`] explicitly declares, or
/// [`MachineManifest::ratchet_default`] when nothing is declared. A declared
/// file that is missing, unreadable, malformed — or a declared path that is
/// not even UTF-8 — is a loud typed error; the default serves only the
/// UNDECLARED case.
pub fn declared_manifest() -> Result<MachineManifest, ManifestLoadError> {
    match std::env::var_os(MANIFEST_ENV) {
        Some(path) => {
            let path = path.to_str().ok_or_else(|| ManifestLoadError::Unreadable {
                path: path.to_string_lossy().into_owned(),
                detail: "the declared path is not valid UTF-8".to_owned(),
            })?;
            load_manifest(path)
        }
        None => Ok(MachineManifest::ratchet_default()),
    }
}

/// Compare one demanded version pin against the machine's word for that
/// capability, returning the refusal when they do not meet.
///
/// Both sides must READ as versions before they can be compared, and a side
/// that does not is its own refusal rather than a silent pass. This is
/// attribution, never verification (`vixen.machine.version-pin`): nothing here
/// asks the tool what it is — the manifest's `toolchain` is a human's statement
/// about the machine, and all this does is hold the program's demand and that
/// statement up against each other so a mismatch is somebody's, on the record.
fn toolchain_refusal(pin: &ToolchainPin, stated: Option<&str>) -> Option<RefusalCause> {
    match pin {
        // Exact: no parsing on either side, which is the whole point. A tool
        // whose version is `22.1std` can still be pinned, and a machine that
        // states something unreadable can still satisfy it.
        ToolchainPin::Exact(wanted) => {
            let Some(stated) = stated else {
                return Some(RefusalCause::ToolchainUnstated {
                    pin: wanted.clone(),
                });
            };
            (stated != wanted).then(|| RefusalCause::Toolchain {
                pin: wanted.clone(),
                stated: stated.to_owned(),
            })
        }
        ToolchainPin::Range(text) => {
            // The pin is read BEFORE the machine's word, so a malformed range
            // — a source bug, wrong on every machine — is not reported as this
            // particular machine failing to measure up.
            let range = match VersionRange::parse(text) {
                Ok(range) => range,
                Err(error) => {
                    return Some(RefusalCause::UnreadablePin {
                        pin: text.clone(),
                        detail: error.to_string(),
                    });
                }
            };
            let Some(stated) = stated else {
                return Some(RefusalCause::ToolchainUnstated { pin: text.clone() });
            };
            let Some(version) = OrderedVersion::parse(stated) else {
                return Some(RefusalCause::UnorderableToolchain {
                    pin: text.clone(),
                    stated: stated.to_owned(),
                });
            };
            (!range.matches(&version)).then(|| RefusalCause::Toolchain {
                pin: text.clone(),
                stated: stated.to_owned(),
            })
        }
    }
}

/// One requirement a test's plans impose on a capability type, in the shared
/// vocabulary — `Target` values, never tool strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetRequirement {
    /// A literal role capture: statically known, checked at bind time.
    Literal(Target),
    /// A capture whose value is computed at run time. The static report
    /// degrades honestly to "target decided at run time"; bind time cannot
    /// check it.
    Computed,
}

/// One capability parameter's requirement row: presence (the declared type),
/// every target requirement extracted from the test's plans, and every version
/// pin its declaring parameters spell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityRequirement {
    /// The declaring parameter's name (diagnostic vocabulary only — binding
    /// is by type).
    pub parameter: String,
    /// The capability type's nominal name.
    pub ty: String,
    pub targets: Vec<TargetRequirement>,
    /// Every toolchain pin declared for this type. Binding is nominal, so two
    /// parameters of one type bind to one offer; both pins are kept and both
    /// are checked, because a demand the author wrote down that nothing
    /// compares is worse than no demand at all.
    pub toolchain_pins: Vec<ToolchainPin>,
}

/// What a parameter demands of the offer's stated toolchain, and therefore
/// which question gets asked of it.
///
/// The two are separate spellings rather than one that guesses, because the
/// guess has no safe default: reading `">=1.89"` as an exact version yields a
/// pin nothing can ever satisfy, and reading `22.1std` as a range yields a
/// comparison nobody can perform.
#[derive(facet::Facet, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ToolchainPin {
    /// `toolchain: "22.1std"` — string equality against the machine's word,
    /// parsing neither side.
    Exact(String),
    /// `toolchain_range: ">=1.89, <1.90"` — an ordering question, which both
    /// sides must be able to answer.
    Range(String),
}

impl ToolchainPin {
    /// The pin as authored, whichever question it asks.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Exact(pin) | Self::Range(pin) => pin,
        }
    }
}

/// One test's requirement set, readable without executing anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestRequirements {
    pub test: String,
    pub capabilities: Vec<CapabilityRequirement>,
}

/// Why one capability parameter could not bind. Exactly one thing went wrong
/// per refusal — a target the offer lacks and a toolchain outside the pin are
/// two refusals, not one carrying two half-filled fields.
#[derive(facet::Facet, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RefusalCause {
    /// The manifest has no offer of the required type at all.
    TypeAbsent,
    /// The offer cannot produce a `Target` the program's plans require.
    Target { required: String },
    /// The machine's stated toolchain is outside the demanded version set.
    Toolchain { pin: String, stated: String },
    /// The program pins a toolchain and the offer states none, so there is
    /// nothing to attribute the demand against.
    ToolchainUnstated { pin: String },
    /// The demanded range could not be read as one.
    UnreadablePin { pin: String, detail: String },
    /// A range was demanded and the machine's stated toolchain cannot be put on
    /// a number line — `>=1.89` against `22.1std`. Refused rather than answered
    /// "no": saying no would imply somebody had performed the comparison.
    UnorderableToolchain { pin: String, stated: String },
}

/// A typed pre-effect refusal: the vixen half of
/// `machine.primitive.capabilities-by-identity`'s admissibility sentence,
/// naming both sides. `offered: None` means the type is absent from the
/// manifest entirely.
///
/// r[impl vixen.machine.binding-fails-before-effects]
#[derive(facet::Facet, Clone, Debug, PartialEq, Eq)]
pub struct CapabilityRefusal {
    pub test: String,
    pub parameter: String,
    /// What the program requires: the capability type's nominal name…
    pub required_type: String,
    /// …and what about the offer failed to meet it.
    pub cause: RefusalCause,
    /// What the machine offers for that type, facts rendered; `None` when the
    /// manifest has no offer of the type.
    pub offered: Option<String>,
}

impl CapabilityRefusal {
    /// The required `Target`, when the refusal is a target refusal — the
    /// vocabulary the diagnostic and the acceptance tests read it in.
    #[must_use]
    pub fn required_target(&self) -> Option<&str> {
        match &self.cause {
            RefusalCause::Target { required } => Some(required),
            _ => None,
        }
    }
}

impl core::fmt::Display for CapabilityRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "error[capability]: `{}` demands {}",
            self.test, self.required_type
        )?;
        match &self.cause {
            RefusalCause::TypeAbsent => {}
            RefusalCause::Target { required } => write!(f, " producing {required}")?,
            RefusalCause::Toolchain { pin, .. }
            | RefusalCause::ToolchainUnstated { pin }
            | RefusalCause::UnreadablePin { pin, .. }
            | RefusalCause::UnorderableToolchain { pin, .. } => write!(f, " toolchain {pin}")?,
        }
        // An unreadable pin is a property of the SOURCE — it is wrong on every
        // machine — so what this one happens to offer is not evidence and
        // naming it would read as though the machine were implicated. Every
        // other cause is genuinely about this machine's word.
        if !matches!(self.cause, RefusalCause::UnreadablePin { .. }) {
            match &self.offered {
                Some(offered) => write!(f, "\n  machine offers: {offered}")?,
                None => write!(f, "\n  machine offers no {}", self.required_type)?,
            }
        }
        // The pin cases say what the machine's word WAS, because that is the
        // attribution: "we asked for this, they said that".
        match &self.cause {
            RefusalCause::Toolchain { stated, .. } => {
                write!(f, "\n  the machine states toolchain {stated}")?;
            }
            RefusalCause::ToolchainUnstated { .. } => {
                write!(
                    f,
                    "\n  the machine states no toolchain, so the pin cannot be attributed"
                )?;
            }
            RefusalCause::UnreadablePin { detail, .. } => {
                write!(
                    f,
                    "\n  that is not a version range: {detail}\
                     \n  this is the source, not the machine — no machine could satisfy it"
                )?;
            }
            RefusalCause::UnorderableToolchain { stated, .. } => {
                write!(
                    f,
                    "\n  the machine states toolchain {stated}, which has no ordering, \
                     so a range cannot be compared against it; pin it exactly with \
                     `toolchain: \"{stated}\"` if that is the one you mean"
                )?;
            }
            RefusalCause::TypeAbsent | RefusalCause::Target { .. } => {}
        }
        write!(f, "\n  no effect was started")
    }
}

/// The nominal name of a capability parameter's declared record type.
#[must_use]
pub fn capability_type_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Record(record) => Some(&record.name),
        _ => None,
    }
}

/// The static requirement report for every test of a compiled module: the
/// root's capability parameter types plus every literal role capture its
/// plans contain, with computed captures degraded honestly. Nothing is
/// executed, demanded, or interned to produce this.
///
/// r[impl vixen.machine.requirements-are-static]
pub fn static_requirements(
    module: &Module,
) -> Result<Vec<TestRequirements>, vix::diagnostic::Diagnostics> {
    module
        .tests
        .iter()
        .map(|test| Ok(test_requirements(&module.try_partition_test(test)?)))
        .collect()
}

/// One partitioned test's requirement set, extracted from use: presence from
/// the declared capability parameters, target requirements from every exec
/// plan's role captures under the owning package's command grammar
/// (`vixen.machine.requirements-from-use`). Literal captures are read
/// directly off the partitioned VIR — this is partition-time knowledge, ahead
/// of bind time and of any execution.
///
/// r[impl vixen.machine.requirements-from-use]
#[must_use]
pub fn test_requirements(partitioned: &PartitionedTest) -> TestRequirements {
    // Presence rows, one per declared capability parameter, in declaration
    // order; target requirements accumulate per capability TYPE (binding is
    // nominal).
    let mut rows: Vec<CapabilityRequirement> = Vec::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    for capability in &partitioned.capabilities {
        let Some(ty) = capability_type_name(&capability.ty) else {
            continue;
        };
        let row = *by_type.entry(ty.to_owned()).or_insert_with(|| {
            rows.push(CapabilityRequirement {
                parameter: capability.name.clone(),
                ty: ty.to_owned(),
                targets: Vec::new(),
                toolchain_pins: Vec::new(),
            });
            rows.len() - 1
        });
        // Binding is nominal, so a second parameter of the same type folds into
        // the row the first opened — but its pin is its own demand and joins
        // the list rather than replacing or being dropped. The two spellings
        // are mutually exclusive per parameter (the compiler refuses both), so
        // at most one of these adds anything for a given declaration.
        let declared = [
            capability.constraints.toolchain.as_ref().map(|pin| ToolchainPin::Exact(pin.clone())),
            capability
                .constraints
                .toolchain_range
                .as_ref()
                .map(|pin| ToolchainPin::Range(pin.clone())),
        ];
        for pin in declared.into_iter().flatten() {
            if !rows[row].toolchain_pins.contains(&pin) {
                rows[row].toolchain_pins.push(pin);
            }
        }
    }
    // Every exec plan in the test, wherever it was partitioned to.
    let mut islands: Vec<&Island> = Vec::new();
    islands.extend(partitioned.values.iter().map(|value| &value.island));
    islands.extend(partitioned.wire_islands.iter().map(|value| &value.island));
    islands.extend(partitioned.islands.iter());
    islands.extend(partitioned.generator.iter());
    for island in islands {
        collect_exec_requirements(&island.nodes, &mut rows, &mut by_type);
        for callee in &island.callees {
            collect_exec_requirements(&callee.nodes, &mut rows, &mut by_type);
        }
    }
    TestRequirements {
        test: partitioned.name.clone(),
        capabilities: rows,
    }
}

/// Scan one node vector for exec invocations and fold each plan's target
/// captures into the requirement rows. The capability type is read from the
/// request record's capability field type; the plan elements are classified
/// literal (`Op::String`) or computed, and the owning package's grammar does
/// the extraction — this function knows no dialect.
fn collect_exec_requirements(
    nodes: &[Node],
    rows: &mut Vec<CapabilityRequirement>,
    by_type: &mut BTreeMap<String, usize>,
) {
    let exec = vix::runtime::exec_primitive_id();
    let node_by_id = |id: vix::vir::NodeId| nodes.iter().find(|node| node.id == id);
    for node in nodes {
        let Op::InvokePrimitive { primitive } = &node.op else {
            continue;
        };
        if *primitive != exec {
            continue;
        }
        // An ABSENT request node is not an invariant break: this scan runs per
        // island, and partitioning decides which island holds which node, so a
        // request the current vector cannot see is one another vector carries.
        // Skipping is right here. A request node that IS present with the wrong
        // arity is a different claim — see the assert below.
        let Some(request) = node.inputs.first().copied().and_then(node_by_id) else {
            continue;
        };
        // `{capability, argv, mounts, env}` — the mounts and the declared env
        // are inputs TO the process, not part of the plan the command grammar
        // extracts targets from, so the requirement scan reads past both.
        //
        // Reading past the env is safe rather than merely convenient: a package
        // whose grammar carries target roles in the environment names those
        // roles, and `exec_primitive::compose_env` refuses a declared
        // assignment to any of them. So a target requirement cannot enter
        // through the `where` clause behind this scan's back — it is refused at
        // the seam where the package's vocabulary is known.
        //
        // A shape mismatch here is an INVARIANT BREAK, not a case to skip: the
        // node is already a confirmed exec-primitive invocation, so its request
        // is the one `lower_exec` built. Skipping quietly is how this scan
        // reported "no requirements" for programs that had them when the mounts
        // field landed — a refusal that never happens. Fail where the cause is.
        assert!(
            request.inputs.len() == 4,
            "an exec request has {{capability, argv, mounts, env}}; found {} inputs. \
             A request that grows a field must teach this scan what it means.",
            request.inputs.len()
        );
        let [capability_id, argv_id, _mounts_id, _env_id] = request.inputs.as_slice() else {
            unreachable!("arity asserted above");
        };
        let Some(ty) =
            node_by_id(*capability_id).and_then(|capability| capability_type_name(&capability.ty))
        else {
            continue;
        };
        let Some(package) = capability_package(ty) else {
            continue;
        };
        let plan: Vec<PlanElement> = match node_by_id(*argv_id) {
            Some(argv) if matches!(argv.op, Op::Array) => argv
                .inputs
                .iter()
                .map(|element| match node_by_id(*element).map(|node| &node.op) {
                    Some(Op::String(text)) => PlanElement::Literal(text.clone()),
                    _ => PlanElement::Computed,
                })
                .collect(),
            // A plan whose argv is not structurally visible here is wholly
            // computed — degrade, never guess.
            _ => vec![PlanElement::Computed],
        };
        let captures = package.target_captures(&plan);
        if captures.is_empty() {
            continue;
        }
        let row = *by_type.entry(ty.to_owned()).or_insert_with(|| {
            rows.push(CapabilityRequirement {
                parameter: ty.to_owned(),
                ty: ty.to_owned(),
                targets: Vec::new(),
                toolchain_pins: Vec::new(),
            });
            rows.len() - 1
        });
        for capture in captures {
            let requirement = match capture {
                TargetCapture::Literal(target) => TargetRequirement::Literal(target),
                TargetCapture::Computed => TargetRequirement::Computed,
            };
            if !rows[row].targets.contains(&requirement) {
                rows[row].targets.push(requirement);
            }
        }
    }
}
