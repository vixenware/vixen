use std::path::Path;

use vx_eval2::{Resolution, Value, ValueRep, eval_source_entry};
use vx_spec::{
    EngineExpectation, EngineId, ObservedPoison, SpecCase, SpecOutcome, SpecValue, StructSpec,
};

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let case = SpecCase::from_path(path)?;
    match case.expectation_for(EngineId::Eval2) {
        EngineExpectation::Pass => {
            let outcome = run_case(&case)?;
            case.assert_matches(&outcome)?;
            Ok(())
        }
        EngineExpectation::Todo | EngineExpectation::Skip => Ok(()),
    }
}

fn run_case(case: &SpecCase) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    match eval_source_entry(&case.main_source, &case.invoke.entry) {
        Ok(Resolution::Ready(value)) => Ok(SpecOutcome::Result(eval2_value_to_spec(&value)?)),
        Ok(Resolution::Pending(pending)) => Err(std::io::Error::other(format!(
            "eval2 returned pending at the external boundary: {pending:?}"
        ))
        .into()),
        Ok(Resolution::Poison(error)) | Err(error) => Ok(SpecOutcome::Poison(ObservedPoison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
        })),
    }
}

fn eval2_value_to_spec(value: &Value) -> Result<SpecValue, std::io::Error> {
    match &value.rep {
        ValueRep::Unit => Ok(SpecValue::Unit(())),
        ValueRep::Bool(value) => Ok(SpecValue::Bool(*value)),
        ValueRep::U64(value) => Ok(SpecValue::U64(*value)),
        ValueRep::String(value) => Ok(SpecValue::String(value.to_string())),
        ValueRep::Array(items) => Ok(SpecValue::Array(
            items
                .iter()
                .map(eval2_value_to_spec)
                .collect::<Result<_, std::io::Error>>()?,
        )),
        ValueRep::Map(map) => Ok(SpecValue::Map(
            map.entries
                .iter()
                .map(|(key, value)| {
                    let ValueRep::String(key) = &key.rep else {
                        return Err(std::io::Error::other(
                            "only string-keyed maps can be converted to spec values",
                        ));
                    };
                    Ok((key.to_string(), eval2_value_to_spec(value)?))
                })
                .collect::<Result<_, std::io::Error>>()?,
        )),
        ValueRep::Struct(struct_value) => Ok(SpecValue::Struct(StructSpec {
            r#type: struct_value.type_name.to_string(),
            fields: struct_value
                .fields
                .iter()
                .map(|(name, value)| Ok((name.to_string(), eval2_value_to_spec(value)?)))
                .collect::<Result<_, std::io::Error>>()?,
        })),
        other => Err(std::io::Error::other(format!(
            "eval2 result value is not supported yet: {other:?}"
        ))),
    }
}

datatest_stable::harness! {
    { test = run_fixture, root = "../../specs/eval", pattern = r"(^|.*/)[^/]+\.styx$" },
}
