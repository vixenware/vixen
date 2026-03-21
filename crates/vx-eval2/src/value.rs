//! Runtime value payloads.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    Claim, CrossingPolicy, ProjectionStep, Provenance, ReuseClass, SourceSpan, ValueMeta,
    program::{CompiledParameter, ProgramNodeHandle},
};

/// A successful runtime value: payload plus orthogonal metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub rep: ValueRep,
    pub meta: ValueMeta,
}

impl Value {
    pub fn new(rep: ValueRep) -> Self {
        Self {
            meta: rep.default_meta(),
            rep,
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

    pub fn tuple(items: impl Into<Vec<Value>>) -> Self {
        Self::new(ValueRep::Tuple(Arc::from(items.into())))
    }

    pub fn range(start: Option<u64>, end: Option<u64>, inclusive_end: bool) -> Self {
        Self::new(ValueRep::Range(RangeValue {
            start,
            end,
            inclusive_end,
        }))
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

    pub fn external(kind: impl Into<Arc<str>>, descriptor: impl Into<Arc<str>>) -> Self {
        Self::new(ValueRep::External(ExternalValue {
            kind: kind.into(),
            descriptor: descriptor.into(),
        }))
    }

    pub fn function(function: FunctionRef) -> Self {
        Self::new(ValueRep::Function(function))
    }

    pub fn with_meta(mut self, meta: ValueMeta) -> Self {
        self.meta = meta;
        self
    }

    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.meta = self.meta.with_provenance(provenance);
        self
    }

    pub fn with_reuse(mut self, reuse: ReuseClass) -> Self {
        self.meta = self.meta.with_reuse(reuse);
        self
    }

    pub fn with_crossing(mut self, crossing: CrossingPolicy) -> Self {
        self.meta = self.meta.with_crossing(crossing);
        self
    }

    pub fn with_claim(mut self, claim: Claim) -> Self {
        self.meta = self.meta.with_claim(claim);
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
    Tuple(Arc<[Value]>),
    Range(RangeValue),
    Map(MapValue),
    Struct(StructValue),
    External(ExternalValue),
    Function(FunctionRef),
}

impl ValueRep {
    pub fn default_meta(&self) -> ValueMeta {
        match self {
            Self::Unit
            | Self::Bool(_)
            | Self::U64(_)
            | Self::String(_)
            | Self::Array(_)
            | Self::Tuple(_)
            | Self::Range(_)
            | Self::Map(_)
            | Self::Struct(_)
            | Self::Function(_) => ValueMeta::new(
                Provenance::unknown(SourceSpan::synthetic("<unknown>")),
                ReuseClass::Stable,
                CrossingPolicy::ByValue,
            ),
            Self::External(_) => ValueMeta::new(
                Provenance::unknown(SourceSpan::synthetic("<unknown>")),
                ReuseClass::Live,
                CrossingPolicy::Blocked,
            ),
        }
    }
}

/// An ordered map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapValue {
    pub entries: Arc<[(Value, Value)]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeValue {
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub inclusive_end: bool,
}

/// A struct value: type name plus fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructValue {
    pub type_name: Arc<str>,
    pub fields: Arc<[(Arc<str>, Value)]>,
}

/// A runtime-owned value whose meaning is outside the evaluator core.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalValue {
    pub kind: Arc<str>,
    pub descriptor: Arc<str>,
}

/// Opaque function handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionRef {
    pub(crate) source_id: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) parameters: Arc<[CompiledParameter]>,
    pub(crate) has_keyword_parameters: bool,
    pub(crate) body: Option<ProgramNodeHandle>,
    pub(crate) captures: Arc<[(Arc<str>, Value)]>,
    pub(crate) self_name: Option<Arc<str>>,
}

impl FunctionRef {
    pub(crate) fn new(
        source_id: Arc<str>,
        name: Arc<str>,
        parameters: Arc<[CompiledParameter]>,
        has_keyword_parameters: bool,
        body: Option<ProgramNodeHandle>,
        captures: Arc<[(Arc<str>, Value)]>,
        self_name: Option<Arc<str>>,
    ) -> Self {
        Self {
            source_id,
            name,
            parameters,
            has_keyword_parameters,
            body,
            captures,
            self_name,
        }
    }

    pub(crate) fn capture_env(&self) -> BTreeMap<String, Value> {
        self.captures
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect()
    }
}
