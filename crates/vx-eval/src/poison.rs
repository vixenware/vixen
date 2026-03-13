//! Poison propagation.
//!
//! Poison is a value state, not an exception. When a node poisons, it
//! wakes all its waiters with the poison, and they propagate it to their
//! waiters in turn. Independent subgraphs are unaffected.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::instr::SourceProvenance;
use crate::node::NodeId;
use crate::transport::TransportSourceProvenance;
use crate::value::HostObservation;
use facet::Facet;

// r[impl error.poison]
/// The error carried by a poisoned node.
#[derive(Debug, Clone)]
pub struct PoisonError {
    pub kind: PoisonKind,
    pub message: String,
    /// The chain of demands that led to this poison — who demanded what.
    /// Most recent demand is last.
    pub demand_chain: DemandChain,
    /// Independent poison causes collected in deterministic input order.
    pub causes: Arc<[PoisonError]>,
    /// Human-facing boundary context (for example, the concrete build.vx exec request).
    contexts: Arc<[String]>,
    /// Hidden discovery observations that still matter if this poison is recovered via `?`.
    observations: Arc<[HostObservation]>,
}

/// What kind of failure caused the poison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoisonKind {
    /// A type error (wrong types for an operation).
    TypeError,
    /// An unresolved name reference.
    UnresolvedName,
    /// Division by zero or similar arithmetic failure.
    ArithmeticError,
    /// A capability violation (e.g., calling #fs without the capability).
    CapabilityViolation,
    /// An explicit `fail` in user code.
    UserFail,
    /// A verification failure from an `exec` verifier.
    VerificationFailure,
    /// Circular dependency detected.
    CircularDependency,
    /// The demand chain exceeded the configured maximum depth.
    DemandDepthExceeded,
    /// Multiple independent poisons were collected together.
    Multiple,
    /// Internal evaluator error (bug).
    Internal,
}

/// The demand chain: a trail of NodeIds showing who demanded what.
/// Used for debugging — this is Vixen's replacement for stack traces.
#[derive(Debug, Clone, Default)]
pub struct DemandChain {
    pub entries: Vec<DemandEntry>,
}

/// One entry in the demand chain.
#[derive(Debug, Clone)]
pub struct DemandEntry {
    pub node_id: NodeId,
    pub description: String,
    pub provenance: Option<SourceProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TransportDemandChain {
    pub entries: Vec<TransportDemandEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TransportDemandEntry {
    pub node_id: u64,
    pub description: String,
    pub provenance: Option<TransportSourceProvenance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum TransportPoisonKind {
    TypeError,
    UnresolvedName,
    ArithmeticError,
    CapabilityViolation,
    UserFail,
    VerificationFailure,
    CircularDependency,
    DemandDepthExceeded,
    Multiple,
    Internal,
}

#[derive(Debug, Clone, Facet)]
pub struct TransportPoisonError {
    pub kind: TransportPoisonKind,
    pub message: String,
    pub demand_chain: TransportDemandChain,
    pub causes: Vec<TransportPoisonError>,
    pub contexts: Vec<String>,
    pub observations: Vec<HostObservation>,
}

impl DemandChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(
        &mut self,
        node_id: NodeId,
        description: String,
        provenance: Option<SourceProvenance>,
    ) {
        self.entries.push(DemandEntry {
            node_id,
            description,
            provenance,
        });
    }
}

impl PoisonError {
    pub fn new(kind: PoisonKind, message: impl Into<String>, demand_chain: DemandChain) -> Self {
        Self {
            kind,
            message: message.into(),
            demand_chain,
            causes: Arc::from([]),
            contexts: Arc::from([]),
            observations: Arc::from([]),
        }
    }

    pub fn aggregate(
        message: impl Into<String>,
        demand_chain: DemandChain,
        errors: Vec<Self>,
    ) -> Self {
        let mut causes = Vec::new();
        let mut contexts = Vec::new();
        let mut observations = BTreeMap::new();
        for error in errors {
            if error.kind == PoisonKind::Multiple && !error.causes.is_empty() {
                contexts.extend(error.contexts.iter().cloned());
                for observation in error.observations.iter().cloned() {
                    observations.insert(observation.key(), observation);
                }
                causes.extend(error.causes.iter().cloned());
            } else {
                contexts.extend(error.contexts.iter().cloned());
                for observation in error.observations.iter().cloned() {
                    observations.insert(observation.key(), observation);
                }
                causes.push(error);
            }
        }

        if causes.len() == 1 {
            return causes
                .pop()
                .expect("checked len above")
                .with_observations(observations.into_values());
        }

        Self {
            kind: PoisonKind::Multiple,
            message: message.into(),
            demand_chain,
            causes: Arc::from(causes),
            contexts: Arc::from(contexts),
            observations: Arc::from(observations.into_values().collect::<Vec<_>>()),
        }
    }

    pub fn contexts(&self) -> &[String] {
        self.contexts.as_ref()
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        let context = context.into();
        if self.contexts.iter().any(|existing| existing == &context) {
            return self;
        }
        let mut contexts = self.contexts.iter().cloned().collect::<Vec<_>>();
        contexts.push(context);
        self.contexts = Arc::from(contexts);
        self
    }

    pub fn observations(&self) -> &[HostObservation] {
        self.observations.as_ref()
    }

    pub fn with_observations(mut self, items: impl IntoIterator<Item = HostObservation>) -> Self {
        let mut observations = BTreeMap::new();
        for observation in self.observations.iter().cloned() {
            observations.insert(observation.key(), observation);
        }
        for observation in items {
            observations.insert(observation.key(), observation);
        }
        self.observations = Arc::from(observations.into_values().collect::<Vec<_>>());
        self
    }
}

impl fmt::Display for PoisonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        if !self.causes.is_empty() {
            write!(f, "\n  causes:")?;
            for cause in self.causes.iter() {
                write!(f, "\n    - {:?}: {}", cause.kind, cause.message)?;
            }
        }
        if !self.contexts.is_empty() {
            write!(f, "\n  contexts:")?;
            for context in self.contexts.iter() {
                write!(f, "\n    - {context}")?;
            }
        }
        if !self.demand_chain.entries.is_empty() {
            write!(f, "\n  demand chain:")?;
            for entry in &self.demand_chain.entries {
                write!(f, "\n    -> {}", entry.description)?;
            }
        }
        Ok(())
    }
}

