use crate::runtime::{PlannerBuiltinId, PlannerExpr, PlannerProgram, PlannerValue};
use crate::{RuleId, RulePlanProvenance, RulePlanSourceId, RulePlanSpan};
use camino::Utf8Path;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VxParseError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

impl std::fmt::Display for VxParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at {}:{})", self.message, self.line, self.column)
    }
}

impl std::error::Error for VxParseError {}

pub fn parse_vx_program(
    source_path: &Utf8Path,
    source_id: RulePlanSourceId,
    input: &str,
) -> Result<PlannerProgram, VxParseError> {
    let mut forms = Vec::new();

    for (line_idx, raw_line) in input.lines().enumerate() {
        let line = (line_idx + 1) as u32;
        let trimmed = strip_comment(raw_line).trim();
        if trimmed.is_empty() {
            continue;
        }

        let tokens = split_tokens(trimmed).map_err(|e| VxParseError {
            message: e,
            line,
            column: 1,
        })?;
        let Some((builtin_raw, _col)) = tokens.first() else {
            continue;
        };

        let builtin = builtin_from_str(builtin_raw).ok_or_else(|| VxParseError {
            message: format!("unknown builtin '{}'", builtin_raw),
            line,
            column: 1,
        })?;

        if !matches!(
            builtin,
            PlannerBuiltinId::EmitNode | PlannerBuiltinId::EmitEdge | PlannerBuiltinId::EmitFact
        ) {
            return Err(VxParseError {
                message: format!(
                    "builtin '{}' is not supported in .vx files yet",
                    builtin.as_str()
                ),
                line,
                column: 1,
            });
        }

        let mut map = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for (token, col) in tokens.iter().skip(1) {
            let (key, value_raw) = token.split_once('=').ok_or_else(|| VxParseError {
                message: format!("expected key=value token, got '{token}'"),
                line,
                column: *col,
            })?;
            if key.is_empty() {
                return Err(VxParseError {
                    message: "expected key=value token with non-empty key".to_string(),
                    line,
                    column: *col,
                });
            }
            if !seen.insert(key.to_string()) {
                return Err(VxParseError {
                    message: format!("duplicate key '{key}'"),
                    line,
                    column: *col,
                });
            }

            let value = parse_value(value_raw).map_err(|msg| VxParseError {
                message: msg,
                line,
                column: *col,
            })?;
            map.insert(key.to_string(), value);
        }

        let provenance = RulePlanProvenance {
            rule_id: RuleId::from(format!("rule.vx.{}", builtin.as_str())),
            source_id: source_id.clone(),
            span: RulePlanSpan {
                line,
                column: 1,
                end_line: line,
                end_column: raw_line.len().max(1) as u32,
            },
            comment: None,
            metadata: BTreeMap::new(),
        };

        // All supported builtins take exactly one map argument.
        forms.push(PlannerExpr::Call(crate::runtime::PlannerCall {
            builtin,
            args: vec![PlannerExpr::Value(PlannerValue::Map(map))],
            provenance,
        }));
    }

    // Ensure the source path is referenced at least once in error messages.
    let _ = source_path;

    Ok(PlannerProgram { forms })
}

fn strip_comment(line: &str) -> &str {
    for (idx, ch) in line.char_indices() {
        if ch == ';' || ch == '#' {
            return &line[..idx];
        }
    }
    line
}

fn split_tokens(input: &str) -> Result<Vec<(String, u32)>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escape = false;
    let mut token_col = 1u32;

    for (idx, ch) in input.chars().enumerate() {
        let col = (idx + 1) as u32;
        if in_string {
            if escape {
                current.push(match ch {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '"' => '"',
                    '\\' => '\\',
                    other => other,
                });
                escape = false;
                continue;
            }
            match ch {
                '\\' => escape = true,
                '"' => in_string = false,
                other => current.push(other),
            }
            continue;
        }

        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push((current.clone(), token_col));
                current.clear();
            }
            continue;
        }

        if current.is_empty() {
            token_col = col;
        }

        if ch == '"' {
            in_string = true;
            continue;
        }

        current.push(ch);
    }

    if escape {
        return Err("unterminated escape sequence".to_string());
    }
    if in_string {
        return Err("unterminated string literal".to_string());
    }
    if !current.is_empty() {
        tokens.push((current, token_col));
    }

    Ok(tokens)
}

fn builtin_from_str(raw: &str) -> Option<PlannerBuiltinId> {
    match raw {
        "emit-node" => Some(PlannerBuiltinId::EmitNode),
        "emit-edge" => Some(PlannerBuiltinId::EmitEdge),
        "emit-fact" => Some(PlannerBuiltinId::EmitFact),
        "sort-map-keys" => Some(PlannerBuiltinId::SortMapKeys),
        "sort-scalars" => Some(PlannerBuiltinId::SortScalars),
        "clock-now" => Some(PlannerBuiltinId::ClockNow),
        "random-int" => Some(PlannerBuiltinId::RandomInt),
        "net-request" => Some(PlannerBuiltinId::NetRequest),
        "fs-read" => Some(PlannerBuiltinId::FsRead),
        "process-spawn" => Some(PlannerBuiltinId::ProcessSpawn),
        _ => None,
    }
}

fn parse_value(raw: &str) -> Result<PlannerValue, String> {
    if raw.is_empty() {
        return Err("expected non-empty value".to_string());
    }
    if let Some(keyword) = raw.strip_prefix(':') {
        return Ok(PlannerValue::Keyword(keyword.to_string()));
    }
    match raw {
        "null" => return Ok(PlannerValue::Null),
        "true" => return Ok(PlannerValue::Bool(true)),
        "false" => return Ok(PlannerValue::Bool(false)),
        _ => {}
    }
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(PlannerValue::Int(value));
    }
    Ok(PlannerValue::String(raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_emit_node_line() {
        let program = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            "emit-node id=n1 kind=action label=\"compile\"",
        )
        .expect("parse should succeed");
        assert_eq!(program.forms.len(), 1);
    }

    #[test]
    fn parses_keywords_and_strings() {
        let program = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            "emit-node id=n1 kind=:action label=compile",
        )
        .expect("parse should succeed");
        assert_eq!(program.forms.len(), 1);
    }

    #[test]
    fn rejects_unknown_builtin() {
        let err = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            "wat id=n1",
        )
        .unwrap_err();
        assert!(err.message.contains("unknown builtin"));
    }
}
