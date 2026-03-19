//! Runtime value payloads.

use std::path::PathBuf;
use std::sync::Arc;

use crate::meta::{BoundaryConversion, PinnedReason, Portability, ValueMeta};
use crate::provenance::{ProjectionStep, Provenance};

/// A successful runtime value: payload plus orthogonal metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub rep: ValueRep,
    pub meta: ValueMeta,
}

impl Value {
    pub fn new(rep: ValueRep) -> Self {
        let portability = rep.default_portability();
        Self {
            rep,
            meta: ValueMeta::new(Provenance::unknown(), portability),
        }
    }

    pub fn unit() -> Self {
        Self::new(ValueRep::Unit)
    }

    pub fn bool(value: bool) -> Self {
        Self::new(ValueRep::Bool(value))
    }

    pub fn u64(value: u64) -> Self {
        Self::new(ValueRep::U64(value))
    }

    pub fn string(value: impl Into<Arc<str>>) -> Self {
        Self::new(ValueRep::String(value.into()))
    }

    pub fn array(items: impl Into<Vec<Value>>) -> Self {
        Self::new(ValueRep::Array(Arc::from(items.into())))
    }

    pub fn map(entries: impl Into<Vec<(Value, Value)>>) -> Self {
        Self::new(ValueRep::Map(MapValue {
            entries: Arc::from(entries.into()),
        }))
    }

    pub fn structure(
        type_name: impl Into<Arc<str>>,
        fields: impl Into<Vec<(Arc<str>, Value)>>,
    ) -> Self {
        Self::new(ValueRep::Struct(StructValue {
            type_name: type_name.into(),
            fields: Arc::from(fields.into()),
        }))
    }

    pub fn local_blob(path: impl Into<PathBuf>, executable: bool) -> Self {
        Self::new(ValueRep::Blob(BlobRep::LocalFile {
            path: path.into(),
            executable,
        }))
    }

    pub fn store_blob(digest: impl Into<Arc<str>>, executable: bool) -> Self {
        Self::new(ValueRep::Blob(BlobRep::StoreRef {
            digest: digest.into(),
            executable,
        }))
    }

    pub fn local_tree(path: impl Into<PathBuf>) -> Self {
        Self::new(ValueRep::Tree(TreeRep::LocalDir { path: path.into() }))
    }

    pub fn store_tree(digest: impl Into<Arc<str>>) -> Self {
        Self::new(ValueRep::Tree(TreeRep::StoreRef {
            digest: digest.into(),
        }))
    }

    pub fn host_handle(id: impl Into<Arc<str>>) -> Self {
        Self::new(ValueRep::Host(HostRef { id: id.into() }))
    }

    pub fn with_meta(mut self, meta: ValueMeta) -> Self {
        self.meta = meta;
        self
    }

    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.meta = self.meta.with_provenance(provenance);
        self
    }

    pub fn with_portability(mut self, portability: Portability) -> Self {
        self.meta = self.meta.with_portability(portability);
        self
    }

    pub fn with_obligation(mut self, observation: crate::Observation) -> Self {
        self.meta = self.meta.with_obligation(observation);
        self
    }

    pub fn project(mut self, step: ProjectionStep) -> Self {
        self.meta.provenance = self.meta.provenance.project(step);
        self
    }

    pub fn as_u64(&self) -> Option<u64> {
        match &self.rep {
            ValueRep::U64(value) => Some(*value),
            _ => None,
        }
    }
}

/// The payload of a successful runtime value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueRep {
    Unit,
    Bool(bool),
    U64(u64),
    String(Arc<str>),
    Array(Arc<[Value]>),
    Map(MapValue),
    Struct(StructValue),
    Blob(BlobRep),
    Tree(TreeRep),
    Host(HostRef),
    Function(FunctionRef),
}

impl ValueRep {
    pub fn default_portability(&self) -> Portability {
        match self {
            Self::Unit
            | Self::Bool(_)
            | Self::U64(_)
            | Self::String(_)
            | Self::Array(_)
            | Self::Map(_)
            | Self::Struct(_)
            | Self::Function(_) => Portability::Portable,
            Self::Blob(BlobRep::InlineBytes { .. }) | Self::Blob(BlobRep::StoreRef { .. }) => {
                Portability::Portable
            }
            Self::Tree(TreeRep::Inline { .. }) | Self::Tree(TreeRep::StoreRef { .. }) => {
                Portability::Portable
            }
            Self::Blob(BlobRep::LocalFile { .. }) => Portability::Convertible {
                via: BoundaryConversion::SnapshotBlob,
            },
            Self::Tree(TreeRep::LocalDir { .. }) => Portability::Convertible {
                via: BoundaryConversion::SnapshotTree,
            },
            Self::Host(_) => Portability::Pinned {
                reason: PinnedReason::HostHandle,
            },
        }
    }
}

/// An ordered map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapValue {
    pub entries: Arc<[(Value, Value)]>,
}

/// A struct value: type name plus fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructValue {
    pub type_name: Arc<str>,
    pub fields: Arc<[(Arc<str>, Value)]>,
}

/// File-like content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobRep {
    InlineBytes { bytes: Arc<[u8]>, executable: bool },
    LocalFile { path: PathBuf, executable: bool },
    StoreRef { digest: Arc<str>, executable: bool },
}

/// Directory-like content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRep {
    Inline { entries: Arc<[(Arc<str>, Value)]> },
    LocalDir { path: PathBuf },
    StoreRef { digest: Arc<str> },
}

/// Opaque host capability.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostRef {
    pub id: Arc<str>,
}

/// Opaque function handle.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionRef {
    pub module_id: Arc<str>,
    pub name: Arc<str>,
}
