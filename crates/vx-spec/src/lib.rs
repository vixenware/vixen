//! Styx-backed evaluator spec fixtures.
//!
//! This crate is intentionally small and parsing-focused. It exists to let us
//! experiment with fixture shapes independently of any particular evaluator
//! implementation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use facet::Facet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpecParseError {
    #[error("failed to read {path}: {source}")]
    ReadError {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {message}")]
    ParseError { path: String, message: String },

    #[error("invalid spec {path}: {message}")]
    ValidationError { path: String, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpecOutcome {
    Result(ObservedResult),
    Poison(ObservedPoison),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservedPoison {
    pub kind: String,
    pub message: String,
    pub events: Vec<EventPattern>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservedResult {
    pub value: SpecValue,
    pub provenance: Option<ProvenancePattern>,
    pub obligations: Vec<ObservationPattern>,
    pub reuse: Option<ReusePattern>,
    pub crossing: Option<CrossingPattern>,
    pub events: Vec<EventPattern>,
}

#[derive(Debug, Error)]
pub enum SpecAssertionError {
    #[error("expected result {expected:?}, but evaluation poisoned with {actual:?}")]
    ExpectedResultGotPoison {
        expected: SpecValue,
        actual: ObservedPoison,
    },

    #[error("expected poison {expected:?}, but evaluation returned {actual:?}")]
    ExpectedPoisonGotResult {
        expected: PoisonPattern,
        actual: SpecValue,
    },

    #[error("result mismatch: expected {expected:?}, got {actual:?}")]
    ResultMismatch {
        expected: SpecValue,
        actual: SpecValue,
    },

    #[error("unexpected poison {actual:?}")]
    UnexpectedPoison { actual: ObservedPoison },

    #[error("poison kind mismatch: expected {expected:?}, got {actual:?}")]
    PoisonKindMismatch { expected: String, actual: String },

    #[error("poison message mismatch: expected message containing {expected:?}, got {actual:?}")]
    PoisonMessageMismatch { expected: String, actual: String },

    #[error("provenance mismatch: expected {expected:?}, got {actual:?}")]
    ProvenanceMismatch {
        expected: ProvenancePattern,
        actual: Option<ProvenancePattern>,
    },

    #[error("obligations mismatch: expected {expected:?}, got {actual:?}")]
    ObligationsMismatch {
        expected: Vec<ObservationPattern>,
        actual: Vec<ObservationPattern>,
    },

    #[error("reuse mismatch: expected {expected:?}, got {actual:?}")]
    ReuseMismatch {
        expected: ReusePattern,
        actual: Option<ReusePattern>,
    },

    #[error("crossing mismatch: expected {expected:?}, got {actual:?}")]
    CrossingMismatch {
        expected: CrossingPattern,
        actual: Option<CrossingPattern>,
    },

    #[error("event assertions failed: expected {expected:?}, got {actual:?}")]
    EventMismatch {
        expected: Vec<EventAssertion>,
        actual: Vec<EventPattern>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EngineId {
    Eval1,
    Eval2,
}

impl EngineId {
    pub fn key(self) -> &'static str {
        match self {
            Self::Eval1 => "eval1",
            Self::Eval2 => "eval2",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpecCase {
    pub path: PathBuf,
    pub dir: PathBuf,
    pub main_source: String,
    pub invoke: InvokeSpec,
    pub expect: ExpectSpec,
    pub runs: Vec<SpecRun>,
    pub files: BTreeMap<String, String>,
}

impl SpecCase {
    pub fn parse(source: &str) -> Result<Self, SpecParseError> {
        let parsed = SpecDocument::parse(source)?;
        let files = resolve_spec_files(
            Path::new("."),
            "<inline case>",
            &parsed.files,
            &parsed.file_paths,
        )?;
        Ok(Self {
            path: PathBuf::from("<inline case>"),
            dir: PathBuf::from("."),
            main_source: parsed.main,
            invoke: parsed.invoke,
            expect: parsed.expect,
            runs: parsed.runs,
            files,
        })
    }

    pub fn from_path(path: &Path) -> Result<Self, SpecParseError> {
        let dir = path
            .parent()
            .ok_or_else(|| SpecParseError::ReadError {
                path: path.display().to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "spec case must live inside a directory",
                ),
            })?
            .to_path_buf();
        let source = std::fs::read_to_string(path).map_err(|source| SpecParseError::ReadError {
            path: path.display().to_string(),
            source,
        })?;
        let parsed = SpecDocument::parse_with_path(&source, path)?;
        let files = resolve_spec_files(
            &dir,
            &path.display().to_string(),
            &parsed.files,
            &parsed.file_paths,
        )?;
        Ok(Self {
            path: path.to_path_buf(),
            dir,
            main_source: parsed.main,
            invoke: parsed.invoke,
            expect: parsed.expect,
            runs: parsed.runs,
            files,
        })
    }

    pub fn expectation_for(&self, engine: EngineId) -> EngineExpectation {
        self.expect
            .engines
            .get(engine.key())
            .cloned()
            .unwrap_or(EngineExpectation::Pass)
    }

    pub fn assert_matches(&self, outcome: &SpecOutcome) -> Result<(), SpecAssertionError> {
        self.expect.assert_matches(outcome)
    }

    pub fn invocations(&self) -> Result<Vec<ResolvedSpecRun>, SpecParseError> {
        if self.runs.is_empty() {
            let invoke = InvokeOverrideSpec::default().resolve_against(&self.path, &self.invoke)?;
            return Ok(vec![ResolvedSpecRun {
                invoke,
                expect: self.expect.clone(),
            }]);
        }

        self.runs
            .iter()
            .map(|run| {
                Ok(ResolvedSpecRun {
                    invoke: run.invoke.resolve_against(&self.path, &self.invoke)?,
                    expect: run.expect.clone(),
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct SpecDocument {
    pub main: String,
    pub invoke: InvokeSpec,
    pub expect: ExpectSpec,
    #[facet(default)]
    pub runs: Vec<SpecRun>,
    #[facet(default)]
    pub files: BTreeMap<String, String>,
    #[facet(default)]
    pub file_paths: BTreeMap<String, String>,
}

impl SpecDocument {
    pub fn parse(source: &str) -> Result<Self, SpecParseError> {
        facet_styx::from_str(source).map_err(|source| SpecParseError::ParseError {
            path: "<inline case>".to_string(),
            message: source.to_string(),
        })
    }

    pub fn parse_with_path(source: &str, path: &Path) -> Result<Self, SpecParseError> {
        facet_styx::from_str(source).map_err(|source| SpecParseError::ParseError {
            path: path.display().to_string(),
            message: source.to_string(),
        })
    }
}

fn resolve_spec_files(
    base_dir: &Path,
    spec_path: &str,
    inline_files: &BTreeMap<String, String>,
    file_paths: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, SpecParseError> {
    let mut files = inline_files.clone();
    for (virtual_path, relative_path) in file_paths {
        if files.contains_key(virtual_path) {
            return Err(SpecParseError::ValidationError {
                path: spec_path.to_string(),
                message: format!(
                    "duplicate file entry for {virtual_path:?} across files and file_paths"
                ),
            });
        }
        let resolved_path = base_dir.join(relative_path);
        let contents = std::fs::read_to_string(&resolved_path).map_err(|source| {
            SpecParseError::ReadError {
                path: resolved_path.display().to_string(),
                source,
            }
        })?;
        files.insert(virtual_path.clone(), contents);
    }
    Ok(files)
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct InvokeSpec {
    pub entry: String,
    #[facet(default)]
    pub place: Option<String>,
    #[facet(default)]
    pub verify_place: Option<String>,
    #[facet(default)]
    pub replay_place: Option<String>,
    #[facet(default)]
    pub args: Vec<SpecValue>,
    #[facet(default)]
    pub capabilities: Vec<String>,
    #[facet(default)]
    pub slot: Option<SlotInvokeSpec>,
    #[facet(default)]
    pub runtime: RuntimeSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSpecRun {
    pub invoke: InvokeSpec,
    pub expect: ExpectSpec,
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct SpecRun {
    #[facet(default)]
    pub invoke: InvokeOverrideSpec,
    pub expect: ExpectSpec,
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct InvokeOverrideSpec {
    #[facet(default)]
    pub entry: Option<String>,
    #[facet(default)]
    pub place: Option<String>,
    #[facet(default)]
    pub verify_place: Option<String>,
    #[facet(default)]
    pub replay_place: Option<String>,
    #[facet(default)]
    pub args: Option<Vec<SpecValue>>,
    #[facet(default)]
    pub capabilities: Option<Vec<String>>,
    #[facet(default)]
    pub slot: Option<SlotInvokeOverrideSpec>,
    #[facet(default)]
    pub runtime: Option<RuntimeSpec>,
}

impl InvokeOverrideSpec {
    fn resolve_against(
        &self,
        path: &Path,
        base: &InvokeSpec,
    ) -> Result<InvokeSpec, SpecParseError> {
        let slot =
            resolve_slot_override(base.slot.as_ref(), self.slot.as_ref()).map_err(|message| {
                SpecParseError::ValidationError {
                    path: path.display().to_string(),
                    message,
                }
            })?;
        Ok(InvokeSpec {
            entry: self.entry.clone().unwrap_or_else(|| base.entry.clone()),
            place: self.place.clone().or_else(|| base.place.clone()),
            verify_place: self
                .verify_place
                .clone()
                .or_else(|| base.verify_place.clone()),
            replay_place: self
                .replay_place
                .clone()
                .or_else(|| base.replay_place.clone()),
            args: self.args.clone().unwrap_or_else(|| base.args.clone()),
            capabilities: self
                .capabilities
                .clone()
                .unwrap_or_else(|| base.capabilities.clone()),
            slot,
            runtime: self.runtime.clone().unwrap_or_else(|| base.runtime.clone()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct SlotInvokeSpec {
    pub id: String,
    pub domain: SlotDomainSpec,
    pub action: SlotActionSpec,
}

#[derive(Debug, Clone, PartialEq, Default, Facet)]
pub struct SlotInvokeOverrideSpec {
    #[facet(default)]
    pub id: Option<String>,
    #[facet(default)]
    pub domain: Option<SlotDomainSpec>,
    #[facet(default)]
    pub action: Option<SlotActionSpec>,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum SlotDomainSpec {
    Pure(PureSlotSpec),
    ObservedWhich(ObservedWhichSlotSpec),
    ToyResolver(ToyResolverSlotSpec),
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct PureSlotSpec {
    pub witness: SpecValue,
    pub value: SpecValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ObservedWhichSlotSpec {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct ToyResolverSlotSpec {
    pub witness: SpecValue,
    #[facet(default)]
    pub requirements: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum SlotActionSpec {
    Current,
    Refresh { intent: SpecValue },
}

fn resolve_slot_override(
    base: Option<&SlotInvokeSpec>,
    override_spec: Option<&SlotInvokeOverrideSpec>,
) -> Result<Option<SlotInvokeSpec>, String> {
    match (base, override_spec) {
        (None, None) => Ok(None),
        (Some(base), None) => validate_slot_spec(base.clone()).map(Some),
        (None, Some(override_spec)) => validate_slot_spec(SlotInvokeSpec {
            id: override_spec.id.clone().ok_or_else(|| {
                "slot override must provide id when no base slot exists".to_string()
            })?,
            domain: override_spec.domain.clone().ok_or_else(|| {
                "slot override must provide a domain when no base slot exists".to_string()
            })?,
            action: override_spec.action.clone().ok_or_else(|| {
                "slot override must provide an action when no base slot exists".to_string()
            })?,
        })
        .map(Some),
        (Some(base), Some(override_spec)) => validate_slot_spec(SlotInvokeSpec {
            id: override_spec.id.clone().unwrap_or_else(|| base.id.clone()),
            domain: override_spec
                .domain
                .clone()
                .unwrap_or_else(|| base.domain.clone()),
            action: override_spec
                .action
                .clone()
                .unwrap_or_else(|| base.action.clone()),
        })
        .map(Some),
    }
}

fn validate_slot_spec(slot: SlotInvokeSpec) -> Result<SlotInvokeSpec, String> {
    if slot.id.trim().is_empty() {
        return Err("slot id must be present and non-empty".to_string());
    }
    Ok(slot)
}

impl InvokeSpec {
    pub fn parse(source: &str) -> Result<Self, SpecParseError> {
        facet_styx::from_str(source).map_err(|source| SpecParseError::ParseError {
            path: "<inline invoke>".to_string(),
            message: source.to_string(),
        })
    }

    pub fn from_path(path: &Path) -> Result<Self, SpecParseError> {
        let source = std::fs::read_to_string(path).map_err(|source| SpecParseError::ReadError {
            path: path.display().to_string(),
            source,
        })?;
        facet_styx::from_str(&source).map_err(|source| SpecParseError::ParseError {
            path: path.display().to_string(),
            message: source.to_string(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct RuntimeSpec {
    #[facet(default)]
    pub places: BTreeMap<String, PlaceSpec>,
    #[facet(default)]
    pub executors: BTreeMap<String, ExecutorSpec>,
    #[facet(default)]
    pub package_registry: ToyPackageRegistrySpec,
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct ToyPackageRegistrySpec {
    #[facet(default)]
    pub packages: BTreeMap<String, ToyPackageSpec>,
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct ToyPackageSpec {
    #[facet(default)]
    pub versions: Vec<ToyPackageVersionSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ToyPackageVersionSpec {
    pub version: String,
    #[facet(default)]
    pub deps: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct PlaceSpec {
    #[facet(default)]
    pub host: HostSpec,
    #[facet(default)]
    pub files: BTreeMap<String, String>,
    #[facet(default)]
    pub tracked_files: BTreeMap<String, String>,
    #[facet(default)]
    pub opaque_bundles: BTreeMap<String, OpaqueBundleSpec>,
}

impl PlaceSpec {
    pub fn tracked_file_contents(&self, path: &str) -> Option<&str> {
        self.tracked_files
            .get(path)
            .or_else(|| self.files.get(path))
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct OpaqueBundleSpec {
    #[facet(default)]
    pub identity: Option<String>,
    #[facet(default)]
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExecutorSpec {
    Host(HostSpec),
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct HostSpec {
    pub os: String,
    pub arch: String,
    #[facet(default)]
    pub which: Vec<WhichEntry>,
    #[facet(default)]
    pub reads: Vec<ReadEntry>,
    #[facet(default)]
    pub hashes: Vec<HashEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct WhichEntry {
    pub name: String,
    pub path: String,
    #[facet(default)]
    pub bundle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ReadEntry {
    pub path: String,
    pub content_hash: String,
    #[facet(default)]
    pub bundle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct HashEntry {
    pub path: String,
    pub content_hash: String,
    #[facet(default)]
    pub bundle: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct ExpectSpec {
    pub result: Option<SpecValue>,
    pub poison: Option<PoisonPattern>,
    pub provenance: Option<ProvenancePattern>,
    #[facet(default)]
    pub obligations: Vec<ObservationPattern>,
    pub reuse: Option<ReusePattern>,
    pub crossing: Option<CrossingPattern>,
    #[facet(default)]
    pub events: Vec<EventAssertion>,
    #[facet(default)]
    pub engines: BTreeMap<String, EngineExpectation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ReusePattern {
    Stable,
    Observed,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum CrossingPattern {
    ByValue,
    ByProxy,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum SpecValue {
    Unit(()),
    Bool(bool),
    U64(u64),
    I64(i64),
    F64(f64),
    String(String),
    Array(Vec<SpecValue>),
    Map(BTreeMap<String, SpecValue>),
    Struct(StructSpec),
    External(ExternalSpec),
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct StructSpec {
    pub r#type: String,
    #[facet(default)]
    pub fields: BTreeMap<String, SpecValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExternalSpec {
    pub kind: String,
    pub descriptor: String,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ProvenancePattern {
    Unknown {
        #[facet(default)]
        source: Option<SourceSpanPattern>,
        #[facet(default)]
        path: Vec<ProjectionPattern>,
    },
    Literal {
        #[facet(default)]
        source: Option<SourceSpanPattern>,
        #[facet(default)]
        path: Vec<ProjectionPattern>,
    },
    Binding {
        name: String,
        #[facet(default)]
        source: Option<SourceSpanPattern>,
        #[facet(default)]
        path: Vec<ProjectionPattern>,
    },
    RuntimeInput {
        kind: RuntimeInputPattern,
        label: String,
        #[facet(default)]
        source: Option<SourceSpanPattern>,
        #[facet(default)]
        path: Vec<ProjectionPattern>,
    },
    HostProbe {
        host: String,
        description: String,
        #[facet(default)]
        source: Option<SourceSpanPattern>,
        #[facet(default)]
        path: Vec<ProjectionPattern>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct SourceSpanPattern {
    pub source_id: String,
    pub start: SourcePointPattern,
    pub end: SourcePointPattern,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct SourcePointPattern {
    pub byte: u32,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeInputPattern {
    Argument,
    AmbientEnv,
    Import,
    LocalPath,
    DiscoverResult,
    ExecResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ProjectionPattern {
    Field(String),
    Index(usize),
    MapKey(String),
    Path(String),
}

impl ExpectSpec {
    pub fn parse(source: &str) -> Result<Self, SpecParseError> {
        facet_styx::from_str(source).map_err(|source| SpecParseError::ParseError {
            path: "<inline expect>".to_string(),
            message: source.to_string(),
        })
    }

    pub fn from_path(path: &Path) -> Result<Self, SpecParseError> {
        let source = std::fs::read_to_string(path).map_err(|source| SpecParseError::ReadError {
            path: path.display().to_string(),
            source,
        })?;
        facet_styx::from_str(&source).map_err(|source| SpecParseError::ParseError {
            path: path.display().to_string(),
            message: source.to_string(),
        })
    }

    pub fn assert_matches(&self, outcome: &SpecOutcome) -> Result<(), SpecAssertionError> {
        match outcome {
            SpecOutcome::Result(actual) => {
                if let Some(expected) = &self.poison {
                    return Err(SpecAssertionError::ExpectedPoisonGotResult {
                        expected: expected.clone(),
                        actual: actual.value.clone(),
                    });
                }
                if let Some(expected) = &self.result
                    && &actual.value != expected
                {
                    return Err(SpecAssertionError::ResultMismatch {
                        expected: expected.clone(),
                        actual: actual.value.clone(),
                    });
                }
                if let Some(expected) = &self.provenance
                    && !expected.matches(actual.provenance.as_ref())
                {
                    return Err(SpecAssertionError::ProvenanceMismatch {
                        expected: expected.clone(),
                        actual: actual.provenance.clone(),
                    });
                }
                if !self.obligations.is_empty() && actual.obligations != self.obligations {
                    return Err(SpecAssertionError::ObligationsMismatch {
                        expected: self.obligations.clone(),
                        actual: actual.obligations.clone(),
                    });
                }
                if let Some(expected) = &self.reuse
                    && actual.reuse.as_ref() != Some(expected)
                {
                    return Err(SpecAssertionError::ReuseMismatch {
                        expected: expected.clone(),
                        actual: actual.reuse.clone(),
                    });
                }
                if let Some(expected) = &self.crossing
                    && actual.crossing.as_ref() != Some(expected)
                {
                    return Err(SpecAssertionError::CrossingMismatch {
                        expected: expected.clone(),
                        actual: actual.crossing.clone(),
                    });
                }
                if !self.events_match(&actual.events) {
                    return Err(SpecAssertionError::EventMismatch {
                        expected: self.events.clone(),
                        actual: actual.events.clone(),
                    });
                }
                Ok(())
            }
            SpecOutcome::Poison(actual) => {
                if let Some(expected) = &self.result {
                    return Err(SpecAssertionError::ExpectedResultGotPoison {
                        expected: expected.clone(),
                        actual: actual.clone(),
                    });
                }
                let Some(expected) = &self.poison else {
                    return Err(SpecAssertionError::UnexpectedPoison {
                        actual: actual.clone(),
                    });
                };
                if actual.kind != expected.kind {
                    return Err(SpecAssertionError::PoisonKindMismatch {
                        expected: expected.kind.clone(),
                        actual: actual.kind.clone(),
                    });
                }
                if let Some(message_contains) = &expected.message_contains
                    && !actual.message.contains(message_contains)
                {
                    return Err(SpecAssertionError::PoisonMessageMismatch {
                        expected: message_contains.clone(),
                        actual: actual.message.clone(),
                    });
                }
                if !self.events_match(&actual.events) {
                    return Err(SpecAssertionError::EventMismatch {
                        expected: self.events.clone(),
                        actual: actual.events.clone(),
                    });
                }
                Ok(())
            }
        }
    }

    fn events_match(&self, actual: &[EventPattern]) -> bool {
        self.events.iter().all(|assertion| match assertion {
            EventAssertion::MustEmit(pattern) => actual.iter().any(|event| pattern.matches(event)),
            EventAssertion::MustNotEmit(pattern) => {
                actual.iter().all(|event| !pattern.matches(event))
            }
            EventAssertion::Before(OrderConstraint { first, then }) => {
                let first_index = actual.iter().position(|event| first.matches(event));
                let then_index = actual.iter().position(|event| then.matches(event));
                match (first_index, then_index) {
                    (Some(first_index), Some(then_index)) => first_index < then_index,
                    _ => false,
                }
            }
        })
    }
}

impl EventPattern {
    fn matches(&self, actual: &EventPattern) -> bool {
        match (self, actual) {
            (
                Self::SlotPopulate {
                    slot: expected_slot,
                    reason: expected_reason,
                },
                Self::SlotPopulate {
                    slot: actual_slot,
                    reason: actual_reason,
                },
            )
            | (
                Self::SlotRewrite {
                    slot: expected_slot,
                    reason: expected_reason,
                },
                Self::SlotRewrite {
                    slot: actual_slot,
                    reason: actual_reason,
                },
            ) => expected_slot == actual_slot && expected_reason.matches(actual_reason),
            (Self::SlotReuse { slot: expected }, Self::SlotReuse { slot: actual }) => {
                expected == actual
            }
            _ => self == actual,
        }
    }
}

impl SlotEventReasonPattern {
    fn matches(&self, actual: &SlotEventReasonPattern) -> bool {
        match (self, actual) {
            (Self::Missing, Self::Missing) | (Self::WitnessChanged, Self::WitnessChanged) => true,
            (
                Self::ClaimsInvalid {
                    reason_contains: expected,
                },
                Self::ClaimsInvalid {
                    reason_contains: actual,
                },
            ) => match expected {
                Some(expected) => actual
                    .as_ref()
                    .is_some_and(|actual| actual.contains(expected)),
                None => true,
            },
            (Self::Refresh { intent: expected }, Self::Refresh { intent: actual }) => {
                expected == actual
            }
            _ => false,
        }
    }
}

impl ProvenancePattern {
    fn matches(&self, actual: Option<&ProvenancePattern>) -> bool {
        let Some(actual) = actual else {
            return false;
        };
        match (self, actual) {
            (
                Self::Unknown {
                    source: expected_source,
                    path: expected_path,
                },
                Self::Unknown {
                    source: actual_source,
                    path: actual_path,
                },
            )
            | (
                Self::Literal {
                    source: expected_source,
                    path: expected_path,
                },
                Self::Literal {
                    source: actual_source,
                    path: actual_path,
                },
            ) => expected_path == actual_path && source_matches(expected_source, actual_source),
            (
                Self::Binding {
                    name: expected_name,
                    source: expected_source,
                    path: expected_path,
                },
                Self::Binding {
                    name: actual_name,
                    source: actual_source,
                    path: actual_path,
                },
            ) => {
                expected_name == actual_name
                    && expected_path == actual_path
                    && source_matches(expected_source, actual_source)
            }
            (
                Self::RuntimeInput {
                    kind: expected_kind,
                    label: expected_label,
                    source: expected_source,
                    path: expected_path,
                },
                Self::RuntimeInput {
                    kind: actual_kind,
                    label: actual_label,
                    source: actual_source,
                    path: actual_path,
                },
            ) => {
                expected_kind == actual_kind
                    && expected_label == actual_label
                    && expected_path == actual_path
                    && source_matches(expected_source, actual_source)
            }
            (
                Self::HostProbe {
                    host: expected_host,
                    description: expected_description,
                    source: expected_source,
                    path: expected_path,
                },
                Self::HostProbe {
                    host: actual_host,
                    description: actual_description,
                    source: actual_source,
                    path: actual_path,
                },
            ) => {
                expected_host == actual_host
                    && expected_description == actual_description
                    && expected_path == actual_path
                    && source_matches(expected_source, actual_source)
            }
            _ => false,
        }
    }
}

fn source_matches(
    expected: &Option<SourceSpanPattern>,
    actual: &Option<SourceSpanPattern>,
) -> bool {
    match expected {
        Some(expected) => actual.as_ref() == Some(expected),
        None => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct PoisonPattern {
    pub kind: String,
    pub message_contains: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ObservationPattern {
    Os {
        value: String,
    },
    Arch {
        value: String,
    },
    Which {
        name: String,
        expected_path: Option<String>,
    },
    Bundle {
        name: String,
        identity: String,
    },
    Read {
        path: String,
        content_hash: String,
    },
    Hash {
        path: String,
        content_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum EventAssertion {
    MustEmit(EventPattern),
    MustNotEmit(EventPattern),
    Before(OrderConstraint),
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct OrderConstraint {
    pub first: EventPattern,
    pub then: EventPattern,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum EventPattern {
    SlotPopulate {
        slot: String,
        reason: SlotEventReasonPattern,
    },
    SlotReuse {
        slot: String,
    },
    SlotRewrite {
        slot: String,
        reason: SlotEventReasonPattern,
    },
    DiscoverStart {
        executor: String,
    },
    DiscoverEnd,
    DiscoverObserve {
        kind: String,
        name: Option<String>,
        path: Option<String>,
    },
    ExecStart,
    ExecVerifyOk,
    ExecVerifyFail,
    VerifyClaimsOk,
    VerifyClaimsFail,
    StabilizeHit,
    StabilizeMiss,
    Poison {
        kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum SlotEventReasonPattern {
    Missing,
    WitnessChanged,
    ClaimsInvalid { reason_contains: Option<String> },
    Refresh { intent: SpecValue },
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum EngineExpectation {
    Pass,
    Todo,
    Skip,
}

#[cfg(test)]
mod tests {
    use super::{SpecCase, SpecParseError};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn slot_override_without_base_id_is_rejected() {
        let case = SpecCase::parse(
            r#"
main <<VX,vx
fn build() {
  0
}
VX

invoke {
  entry build
}

runs (
  {
    invoke {
      slot {
        domain @pure{
          witness @string"w1"
          value @u64"1"
        }
        action @current
      }
    }

    expect {
      result @u64"1"
    }
  }
)
"#,
        )
        .expect("parse inline spec");

        let error = case.invocations().expect_err("missing slot id should fail");
        assert!(matches!(
            error,
            SpecParseError::ValidationError { message, .. }
            if message.contains("slot override must provide id")
        ));
    }

    #[test]
    fn empty_slot_id_is_rejected() {
        let case = SpecCase::parse(
            r#"
main <<VX,vx
fn build() {
  0
}
VX

invoke {
  entry build

  slot {
    id ""
    domain @pure{
      witness @string"w1"
      value @u64"1"
    }
    action @current
  }
}
"#,
        )
        .expect("parse inline spec");

        let error = case.invocations().expect_err("empty slot id should fail");
        assert!(matches!(
            error,
            SpecParseError::ValidationError { message, .. }
            if message.contains("slot id must be present and non-empty")
        ));
    }

    #[test]
    fn file_paths_load_relative_files_from_spec_directory() {
        let temp = tempdir().expect("temp dir");
        let dir = temp.path();
        fs::write(dir.join("helper.vx"), "fn helper() { 41 }\n").expect("write helper");
        fs::write(
            dir.join("case.styx"),
            r#"
main <<VX,vx
fn build() {
  helper()
}
VX

files {
  "extra.vx" <<VX,vx
fn extra() { 0 }
VX
}

file_paths {
  "helper.vx" "helper.vx"
}

invoke {
  entry build
}

expect {
  result @u64"41"
}
"#,
        )
        .expect("write case");

        let case = SpecCase::from_path(&dir.join("case.styx")).expect("load case");
        assert_eq!(
            case.files.get("helper.vx").map(String::as_str),
            Some("fn helper() { 41 }\n")
        );
        assert!(case.files.contains_key("extra.vx"));
    }

    #[test]
    fn file_paths_conflicts_with_inline_files_are_rejected() {
        let error = SpecCase::parse(
            r#"
main <<VX,vx
fn build() {
  0
}
VX

files {
  "helper.vx" <<VX,vx
fn helper() { 0 }
VX
}

file_paths {
  "helper.vx" "helper.vx"
}

invoke {
  entry build
}

expect {
  result @u64"0"
}
"#,
        )
        .expect_err("duplicate file key should fail");
        assert!(matches!(
            error,
            SpecParseError::ValidationError { message, .. }
            if message.contains("duplicate file entry")
        ));
    }
}
