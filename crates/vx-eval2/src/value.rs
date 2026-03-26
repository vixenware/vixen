//! Runtime value payloads.

use std::{cmp::Ordering, sync::Arc};

use im::OrdMap;

use crate::{
    Claim, CrossingPolicy, ProjectionStep, Provenance, ReuseClass, SourceSpan, ValueMeta,
    program::{BytecodeBodyId, CompiledParameter, ExprBodyRef},
};

pub type FunctionCaptures = OrdMap<String, Value>;

pub fn empty_function_captures() -> Arc<FunctionCaptures> {
    Arc::new(FunctionCaptures::new())
}

pub fn function_captures<I, N>(entries: I) -> Arc<FunctionCaptures>
where
    I: IntoIterator<Item = (N, Value)>,
    N: Into<String>,
{
    let mut captures = FunctionCaptures::new();
    for (name, value) in entries {
        captures.insert(name.into(), value);
    }
    Arc::new(captures)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionBytecodeEntry {
    pub body: BytecodeBodyId,
    pub local_count: usize,
    pub parameter_slots: Arc<[Option<u32>]>,
    pub scope_prefill_slots: Arc<[(Arc<str>, u32)]>,
}

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

pub(crate) fn compare_values(lhs: &Value, rhs: &Value) -> Ordering {
    compare_value_reps(&lhs.rep, &rhs.rep)
}

pub(crate) fn compare_value_reps(lhs: &ValueRep, rhs: &ValueRep) -> Ordering {
    let tag_order = value_rep_tag(lhs).cmp(&value_rep_tag(rhs));
    if tag_order != Ordering::Equal {
        return tag_order;
    }

    match (lhs, rhs) {
        (ValueRep::Unit, ValueRep::Unit) => Ordering::Equal,
        (ValueRep::Bool(lhs), ValueRep::Bool(rhs)) => lhs.cmp(rhs),
        (ValueRep::U64(lhs), ValueRep::U64(rhs)) => lhs.cmp(rhs),
        (ValueRep::String(lhs), ValueRep::String(rhs)) => lhs.cmp(rhs),
        (ValueRep::Array(lhs), ValueRep::Array(rhs))
        | (ValueRep::Tuple(lhs), ValueRep::Tuple(rhs)) => compare_value_slices(lhs, rhs),
        (ValueRep::Range(lhs), ValueRep::Range(rhs)) => lhs
            .start
            .cmp(&rhs.start)
            .then_with(|| lhs.end.cmp(&rhs.end))
            .then_with(|| lhs.inclusive_end.cmp(&rhs.inclusive_end)),
        (ValueRep::Map(lhs), ValueRep::Map(rhs)) => {
            compare_value_pair_slices(&lhs.entries, &rhs.entries)
        }
        (ValueRep::Struct(lhs), ValueRep::Struct(rhs)) => lhs
            .type_name
            .cmp(&rhs.type_name)
            .then_with(|| compare_named_value_slices(&lhs.fields, &rhs.fields)),
        (ValueRep::External(lhs), ValueRep::External(rhs)) => lhs
            .kind
            .cmp(&rhs.kind)
            .then_with(|| lhs.descriptor.cmp(&rhs.descriptor)),
        (ValueRep::Function(lhs), ValueRep::Function(rhs)) => lhs
            .source_id
            .cmp(&rhs.source_id)
            .then_with(|| lhs.name.cmp(&rhs.name))
            .then_with(|| lhs.has_keyword_parameters.cmp(&rhs.has_keyword_parameters))
            .then_with(|| lhs.body.cmp(&rhs.body))
            .then_with(|| {
                compare_function_bytecode_entry(
                    lhs.bytecode_entry.as_ref(),
                    rhs.bytecode_entry.as_ref(),
                )
            })
            .then_with(|| lhs.self_name.cmp(&rhs.self_name))
            .then_with(|| compare_compiled_parameters(&lhs.parameters, &rhs.parameters))
            .then_with(|| compare_function_captures(&lhs.captures, &rhs.captures)),
        _ => Ordering::Equal,
    }
}

fn value_rep_tag(value: &ValueRep) -> u8 {
    match value {
        ValueRep::Unit => 0,
        ValueRep::Bool(_) => 1,
        ValueRep::U64(_) => 2,
        ValueRep::String(_) => 3,
        ValueRep::Array(_) => 4,
        ValueRep::Tuple(_) => 5,
        ValueRep::Range(_) => 6,
        ValueRep::Map(_) => 7,
        ValueRep::Struct(_) => 8,
        ValueRep::External(_) => 9,
        ValueRep::Function(_) => 10,
    }
}

fn compare_value_slices(lhs: &[Value], rhs: &[Value]) -> Ordering {
    for (lhs, rhs) in lhs.iter().zip(rhs.iter()) {
        let item_order = compare_values(lhs, rhs);
        if item_order != Ordering::Equal {
            return item_order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn compare_value_pair_slices(lhs: &[(Value, Value)], rhs: &[(Value, Value)]) -> Ordering {
    for ((lhs_key, lhs_value), (rhs_key, rhs_value)) in lhs.iter().zip(rhs.iter()) {
        let key_order = compare_values(lhs_key, rhs_key);
        if key_order != Ordering::Equal {
            return key_order;
        }
        let value_order = compare_values(lhs_value, rhs_value);
        if value_order != Ordering::Equal {
            return value_order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn compare_named_value_slices(lhs: &[(Arc<str>, Value)], rhs: &[(Arc<str>, Value)]) -> Ordering {
    for ((lhs_name, lhs_value), (rhs_name, rhs_value)) in lhs.iter().zip(rhs.iter()) {
        let name_order = lhs_name.cmp(rhs_name);
        if name_order != Ordering::Equal {
            return name_order;
        }
        let value_order = compare_values(lhs_value, rhs_value);
        if value_order != Ordering::Equal {
            return value_order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn compare_function_captures(lhs: &FunctionCaptures, rhs: &FunctionCaptures) -> Ordering {
    for ((lhs_name, lhs_value), (rhs_name, rhs_value)) in lhs.iter().zip(rhs.iter()) {
        let name_order = lhs_name.cmp(rhs_name);
        if name_order != Ordering::Equal {
            return name_order;
        }
        let value_order = compare_values(lhs_value, rhs_value);
        if value_order != Ordering::Equal {
            return value_order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn compare_function_bytecode_entry(
    lhs: Option<&FunctionBytecodeEntry>,
    rhs: Option<&FunctionBytecodeEntry>,
) -> Ordering {
    match (lhs, rhs) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(lhs), Some(rhs)) => lhs
            .body
            .cmp(&rhs.body)
            .then_with(|| lhs.local_count.cmp(&rhs.local_count))
            .then_with(|| compare_optional_u32_slices(&lhs.parameter_slots, &rhs.parameter_slots))
            .then_with(|| {
                compare_named_slot_slices(&lhs.scope_prefill_slots, &rhs.scope_prefill_slots)
            }),
    }
}

fn compare_optional_u32_slices(lhs: &[Option<u32>], rhs: &[Option<u32>]) -> Ordering {
    for (lhs, rhs) in lhs.iter().zip(rhs.iter()) {
        let order = lhs.cmp(rhs);
        if order != Ordering::Equal {
            return order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn compare_named_slot_slices(lhs: &[(Arc<str>, u32)], rhs: &[(Arc<str>, u32)]) -> Ordering {
    for ((lhs_name, lhs_slot), (rhs_name, rhs_slot)) in lhs.iter().zip(rhs.iter()) {
        let name_order = lhs_name.cmp(rhs_name);
        if name_order != Ordering::Equal {
            return name_order;
        }
        let slot_order = lhs_slot.cmp(rhs_slot);
        if slot_order != Ordering::Equal {
            return slot_order;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn compare_compiled_parameters(lhs: &[CompiledParameter], rhs: &[CompiledParameter]) -> Ordering {
    for (lhs, rhs) in lhs.iter().zip(rhs.iter()) {
        let label_order = lhs.label.cmp(&rhs.label);
        if label_order != Ordering::Equal {
            return label_order;
        }
        let name_order = lhs.name.cmp(&rhs.name);
        if name_order != Ordering::Equal {
            return name_order;
        }
    }
    lhs.len().cmp(&rhs.len())
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
    pub(crate) body: Option<ExprBodyRef>,
    pub(crate) bytecode_entry: Option<FunctionBytecodeEntry>,
    pub(crate) entry_locals: Option<Arc<[Option<Value>]>>,
    pub(crate) captures: Arc<FunctionCaptures>,
    pub(crate) self_name: Option<Arc<str>>,
}

impl FunctionRef {
    pub(crate) fn new(
        source_id: Arc<str>,
        name: Arc<str>,
        parameters: Arc<[CompiledParameter]>,
        has_keyword_parameters: bool,
        body: Option<ExprBodyRef>,
        bytecode_entry: Option<FunctionBytecodeEntry>,
        captures: Arc<FunctionCaptures>,
        self_name: Option<Arc<str>>,
    ) -> Self {
        let entry_locals = bytecode_entry
            .as_ref()
            .map(|entry| Arc::from(prefilled_entry_locals(entry, &captures)));
        Self {
            source_id,
            name,
            parameters,
            has_keyword_parameters,
            body,
            bytecode_entry,
            entry_locals,
            captures,
            self_name,
        }
    }

    pub fn name(&self) -> &str {
        self.name.as_ref()
    }

    pub fn captures(&self) -> &FunctionCaptures {
        &self.captures
    }
}

fn prefilled_entry_locals(
    entry: &FunctionBytecodeEntry,
    captures: &FunctionCaptures,
) -> Vec<Option<Value>> {
    let mut locals = vec![None; entry.local_count];
    for (name, slot) in entry.scope_prefill_slots.iter() {
        if let Some(value) = captures.get(name.as_ref()) {
            locals[*slot as usize] = Some(value.clone());
        }
    }
    locals
}
