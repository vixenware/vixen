use std::path::Path;
use std::sync::Arc;

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
        .map(spec_value_to_eval_value)
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

fn spec_value_to_eval_value(value: SpecValue) -> Result<Value, std::io::Error> {
    match value {
        SpecValue::Unit(()) => Ok(Value::Unit),
        SpecValue::Bool(value) => Ok(Value::Bool(value)),
        SpecValue::U64(value) => Ok(Value::U64(value)),
        SpecValue::I64(value) => Ok(Value::I64(value)),
        SpecValue::F64(value) => Ok(Value::F64(value)),
        SpecValue::String(value) => Ok(Value::String(Arc::from(value))),
        SpecValue::Array(items) => Ok(Value::Array(
            items
                .into_iter()
                .map(spec_value_to_eval_value)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        )),
        SpecValue::Map(entries) => Ok(Value::Map(vx_eval::MapValue {
            entries: entries
                .into_iter()
                .map(|(key, value)| {
                    Ok((
                        Value::String(Arc::from(key)),
                        spec_value_to_eval_value(value)?,
                    ))
                })
                .collect::<Result<Vec<_>, std::io::Error>>()?
                .into(),
        })),
        SpecValue::Struct(struct_spec) => Ok(Value::Struct(vx_eval::StructValue {
            type_name: Arc::from(struct_spec.r#type),
            fields: struct_spec
                .fields
                .into_iter()
                .map(|(name, value)| Ok((Arc::from(name), spec_value_to_eval_value(value)?)))
                .collect::<Result<Vec<_>, std::io::Error>>()?
                .into(),
        })),
        SpecValue::External(external) => Err(std::io::Error::other(format!(
            "eval1 fixture args do not support external values yet: {external:?}"
        ))),
    }
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
