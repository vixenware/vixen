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
    pub executors: BTreeMap<String, ExecutorSpec>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ReadEntry {
    pub path: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct HashEntry {
    pub path: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Facet)]
pub struct ExpectSpec {
    pub result: Option<DynamicValue>,
    pub poison: Option<PoisonPattern>,
    #[facet(default)]
    pub obligations: Vec<ObservationPattern>,
    #[facet(default)]
    pub events: Vec<EventAssertion>,
    #[facet(default)]
    pub engines: BTreeMap<String, EngineExpectation>,
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
