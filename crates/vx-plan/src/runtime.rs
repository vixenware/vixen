use crate::{
    RuleId, RulePlan, RulePlanEdge, RulePlanEdgeKind, RulePlanFact, RulePlanFactId, RulePlanNode,
    RulePlanNodeId, RulePlanNodeKind, RulePlanProvenance, RulePlanSource, RulePlanSourceId,
    RulePlanValue,
};
use camino::Utf8PathBuf;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlannerCapability {
    EmitGraph,
    Time,
    Random,
    Network,
    Filesystem,
    Process,
}

impl PlannerCapability {
    fn as_str(self) -> &'static str {
        match self {
            Self::EmitGraph => "emit-graph",
            Self::Time => "time",
            Self::Random => "random",
            Self::Network => "network",
            Self::Filesystem => "filesystem",
            Self::Process => "process",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlannerPolicy {
    allowed: BTreeSet<PlannerCapability>,
}

impl PlannerPolicy {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn with_allowed(capabilities: impl IntoIterator<Item = PlannerCapability>) -> Self {
        Self {
            allowed: capabilities.into_iter().collect(),
        }
    }

    pub fn allows(&self, capability: PlannerCapability) -> bool {
        self.allowed.contains(&capability)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerValue {
    Null,
    Bool(bool),
    Int(i64),
    String(String),
    Keyword(String),
    Vector(Vec<PlannerValue>),
    Set(BTreeSet<PlannerScalar>),
    Map(BTreeMap<String, PlannerValue>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlannerScalar {
    Bool(bool),
    Int(i64),
    String(String),
    Keyword(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerProgram {
    pub forms: Vec<PlannerExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerExpr {
    Value(PlannerValue),
    Call(PlannerCall),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerCall {
    pub builtin: String,
    pub args: Vec<PlannerExpr>,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerBuiltin {
    pub name: &'static str,
    pub capability: PlannerCapability,
}

#[derive(Debug, Clone)]
pub struct PlannerCallSite {
    pub builtin: String,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerStackFrame {
    pub builtin: String,
    pub rule_id: RuleId,
    pub source_id: RulePlanSourceId,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerDiagnostic {
    pub message: String,
    pub rule_id: RuleId,
    pub source_id: RulePlanSourceId,
    pub line: u32,
    pub column: u32,
    pub stack: Vec<PlannerStackFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerRuntimeError {
    pub diagnostic: PlannerDiagnostic,
}

impl std::fmt::Display for PlannerRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let d = &self.diagnostic;
        write!(
            f,
            "{} (rule={}, source={}, at {}:{})",
            d.message, d.rule_id, d.source_id, d.line, d.column
        )?;
        if !d.stack.is_empty() {
            write!(f, " stack=")?;
            for (idx, frame) in d.stack.iter().enumerate() {
                if idx > 0 {
                    write!(f, " -> ")?;
                }
                write!(
                    f,
                    "{}@{}:{}:{}",
                    frame.builtin, frame.source_id, frame.line, frame.column
                )?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for PlannerRuntimeError {}

pub trait PlannerHost {
    fn register_builtin(&mut self, builtin: PlannerBuiltin) -> Result<(), String>;
    fn invoke_builtin(
        &mut self,
        name: &str,
        args: &[PlannerValue],
        site: &PlannerCallSite,
    ) -> Result<PlannerValue, String>;
}

pub struct PlannerRuntime<H: PlannerHost> {
    host: H,
    policy: PlannerPolicy,
    builtins: BTreeMap<String, PlannerCapability>,
    stack: Vec<PlannerStackFrame>,
}

impl<H: PlannerHost> PlannerRuntime<H> {
    pub fn new(
        mut host: H,
        policy: PlannerPolicy,
        builtins: impl IntoIterator<Item = PlannerBuiltin>,
    ) -> Result<Self, String> {
        let mut builtin_map = BTreeMap::new();
        for builtin in builtins {
            host.register_builtin(builtin)?;
            if builtin_map
                .insert(builtin.name.to_string(), builtin.capability)
                .is_some()
            {
                return Err(format!("duplicate builtin registration: {}", builtin.name));
            }
        }

        Ok(Self {
            host,
            policy,
            builtins: builtin_map,
            stack: Vec::new(),
        })
    }

    pub fn evaluate_program(
        &mut self,
        program: &PlannerProgram,
    ) -> Result<Vec<PlannerValue>, PlannerRuntimeError> {
        let mut values = Vec::with_capacity(program.forms.len());
        for form in &program.forms {
            values.push(self.evaluate_expr(form)?);
        }
        Ok(values)
    }

    pub fn into_host(self) -> H {
        self.host
    }

    fn evaluate_expr(&mut self, expr: &PlannerExpr) -> Result<PlannerValue, PlannerRuntimeError> {
        match expr {
            PlannerExpr::Value(value) => Ok(value.clone()),
            PlannerExpr::Call(call) => self.evaluate_call(call),
        }
    }

    fn evaluate_call(&mut self, call: &PlannerCall) -> Result<PlannerValue, PlannerRuntimeError> {
        let capability = self.builtins.get(&call.builtin).copied().ok_or_else(|| {
            self.error_for_call(call, format!("unknown builtin: {}", call.builtin))
        })?;

        if !self.policy.allows(capability) {
            return Err(self.error_for_call(
                call,
                format!(
                    "builtin '{}' requires denied capability '{}'",
                    call.builtin,
                    capability.as_str()
                ),
            ));
        }

        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            args.push(self.evaluate_expr(arg)?);
        }

        self.stack.push(PlannerStackFrame {
            builtin: call.builtin.clone(),
            rule_id: call.provenance.rule_id.clone(),
            source_id: call.provenance.source_id.clone(),
            line: call.provenance.span.line,
            column: call.provenance.span.column,
        });

        let site = PlannerCallSite {
            builtin: call.builtin.clone(),
            provenance: call.provenance.clone(),
        };
        let result = self
            .host
            .invoke_builtin(&call.builtin, &args, &site)
            .map_err(|msg| self.error_for_call(call, msg));

        self.stack.pop();
        result
    }

    fn error_for_call(&self, call: &PlannerCall, message: String) -> PlannerRuntimeError {
        PlannerRuntimeError {
            diagnostic: PlannerDiagnostic {
                message,
                rule_id: call.provenance.rule_id.clone(),
                source_id: call.provenance.source_id.clone(),
                line: call.provenance.span.line,
                column: call.provenance.span.column,
                stack: self.stack.clone(),
            },
        }
    }
}

pub const BUILTIN_EMIT_NODE: PlannerBuiltin = PlannerBuiltin {
    name: "emit-node",
    capability: PlannerCapability::EmitGraph,
};

pub const BUILTIN_EMIT_EDGE: PlannerBuiltin = PlannerBuiltin {
    name: "emit-edge",
    capability: PlannerCapability::EmitGraph,
};

pub const BUILTIN_EMIT_FACT: PlannerBuiltin = PlannerBuiltin {
    name: "emit-fact",
    capability: PlannerCapability::EmitGraph,
};

pub const BUILTIN_CLOCK_NOW: PlannerBuiltin = PlannerBuiltin {
    name: "clock-now",
    capability: PlannerCapability::Time,
};

pub fn planner_builtins() -> [PlannerBuiltin; 4] {
    [
        BUILTIN_EMIT_NODE,
        BUILTIN_EMIT_EDGE,
        BUILTIN_EMIT_FACT,
        BUILTIN_CLOCK_NOW,
    ]
}

#[derive(Debug, Clone)]
pub struct RulePlanHost {
    registered: BTreeMap<String, PlannerCapability>,
    pub plan: RulePlan,
}

impl RulePlanHost {
    pub fn with_sources(sources: Vec<RulePlanSource>) -> Self {
        Self {
            registered: BTreeMap::new(),
            plan: RulePlan {
                sources,
                nodes: Vec::new(),
                edges: Vec::new(),
                facts: Vec::new(),
            },
        }
    }

    pub fn into_plan(self) -> RulePlan {
        self.plan
    }
}

impl PlannerHost for RulePlanHost {
    fn register_builtin(&mut self, builtin: PlannerBuiltin) -> Result<(), String> {
        if self
            .registered
            .insert(builtin.name.to_string(), builtin.capability)
            .is_some()
        {
            return Err(format!("builtin already registered: {}", builtin.name));
        }
        Ok(())
    }

    fn invoke_builtin(
        &mut self,
        name: &str,
        args: &[PlannerValue],
        site: &PlannerCallSite,
    ) -> Result<PlannerValue, String> {
        if !self.registered.contains_key(name) {
            return Err(format!("builtin '{}' is not registered", name));
        }

        match name {
            "emit-node" => {
                let arg = exactly_one_arg(name, args)?;
                let map = as_map(arg, name)?;
                let id = required_string(map, "id", name)?;
                let kind = parse_node_kind(required_string(map, "kind", name)?)?;
                let label = required_string(map, "label", name)?;

                self.plan.nodes.push(RulePlanNode {
                    id: RulePlanNodeId::from(id.to_owned()),
                    kind,
                    label: label.to_owned(),
                    provenance: site.provenance.clone(),
                });

                Ok(PlannerValue::Keyword("ok".to_string()))
            }
            "emit-edge" => {
                let arg = exactly_one_arg(name, args)?;
                let map = as_map(arg, name)?;
                let from = required_string(map, "from", name)?;
                let to = required_string(map, "to", name)?;
                let kind = parse_edge_kind(required_string(map, "kind", name)?)?;

                self.plan.edges.push(RulePlanEdge {
                    from: RulePlanNodeId::from(from.to_owned()),
                    to: RulePlanNodeId::from(to.to_owned()),
                    kind,
                    provenance: site.provenance.clone(),
                });

                Ok(PlannerValue::Keyword("ok".to_string()))
            }
            "emit-fact" => {
                let arg = exactly_one_arg(name, args)?;
                let map = as_map(arg, name)?;
                let id = required_string(map, "id", name)?;
                let node_id = required_string(map, "node-id", name)?;
                let value = required_string(map, "value", name)?;

                self.plan.facts.push(RulePlanFact {
                    id: RulePlanFactId::from(id.to_owned()),
                    node_id: RulePlanNodeId::from(node_id.to_owned()),
                    value: RulePlanValue::from(value.to_owned()),
                    provenance: site.provenance.clone(),
                });

                Ok(PlannerValue::Keyword("ok".to_string()))
            }
            "clock-now" => Ok(PlannerValue::Int(1_734_810_000)),
            _ => Err(format!("unsupported builtin '{}'", name)),
        }
    }
}

fn exactly_one_arg<'a>(
    builtin: &str,
    args: &'a [PlannerValue],
) -> Result<&'a PlannerValue, String> {
    if args.len() == 1 {
        Ok(&args[0])
    } else {
        Err(format!(
            "builtin '{}' expected exactly one argument but got {}",
            builtin,
            args.len()
        ))
    }
}

fn as_map<'a>(
    value: &'a PlannerValue,
    builtin: &str,
) -> Result<&'a BTreeMap<String, PlannerValue>, String> {
    match value {
        PlannerValue::Map(map) => Ok(map),
        _ => Err(format!("builtin '{}' expected a map argument", builtin)),
    }
}

fn required_string<'a>(
    map: &'a BTreeMap<String, PlannerValue>,
    key: &str,
    builtin: &str,
) -> Result<&'a str, String> {
    let value = map
        .get(key)
        .ok_or_else(|| format!("builtin '{}' missing required key '{}'", builtin, key))?;
    match value {
        PlannerValue::String(s) => Ok(s.as_str()),
        PlannerValue::Keyword(s) => Ok(s.as_str()),
        _ => Err(format!(
            "builtin '{}' key '{}' must be a string/keyword",
            builtin, key
        )),
    }
}

fn parse_node_kind(raw: &str) -> Result<RulePlanNodeKind, String> {
    match raw {
        "action" => Ok(RulePlanNodeKind::Action),
        "artifact" => Ok(RulePlanNodeKind::Artifact),
        "fact" => Ok(RulePlanNodeKind::Fact),
        "group" => Ok(RulePlanNodeKind::Group),
        _ => Err(format!("unknown node kind '{}'", raw)),
    }
}

fn parse_edge_kind(raw: &str) -> Result<RulePlanEdgeKind, String> {
    match raw {
        "ifchanged" => Ok(RulePlanEdgeKind::IfChanged),
        "ifwritten" => Ok(RulePlanEdgeKind::IfWritten),
        "ifcreate" => Ok(RulePlanEdgeKind::IfCreate),
        "ifdeleted" => Ok(RulePlanEdgeKind::IfDeleted),
        "always" => Ok(RulePlanEdgeKind::Always),
        _ => Err(format!("unknown edge kind '{}'", raw)),
    }
}

pub fn default_source() -> RulePlanSource {
    RulePlanSource {
        id: RulePlanSourceId::from("runtime".to_string()),
        path: Utf8PathBuf::from("planner/runtime.edn"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RulePlanSpan;

    fn provenance(rule_id: &str, line: u32) -> RulePlanProvenance {
        RulePlanProvenance {
            rule_id: RuleId::from(rule_id.to_string()),
            source_id: RulePlanSourceId::from("runtime".to_string()),
            span: RulePlanSpan {
                line,
                column: 1,
                end_line: line,
                end_column: 20,
            },
            comment: None,
            metadata: BTreeMap::new(),
        }
    }

    fn emit_node_call(id: &str, kind: &str, label: &str, line: u32) -> PlannerExpr {
        PlannerExpr::Call(PlannerCall {
            builtin: "emit-node".to_string(),
            args: vec![PlannerExpr::Value(PlannerValue::Map(BTreeMap::from([
                ("id".to_string(), PlannerValue::String(id.to_string())),
                ("kind".to_string(), PlannerValue::Keyword(kind.to_string())),
                ("label".to_string(), PlannerValue::String(label.to_string())),
            ])))],
            provenance: provenance("rule.emit.node", line),
        })
    }

    fn emit_edge_call(from: &str, to: &str, kind: &str, line: u32) -> PlannerExpr {
        PlannerExpr::Call(PlannerCall {
            builtin: "emit-edge".to_string(),
            args: vec![PlannerExpr::Value(PlannerValue::Map(BTreeMap::from([
                ("from".to_string(), PlannerValue::String(from.to_string())),
                ("to".to_string(), PlannerValue::String(to.to_string())),
                ("kind".to_string(), PlannerValue::Keyword(kind.to_string())),
            ])))],
            provenance: provenance("rule.emit.edge", line),
        })
    }

    fn emit_fact_call(id: &str, node_id: &str, value: &str, line: u32) -> PlannerExpr {
        PlannerExpr::Call(PlannerCall {
            builtin: "emit-fact".to_string(),
            args: vec![PlannerExpr::Value(PlannerValue::Map(BTreeMap::from([
                ("id".to_string(), PlannerValue::String(id.to_string())),
                (
                    "node-id".to_string(),
                    PlannerValue::String(node_id.to_string()),
                ),
                ("value".to_string(), PlannerValue::String(value.to_string())),
            ])))],
            provenance: provenance("rule.emit.fact", line),
        })
    }

    #[test]
    fn runtime_vertical_slice_emits_valid_rule_plan() {
        let program = PlannerProgram {
            forms: vec![
                emit_node_call("n_group", "group", "build", 1),
                emit_node_call("n_action", "action", "compile", 2),
                emit_node_call("n_artifact", "artifact", "target/app", 3),
                emit_node_call("n_fact", "fact", "src-changed", 4),
                emit_edge_call("n_group", "n_action", "always", 5),
                emit_edge_call("n_action", "n_artifact", "ifwritten", 6),
                emit_edge_call("n_fact", "n_action", "ifchanged", 7),
                emit_fact_call("f_src_changed", "n_fact", "true", 8),
            ],
        };

        let host = RulePlanHost::with_sources(vec![default_source()]);
        let policy = PlannerPolicy::with_allowed([PlannerCapability::EmitGraph]);
        let mut runtime =
            PlannerRuntime::new(host, policy, planner_builtins()).expect("runtime should build");

        runtime
            .evaluate_program(&program)
            .expect("program should evaluate");
        let plan = runtime.into_host().into_plan();
        plan.validate().expect("plan should validate");
    }

    #[test]
    fn runtime_is_deterministic_for_same_program() {
        let program = PlannerProgram {
            forms: vec![
                emit_node_call("n_action", "action", "compile", 1),
                emit_node_call("n_group", "group", "build", 2),
                emit_edge_call("n_group", "n_action", "always", 3),
            ],
        };

        let run_once = || {
            let host = RulePlanHost::with_sources(vec![default_source()]);
            let policy = PlannerPolicy::with_allowed([PlannerCapability::EmitGraph]);
            let mut runtime = PlannerRuntime::new(host, policy, planner_builtins())
                .expect("runtime should build");
            runtime
                .evaluate_program(&program)
                .expect("program should evaluate");
            runtime
                .into_host()
                .into_plan()
                .to_canonical_json()
                .expect("canonical plan json")
        };

        assert_eq!(run_once(), run_once());
    }

    #[test]
    fn capability_violation_has_source_mapped_diagnostics() {
        let program = PlannerProgram {
            forms: vec![PlannerExpr::Call(PlannerCall {
                builtin: "clock-now".to_string(),
                args: Vec::new(),
                provenance: provenance("rule.time.read", 42),
            })],
        };

        let host = RulePlanHost::with_sources(vec![default_source()]);
        let policy = PlannerPolicy::with_allowed([PlannerCapability::EmitGraph]);
        let mut runtime =
            PlannerRuntime::new(host, policy, planner_builtins()).expect("runtime should build");

        let err = runtime
            .evaluate_program(&program)
            .expect_err("time capability should be denied");

        assert!(
            err.diagnostic
                .message
                .contains("requires denied capability 'time'")
        );
        assert_eq!(err.diagnostic.rule_id.as_str(), "rule.time.read");
        assert_eq!(err.diagnostic.source_id.as_str(), "runtime");
        assert_eq!(err.diagnostic.line, 42);
        assert_eq!(err.diagnostic.column, 1);
    }
}
