//! Successful value provenance.

use std::sync::Arc;

/// Where a successful value originally came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Provenance {
    pub root: ProvenanceRoot,
    pub path: Arc<[ProjectionStep]>,
}

impl Provenance {
    pub fn unknown() -> Self {
        Self {
            root: ProvenanceRoot::Unknown,
            path: Arc::from([]),
        }
    }

    pub fn literal() -> Self {
        Self {
            root: ProvenanceRoot::Literal,
            path: Arc::from([]),
        }
    }

    pub fn binding(name: impl Into<Arc<str>>) -> Self {
        Self {
            root: ProvenanceRoot::Binding { name: name.into() },
            path: Arc::from([]),
        }
    }

    pub fn runtime_input(kind: RuntimeInputKind, label: impl Into<Arc<str>>) -> Self {
        Self {
            root: ProvenanceRoot::RuntimeInput {
                kind,
                label: label.into(),
            },
            path: Arc::from([]),
        }
    }

    pub fn host_probe(host: impl Into<Arc<str>>, description: impl Into<Arc<str>>) -> Self {
        Self {
            root: ProvenanceRoot::HostProbe {
                host: host.into(),
                description: description.into(),
            },
            path: Arc::from([]),
        }
    }

    pub fn project(&self, step: ProjectionStep) -> Self {
        let mut path = self.path.iter().cloned().collect::<Vec<_>>();
        path.push(step);
        Self {
            root: self.root.clone(),
            path: Arc::from(path),
        }
    }
}

/// High-level origin of a value, before any projections.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProvenanceRoot {
    Unknown,
    Literal,
    Binding {
        name: Arc<str>,
    },
    RuntimeInput {
        kind: RuntimeInputKind,
        label: Arc<str>,
    },
    HostProbe {
        host: Arc<str>,
        description: Arc<str>,
    },
}

/// Kinds of runtime inputs that can seed a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeInputKind {
    Argument,
    AmbientEnv,
    Import,
    LocalPath,
    DiscoverResult,
    ExecResult,
}

/// Projection applied to a value after its root origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectionStep {
    Field(Arc<str>),
    Index(usize),
    MapKey(Arc<str>),
    Path(Arc<str>),
}
