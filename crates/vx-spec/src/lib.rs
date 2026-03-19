//! Styx-backed evaluator spec fixtures.
//!
//! This crate is intentionally small and parsing-focused. It exists to let us
//! experiment with fixture shapes independently of `vx-eval`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use facet::Facet;
use facet_value::Value as DynamicValue;
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpecOutcome {
    Result(ObservedResult),
    Poison(ObservedPoison),
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub files: BTreeMap<String, String>,
}

impl SpecCase {
    pub fn parse(source: &str) -> Result<Self, SpecParseError> {
        let parsed = SpecDocument::parse(source)?;
        Ok(Self {
            path: PathBuf::from("<inline case>"),
            dir: PathBuf::from("."),
            main_source: parsed.main,
            invoke: parsed.invoke,
            expect: parsed.expect,
            files: parsed.files,
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
        Ok(Self {
            path: path.to_path_buf(),
            dir,
            main_source: parsed.main,
            invoke: parsed.invoke,
            expect: parsed.expect,
            files: parsed.files,
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
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct SpecDocument {
    pub main: String,
    pub invoke: InvokeSpec,
    pub expect: ExpectSpec,
    #[facet(default)]
    pub files: BTreeMap<String, String>,
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
    pub args: Vec<DynamicValue>,
    #[facet(default)]
    pub capabilities: Vec<String>,
    #[facet(default)]
    pub runtime: RuntimeSpec,
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
            EventAssertion::MustEmit(pattern) => actual.iter().any(|event| event == pattern),
            EventAssertion::MustNotEmit(pattern) => actual.iter().all(|event| event != pattern),
            EventAssertion::Before(OrderConstraint { first, then }) => {
                let first_index = actual.iter().position(|event| event == first);
                let then_index = actual.iter().position(|event| event == then);
                match (first_index, then_index) {
                    (Some(first_index), Some(then_index)) => first_index < then_index,
                    _ => false,
                }
            }
        })
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

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum EventPattern {
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

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum EngineExpectation {
    Pass,
    Todo,
    Skip,
}
