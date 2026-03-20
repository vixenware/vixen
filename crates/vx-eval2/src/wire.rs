//! Conversion between local evaluator values and protocol envelopes.

use std::sync::Arc;

use vx_eval2_proto::{EvalOutcome, StructValue as WireStructValue, ValueEnvelope, WireValue};

use crate::{EvalError, EvalErrorKind, Resolution, StructValue, Value, ValueRep};

pub fn value_to_envelope(value: &Value) -> Result<ValueEnvelope, EvalError> {
    Ok(ValueEnvelope {
        value: value_to_wire_value(value)?,
        meta: value.meta.clone(),
    })
}

pub fn envelope_to_value(envelope: ValueEnvelope) -> Result<Value, EvalError> {
    Ok(Value {
        rep: wire_value_to_value_rep(envelope.value)?,
        meta: envelope.meta,
    })
}

pub fn resolution_to_outcome(resolution: Resolution) -> Result<EvalOutcome, EvalError> {
    match resolution {
        Resolution::Ready(value) => Ok(EvalOutcome::Ready(value_to_envelope(&value)?)),
        Resolution::Pending(pending) => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("pending values cannot cross the daemon boundary: {pending:?}"),
        )),
        Resolution::Waiting => Err(EvalError::new(
            EvalErrorKind::Internal,
            "waiting eval sessions cannot cross the daemon boundary",
        )),
        Resolution::Poison(error) => Ok(EvalOutcome::Poison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
        }),
    }
}

fn value_to_wire_value(value: &Value) -> Result<WireValue, EvalError> {
    match &value.rep {
        ValueRep::Unit => Ok(WireValue::Unit(())),
        ValueRep::Bool(value) => Ok(WireValue::Bool(*value)),
        ValueRep::U64(value) => Ok(WireValue::U64(*value)),
        ValueRep::String(value) => Ok(WireValue::String(value.to_string())),
        ValueRep::Array(items) => Ok(WireValue::Array(
            items
                .iter()
                .map(value_to_envelope)
                .collect::<Result<_, _>>()?,
        )),
        ValueRep::Map(map) => Ok(WireValue::Map(
            map.entries
                .iter()
                .map(|(key, value)| Ok((value_to_envelope(key)?, value_to_envelope(value)?)))
                .collect::<Result<_, EvalError>>()?,
        )),
        ValueRep::Struct(StructValue { type_name, fields }) => {
            Ok(WireValue::Struct(WireStructValue {
                type_name: type_name.to_string(),
                fields: fields
                    .iter()
                    .map(|(name, value)| Ok((name.to_string(), value_to_envelope(value)?)))
                    .collect::<Result<_, EvalError>>()?,
            }))
        }
        ValueRep::External(external) => Ok(WireValue::External {
            kind: external.kind.to_string(),
            descriptor: external.descriptor.to_string(),
        }),
        ValueRep::Function(function) => Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "function values cannot cross the daemon boundary yet: {}::{}",
                function.source_id, function.name,
            ),
        )),
    }
}

fn wire_value_to_value_rep(value: WireValue) -> Result<ValueRep, EvalError> {
    match value {
        WireValue::Unit(()) => Ok(ValueRep::Unit),
        WireValue::Bool(value) => Ok(ValueRep::Bool(value)),
        WireValue::U64(value) => Ok(ValueRep::U64(value)),
        WireValue::I64(value) => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("i64 wire values are not supported by eval2 core yet: {value}"),
        )),
        WireValue::String(value) => Ok(ValueRep::String(Arc::from(value))),
        WireValue::Array(items) => Ok(ValueRep::Array(Arc::from(
            items
                .into_iter()
                .map(envelope_to_value)
                .collect::<Result<Vec<_>, _>>()?,
        ))),
        WireValue::Map(entries) => Ok(ValueRep::Map(crate::MapValue {
            entries: Arc::from(
                entries
                    .into_iter()
                    .map(|(key, value)| Ok((envelope_to_value(key)?, envelope_to_value(value)?)))
                    .collect::<Result<Vec<_>, EvalError>>()?,
            ),
        })),
        WireValue::Struct(WireStructValue { type_name, fields }) => {
            Ok(ValueRep::Struct(StructValue {
                type_name: Arc::from(type_name),
                fields: Arc::from(
                    fields
                        .into_iter()
                        .map(|(name, value)| Ok((Arc::from(name), envelope_to_value(value)?)))
                        .collect::<Result<Vec<_>, EvalError>>()?,
                ),
            }))
        }
        WireValue::External { kind, descriptor } => Ok(ValueRep::External(crate::ExternalValue {
            kind: Arc::from(kind),
            descriptor: Arc::from(descriptor),
        })),
        WireValue::Proxy(reference) => Ok(ValueRep::External(crate::ExternalValue {
            kind: Arc::from("proxy"),
            descriptor: Arc::from(format!("{}:{}", reference.owner_place, reference.handle_id)),
        })),
    }
}