impl From<&DemandChain> for TransportDemandChain {
    fn from(chain: &DemandChain) -> Self {
        Self {
            entries: chain
                .entries
                .iter()
                .map(TransportDemandEntry::from)
                .collect(),
        }
    }
}

impl From<TransportDemandChain> for DemandChain {
    fn from(chain: TransportDemandChain) -> Self {
        Self {
            entries: chain.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&DemandEntry> for TransportDemandEntry {
    fn from(entry: &DemandEntry) -> Self {
        Self {
            node_id: entry.node_id.0,
            description: entry.description.clone(),
            provenance: entry
                .provenance
                .as_ref()
                .map(TransportSourceProvenance::from),
        }
    }
}

impl From<TransportDemandEntry> for DemandEntry {
    fn from(entry: TransportDemandEntry) -> Self {
        Self {
            node_id: NodeId(entry.node_id),
            description: entry.description,
            provenance: entry.provenance.map(Into::into),
        }
    }
}

impl From<&PoisonError> for TransportPoisonError {
    fn from(error: &PoisonError) -> Self {
        Self {
            kind: TransportPoisonKind::from(&error.kind),
            message: error.message.clone(),
            demand_chain: TransportDemandChain::from(&error.demand_chain),
            causes: error
                .causes
                .iter()
                .map(TransportPoisonError::from)
                .collect(),
            contexts: error.contexts.iter().cloned().collect(),
            observations: error.observations.iter().cloned().collect(),
        }
    }
}

impl From<TransportPoisonError> for PoisonError {
    fn from(error: TransportPoisonError) -> Self {
        Self {
            kind: PoisonKind::from(error.kind),
            message: error.message,
            demand_chain: error.demand_chain.into(),
            causes: Arc::from(
                error
                    .causes
                    .into_iter()
                    .map(PoisonError::from)
                    .collect::<Vec<_>>(),
            ),
            contexts: Arc::from(error.contexts),
            observations: Arc::from(error.observations),
        }
    }
}

impl From<&PoisonKind> for TransportPoisonKind {
    fn from(kind: &PoisonKind) -> Self {
        match kind {
            PoisonKind::TypeError => Self::TypeError,
            PoisonKind::UnresolvedName => Self::UnresolvedName,
            PoisonKind::ArithmeticError => Self::ArithmeticError,
            PoisonKind::CapabilityViolation => Self::CapabilityViolation,
            PoisonKind::UserFail => Self::UserFail,
            PoisonKind::VerificationFailure => Self::VerificationFailure,
            PoisonKind::CircularDependency => Self::CircularDependency,
            PoisonKind::DemandDepthExceeded => Self::DemandDepthExceeded,
            PoisonKind::Multiple => Self::Multiple,
            PoisonKind::Internal => Self::Internal,
        }
    }
}

impl From<TransportPoisonKind> for PoisonKind {
    fn from(kind: TransportPoisonKind) -> Self {
        match kind {
            TransportPoisonKind::TypeError => Self::TypeError,
            TransportPoisonKind::UnresolvedName => Self::UnresolvedName,
            TransportPoisonKind::ArithmeticError => Self::ArithmeticError,
            TransportPoisonKind::CapabilityViolation => Self::CapabilityViolation,
            TransportPoisonKind::UserFail => Self::UserFail,
            TransportPoisonKind::VerificationFailure => Self::VerificationFailure,
            TransportPoisonKind::CircularDependency => Self::CircularDependency,
            TransportPoisonKind::DemandDepthExceeded => Self::DemandDepthExceeded,
            TransportPoisonKind::Multiple => Self::Multiple,
            TransportPoisonKind::Internal => Self::Internal,
        }
    }
}

impl fmt::Display for TransportPoisonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        PoisonError::from(self.clone()).fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_poison_roundtrips_structure() {
        let mut demand_chain = DemandChain::new();
        demand_chain.push(NodeId(7), "call(..)".to_string(), None);

        let error = PoisonError::new(PoisonKind::UserFail, "boom", demand_chain)
            .with_context("build.vx discover[class:orb]")
            .with_observations([HostObservation::Which {
                name: "cc".to_string(),
                expected_path: None,
            }]);

        let transport = TransportPoisonError::from(&error);
        let roundtrip = PoisonError::from(transport);

        assert_eq!(roundtrip.kind, PoisonKind::UserFail);
        assert_eq!(roundtrip.message, "boom");
        assert_eq!(roundtrip.contexts(), ["build.vx discover[class:orb]"]);
        assert_eq!(roundtrip.demand_chain.entries.len(), 1);
        assert_eq!(roundtrip.demand_chain.entries[0].description, "call(..)");
        assert_eq!(roundtrip.observations().len(), 1);
    }
}
