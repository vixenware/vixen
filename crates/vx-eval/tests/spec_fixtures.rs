use std::path::Path;
use std::sync::Arc;

use facet_value::Value as DynamicValue;
use vx_eval::{CapabilitySet, Engine, Value, lower_source_file_with_source_id};
use vx_parser::SourceFile;
use vx_spec::{
    EngineExpectation, EngineId, ObservedPoison, ObservedResult, ResolvedSpecRun, SpecCase,
    SpecOutcome, SpecValue, StructSpec,
};

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let case = SpecCase::from_path(path)?;
    match case.expectation_for(EngineId::Eval1) {
        EngineExpectation::Pass => {
            let runtime = tokio::runtime::Runtime::new()?;
            for (index, invocation) in case.invocations().iter().enumerate() {
                let outcome = runtime.block_on(run_case(&case, invocation))?;
                invocation
                    .expect
                    .assert_matches(&outcome)
                    .map_err(|error| {
                        std::io::Error::other(format!(
                            "run #{} expectation failed: {error}",
                            index + 1
                        ))
                    })?;
            }
            Ok(())
        }
        EngineExpectation::Todo | EngineExpectation::Skip => Ok(()),
    }
}

async fn run_case(
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    let parse = vx_parser::parse(&case.main_source);
    if !parse.errors().is_empty() {
        return Err(std::io::Error::other(format!("parse errors: {:?}", parse.errors())).into());
    }
    let root = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file_with_source_id(&root, case.path.display().to_string())
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let engine = Engine::from_module(&module).with_capabilities(CapabilitySet::TEST_ONLY);
    let args = invocation
        .invoke
        .args
        .iter()
        .cloned()
        .map(dynamic_to_eval_value)
        .collect::<Result<Vec<_>, _>>()?;
    match engine.call(&invocation.invoke.entry, args).await {
        Ok(value) => Ok(SpecOutcome::Result(ObservedResult {
            value: eval_value_to_spec(&value)?,
            provenance: None,
            obligations: Vec::new(),
            reuse: None,
            crossing: None,
            events: Vec::new(),
        })),
        Err(error) => Ok(SpecOutcome::Poison(ObservedPoison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
            events: Vec::new(),
        })),
    }
}

fn dynamic_to_eval_value(value: DynamicValue) -> Result<Value, std::io::Error> {
    if value.is_null() {
        return Ok(Value::Unit);
    }
    if let Some(value) = value.as_bool() {
        return Ok(Value::Bool(value));
    }
    if let Some(number) = value.as_number().and_then(|number| number.to_u64()) {
        return Ok(Value::U64(number));
    }
    if let Some(text) = value.as_string() {
        return Ok(Value::String(Arc::from(text.as_str())));
    }
    if let Some(array) = value.as_array() {
        let items = array
            .iter()
            .map(|item| dynamic_to_eval_value(item.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Value::Array(items.into()));
    }
    if let Some(object) = value.as_object() {
        let entries = object
            .iter()
            .map(|(key, value)| {
                Ok((
                    Value::String(Arc::from(key.as_str())),
                    dynamic_to_eval_value(value.clone())?,
                ))
            })
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        return Ok(Value::Map(vx_eval::MapValue {
            entries: entries.into(),
        }));
    }
    Err(std::io::Error::other(format!(
        "dynamic argument value is not supported yet: {value:?}"
    )))
}

fn eval_value_to_spec(value: &Value) -> Result<SpecValue, std::io::Error> {
    match value {
        Value::Discovered(discovered) => eval_value_to_spec(discovered.value.as_ref()),
        Value::Unit => Ok(SpecValue::Unit(())),
        Value::Bool(value) => Ok(SpecValue::Bool(*value)),
        Value::U64(value) => Ok(SpecValue::U64(*value)),
        Value::I64(value) => Ok(SpecValue::I64(*value)),
        Value::F64(value) => Ok(SpecValue::F64(*value)),
        Value::String(value) => Ok(SpecValue::String(value.to_string())),
        Value::Array(items) | Value::Tuple(items) => Ok(SpecValue::Array(
            items
                .iter()
                .map(eval_value_to_spec)
                .collect::<Result<_, _>>()?,
        )),
        Value::Struct(struct_value) => Ok(SpecValue::Struct(StructSpec {
            r#type: struct_value.type_name.to_string(),
            fields: struct_value
                .fields
                .iter()
                .map(|(name, value)| Ok((name.to_string(), eval_value_to_spec(value)?)))
                .collect::<Result<_, std::io::Error>>()?,
        })),
        Value::Map(map) => Ok(SpecValue::Map(
            map.entries
                .iter()
                .map(|(key, value)| {
                    let Value::String(key) = key else {
                        return Err(std::io::Error::other(
                            "only string-keyed maps can be converted to spec values",
                        ));
                    };
                    Ok((key.to_string(), eval_value_to_spec(value)?))
                })
                .collect::<Result<_, std::io::Error>>()?,
        )),
        other => Err(std::io::Error::other(format!(
            "eval1 result value is not supported yet: {other:?}"
        ))),
    }
}

datatest_stable::harness! {
    { test = run_fixture, root = "../../specs/eval", pattern = r"(^|.*/)[^/]+\.styx$" },
}
