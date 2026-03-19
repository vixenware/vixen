//! Value metadata.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::provenance::Provenance;

/// Host facts observed earlier and rechecked at a later boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Observation {
    Os {
        value: Arc<str>,
    },
    Arch {
        value: Arc<str>,
    },
    Which {
        name: Arc<str>,
        expected_path: Option<Arc<str>>,
    },
    Read {
        path: Arc<str>,
        content_hash: Arc<str>,
    },
    Hash {
        path: Arc<str>,
        content_hash: Arc<str>,
    },
}

/// How a value may cross a host boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Portability {
    /// Already safe to move to another host unchanged.
    Portable,
    /// Can cross a boundary after a runtime conversion.
    Convertible { via: BoundaryConversion },
    /// Cannot cross a boundary in this representation.
    Pinned { reason: PinnedReason },
}

/// Runtime conversion used to make a value portable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BoundaryConversion {
    SnapshotBlob,
    SnapshotTree,
}

/// Why a value is pinned to the current host or runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PinnedReason {
    HostHandle,
    RunningProcess,
    OpenStream,
}

/// Metadata attached to every successful value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueMeta {
    pub provenance: Provenance,
    pub obligations: Arc<[Observation]>,
    pub portability: Portability,
}

impl ValueMeta {
    pub fn new(provenance: Provenance, portability: Portability) -> Self {
        Self {
            provenance,
            obligations: Arc::from([]),
            portability,
        }
    }

    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = provenance;
        self
    }

    pub fn with_portability(mut self, portability: Portability) -> Self {
        self.portability = portability;
        self
    }

    pub fn with_obligation(mut self, observation: Observation) -> Self {
        self = self.with_obligations([observation]);
        self
    }

    pub fn with_obligations(mut self, observations: impl IntoIterator<Item = Observation>) -> Self {
        let mut deduped = self.obligations.iter().cloned().collect::<BTreeSet<_>>();
        deduped.extend(observations);
        self.obligations = Arc::from(deduped.into_iter().collect::<Vec<_>>());
        self
    }
}
