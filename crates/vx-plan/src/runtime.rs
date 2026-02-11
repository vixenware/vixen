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
    Transform,
    Env,
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
            Self::Transform => "transform",
            Self::Env => "env",
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

    pub fn deterministic() -> Self {
        Self::with_allowed([
            PlannerCapability::EmitGraph,
            PlannerCapability::Transform,
            PlannerCapability::Env,
        ])
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

impl PlannerScalar {
    fn from_value(value: &PlannerValue) -> Option<Self> {
        match value {
            PlannerValue::Bool(v) => Some(Self::Bool(*v)),
            PlannerValue::Int(v) => Some(Self::Int(*v)),
            PlannerValue::String(v) => Some(Self::String(v.clone())),
            PlannerValue::Keyword(v) => Some(Self::Keyword(v.clone())),
            _ => None,
        }
    }

    fn into_value(self) -> PlannerValue {
        match self {
            Self::Bool(v) => PlannerValue::Bool(v),
            Self::Int(v) => PlannerValue::Int(v),
            Self::String(v) => PlannerValue::String(v),
            Self::Keyword(v) => PlannerValue::Keyword(v),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerProgram {
    pub forms: Vec<PlannerExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerExpr {
    Value(PlannerValue),
    VarRef {
        name: String,
        provenance: RulePlanProvenance,
    },
    VectorLiteral {
        items: Vec<PlannerExpr>,
        provenance: RulePlanProvenance,
    },
    MapLiteral {
        entries: BTreeMap<String, PlannerExpr>,
        provenance: RulePlanProvenance,
    },
    BinaryOp {
        op: PlannerBinaryOp,
        lhs: Box<PlannerExpr>,
        rhs: Box<PlannerExpr>,
        provenance: RulePlanProvenance,
    },
    Call(PlannerCall),
    Let {
        name: String,
        value: Box<PlannerExpr>,
        provenance: RulePlanProvenance,
    },
    If {
        cond: Box<PlannerExpr>,
        then_branch: Vec<PlannerExpr>,
        else_branch: Vec<PlannerExpr>,
        provenance: RulePlanProvenance,
    },
    ForEach {
        name: String,
        iter: Box<PlannerExpr>,
        body: Vec<PlannerExpr>,
        provenance: RulePlanProvenance,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerBinaryOp {
    Eq,
    Neq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerCall {
    pub builtin: PlannerBuiltinId,
    pub args: Vec<PlannerExpr>,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlannerBuiltinId {
    EmitNode,
    EmitEdge,
    EmitFact,
    SortMapKeys,
    SortScalars,
    Get,
    Len,
    StrCat,
    RustCrateGraph,
    RustRoot,
    RustSelect,
    RustTransitiveClosure,
    EnvProfile,
    EnvTargetTriple,
    ClockNow,
    RandomInt,
    NetRequest,
    FsRead,
    ProcessSpawn,
}

impl PlannerBuiltinId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmitNode => "emit-node",
            Self::EmitEdge => "emit-edge",
            Self::EmitFact => "emit-fact",
            Self::SortMapKeys => "sort-map-keys",
            Self::SortScalars => "sort-scalars",
            Self::Get => "get",
            Self::Len => "len",
            Self::StrCat => "str-cat",
            Self::RustCrateGraph => "rust-crate-graph",
            Self::RustRoot => "rust-root",
            Self::RustSelect => "rust-select",
            Self::RustTransitiveClosure => "rust-transitive-closure",
            Self::EnvProfile => "env-profile",
            Self::EnvTargetTriple => "env-target-triple",
            Self::ClockNow => "clock-now",
            Self::RandomInt => "random-int",
            Self::NetRequest => "net-request",
            Self::FsRead => "fs-read",
            Self::ProcessSpawn => "process-spawn",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerBuiltin {
    pub id: PlannerBuiltinId,
    pub capability: PlannerCapability,
}

#[derive(Debug, Clone)]
pub struct PlannerCallSite {
    pub builtin: PlannerBuiltinId,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerStackFrame {
    pub builtin: PlannerBuiltinId,
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
                    frame.builtin.as_str(),
                    frame.source_id,
                    frame.line,
                    frame.column
                )?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for PlannerRuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitNodeArgs {
    pub id: String,
    pub kind: RulePlanNodeKind,
    pub label: String,
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitEdgeArgs {
    pub from: String,
    pub to: String,
    pub kind: RulePlanEdgeKind,
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitFactArgs {
    pub id: String,
    pub node_id: String,
    pub value: String,
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortMapKeysArgs {
    pub map: BTreeMap<String, PlannerValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortScalarsArgs {
    pub values: Vec<PlannerScalar>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetArgs {
    pub value: PlannerValue,
    pub key: PlannerValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LenArgs {
    pub value: PlannerValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrCatArgs {
    pub values: Vec<PlannerValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustRootArgs {
    pub graph: PlannerValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustSelectArgs {
    pub graph: PlannerValue,
    pub name: String,
    pub crate_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustTransitiveClosureArgs {
    pub graph: PlannerValue,
    pub roots: Vec<PlannerValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerBuiltinArgs {
    None,
    EmitNode(EmitNodeArgs),
    EmitEdge(EmitEdgeArgs),
    EmitFact(EmitFactArgs),
    SortMapKeys(SortMapKeysArgs),
    SortScalars(SortScalarsArgs),
    Get(GetArgs),
    Len(LenArgs),
    StrCat(StrCatArgs),
    RustCrateGraph,
    RustRoot(RustRootArgs),
    RustSelect(RustSelectArgs),
    RustTransitiveClosure(RustTransitiveClosureArgs),
}

pub trait PlannerHost {
    fn register_builtin(&mut self, builtin: PlannerBuiltin) -> Result<(), String>;
    fn invoke_builtin(
        &mut self,
        builtin: PlannerBuiltinId,
        args: PlannerBuiltinArgs,
        site: &PlannerCallSite,
    ) -> Result<PlannerValue, String>;
}

pub struct PlannerRuntime<H: PlannerHost> {
    host: H,
    policy: PlannerPolicy,
    builtins: BTreeMap<PlannerBuiltinId, PlannerCapability>,
    stack: Vec<PlannerStackFrame>,
    scopes: Vec<BTreeMap<String, PlannerValue>>,
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
            if builtin_map.insert(builtin.id, builtin.capability).is_some() {
                return Err(format!(
                    "duplicate builtin registration: {}",
                    builtin.id.as_str()
                ));
            }
        }

        Ok(Self {
            host,
            policy,
            builtins: builtin_map,
            stack: Vec::new(),
            scopes: vec![BTreeMap::new()],
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
            PlannerExpr::VarRef { name, provenance } => self.lookup_var(name).ok_or_else(|| {
                self.error_for_provenance(
                    provenance,
                    format!("unknown variable reference: ${name}"),
                )
            }),
            PlannerExpr::VectorLiteral { items, .. } => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.evaluate_expr(item)?);
                }
                Ok(PlannerValue::Vector(out))
            }
            PlannerExpr::MapLiteral { entries, .. } => {
                let mut out = BTreeMap::new();
                for (k, v) in entries {
                    out.insert(k.clone(), self.evaluate_expr(v)?);
                }
                Ok(PlannerValue::Map(out))
            }
            PlannerExpr::BinaryOp {
                op,
                lhs,
                rhs,
                provenance: _,
            } => {
                let lhs_v = self.evaluate_expr(lhs)?;
                let rhs_v = self.evaluate_expr(rhs)?;
                let out = match op {
                    PlannerBinaryOp::Eq => lhs_v == rhs_v,
                    PlannerBinaryOp::Neq => lhs_v != rhs_v,
                };
                Ok(PlannerValue::Bool(out))
            }
            PlannerExpr::Call(call) => self.evaluate_call(call),
            PlannerExpr::Let {
                name,
                value,
                provenance: _,
            } => {
                let v = self.evaluate_expr(value)?;
                self.set_var(name.clone(), v.clone());
                Ok(v)
            }
            PlannerExpr::If {
                cond,
                then_branch,
                else_branch,
                provenance,
            } => {
                let cond_v = self.evaluate_expr(cond)?;
                let PlannerValue::Bool(b) = cond_v else {
                    return Err(self.error_for_provenance(
                        provenance,
                        "if condition must evaluate to bool".to_string(),
                    ));
                };
                if b {
                    self.evaluate_block(then_branch)
                } else {
                    self.evaluate_block(else_branch)
                }
            }
            PlannerExpr::ForEach {
                name,
                iter,
                body,
                provenance,
            } => {
                let iter_v = self.evaluate_expr(iter)?;
                let PlannerValue::Vector(items) = iter_v else {
                    return Err(self.error_for_provenance(
                        provenance,
                        "for loop iterable must evaluate to vector".to_string(),
                    ));
                };

                let mut results = Vec::with_capacity(items.len());
                for item in items {
                    self.push_scope();
                    self.set_var(name.clone(), item);
                    let v = self.evaluate_block(body)?;
                    results.push(v);
                    self.pop_scope();
                }

                Ok(PlannerValue::Vector(results))
            }
        }
    }

    fn evaluate_block(
        &mut self,
        exprs: &[PlannerExpr],
    ) -> Result<PlannerValue, PlannerRuntimeError> {
        let mut last = PlannerValue::Null;
        for expr in exprs {
            last = self.evaluate_expr(expr)?;
        }
        Ok(last)
    }

    fn evaluate_call(&mut self, call: &PlannerCall) -> Result<PlannerValue, PlannerRuntimeError> {
        let capability = self.builtins.get(&call.builtin).copied().ok_or_else(|| {
            self.error_for_call(call, format!("unknown builtin: {}", call.builtin.as_str()))
        })?;

        if !self.policy.allows(capability) {
            return Err(self.error_for_call(
                call,
                format!(
                    "builtin '{}' requires denied capability '{}'",
                    call.builtin.as_str(),
                    capability.as_str()
                ),
            ));
        }

        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            args.push(self.evaluate_expr(arg)?);
        }

        let parsed_args = parse_builtin_args(call.builtin, &args)
            .map_err(|msg| self.error_for_call(call, msg))?;

        self.stack.push(PlannerStackFrame {
            builtin: call.builtin,
            rule_id: call.provenance.rule_id.clone(),
            source_id: call.provenance.source_id.clone(),
            line: call.provenance.span.line,
            column: call.provenance.span.column,
        });

        let site = PlannerCallSite {
            builtin: call.builtin,
            provenance: call.provenance.clone(),
        };
        let result = self
            .host
            .invoke_builtin(call.builtin, parsed_args, &site)
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

    fn error_for_provenance(
        &self,
        provenance: &RulePlanProvenance,
        message: String,
    ) -> PlannerRuntimeError {
        PlannerRuntimeError {
            diagnostic: PlannerDiagnostic {
                message,
                rule_id: provenance.rule_id.clone(),
                source_id: provenance.source_id.clone(),
                line: provenance.span.line,
                column: provenance.span.column,
                stack: self.stack.clone(),
            },
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop_scope(&mut self) {
        // The outermost scope must always exist.
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    fn set_var(&mut self, name: String, value: PlannerValue) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, value);
        }
    }

    fn lookup_var(&self, name: &str) -> Option<PlannerValue> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v.clone());
            }
        }
        None
    }
}

pub const BUILTIN_EMIT_NODE: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::EmitNode,
    capability: PlannerCapability::EmitGraph,
};

pub const BUILTIN_EMIT_EDGE: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::EmitEdge,
    capability: PlannerCapability::EmitGraph,
};

pub const BUILTIN_EMIT_FACT: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::EmitFact,
    capability: PlannerCapability::EmitGraph,
};

pub const BUILTIN_SORT_MAP_KEYS: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::SortMapKeys,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_SORT_SCALARS: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::SortScalars,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_GET: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::Get,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_LEN: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::Len,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_STR_CAT: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::StrCat,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_ENV_PROFILE: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::EnvProfile,
    capability: PlannerCapability::Env,
};

pub const BUILTIN_ENV_TARGET_TRIPLE: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::EnvTargetTriple,
    capability: PlannerCapability::Env,
};

pub const BUILTIN_CLOCK_NOW: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::ClockNow,
    capability: PlannerCapability::Time,
};

pub const BUILTIN_RANDOM_INT: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::RandomInt,
    capability: PlannerCapability::Random,
};

pub const BUILTIN_NET_REQUEST: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::NetRequest,
    capability: PlannerCapability::Network,
};

pub const BUILTIN_FS_READ: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::FsRead,
    capability: PlannerCapability::Filesystem,
};

pub const BUILTIN_PROCESS_SPAWN: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::ProcessSpawn,
    capability: PlannerCapability::Process,
};

pub fn planner_builtins() -> [PlannerBuiltin; 15] {
    [
        BUILTIN_EMIT_NODE,
        BUILTIN_EMIT_EDGE,
        BUILTIN_EMIT_FACT,
        BUILTIN_SORT_MAP_KEYS,
        BUILTIN_SORT_SCALARS,
        BUILTIN_GET,
        BUILTIN_LEN,
        BUILTIN_STR_CAT,
        BUILTIN_ENV_PROFILE,
        BUILTIN_ENV_TARGET_TRIPLE,
        BUILTIN_CLOCK_NOW,
        BUILTIN_RANDOM_INT,
        BUILTIN_NET_REQUEST,
        BUILTIN_FS_READ,
        BUILTIN_PROCESS_SPAWN,
    ]
}

pub const BUILTIN_RUST_CRATE_GRAPH: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::RustCrateGraph,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_RUST_ROOT: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::RustRoot,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_RUST_SELECT: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::RustSelect,
    capability: PlannerCapability::Transform,
};

pub const BUILTIN_RUST_TRANSITIVE_CLOSURE: PlannerBuiltin = PlannerBuiltin {
    id: PlannerBuiltinId::RustTransitiveClosure,
    capability: PlannerCapability::Transform,
};

pub fn planner_builtins_with_rust() -> [PlannerBuiltin; 19] {
    [
        BUILTIN_EMIT_NODE,
        BUILTIN_EMIT_EDGE,
        BUILTIN_EMIT_FACT,
        BUILTIN_SORT_MAP_KEYS,
        BUILTIN_SORT_SCALARS,
        BUILTIN_GET,
        BUILTIN_LEN,
        BUILTIN_STR_CAT,
        BUILTIN_RUST_CRATE_GRAPH,
        BUILTIN_RUST_ROOT,
        BUILTIN_RUST_SELECT,
        BUILTIN_RUST_TRANSITIVE_CLOSURE,
        BUILTIN_ENV_PROFILE,
        BUILTIN_ENV_TARGET_TRIPLE,
        BUILTIN_CLOCK_NOW,
        BUILTIN_RANDOM_INT,
        BUILTIN_NET_REQUEST,
        BUILTIN_FS_READ,
        BUILTIN_PROCESS_SPAWN,
    ]
}

#[derive(Debug, Clone)]
pub struct RulePlanHost {
    registered: BTreeSet<PlannerBuiltinId>,
    pub plan: RulePlan,
    env_profile: Option<String>,
    env_target_triple: Option<String>,
}

impl RulePlanHost {
    pub fn with_sources(sources: Vec<RulePlanSource>) -> Self {
        Self {
            registered: BTreeSet::new(),
            plan: RulePlan {
                sources,
                nodes: Vec::new(),
                edges: Vec::new(),
                facts: Vec::new(),
            },
            env_profile: None,
            env_target_triple: None,
        }
    }

    pub fn set_env_profile(&mut self, profile: impl Into<String>) {
        self.env_profile = Some(profile.into());
    }

    pub fn set_env_target_triple(&mut self, target_triple: impl Into<String>) {
        self.env_target_triple = Some(target_triple.into());
    }

    pub fn into_plan(self) -> RulePlan {
        self.plan
    }
}

impl PlannerHost for RulePlanHost {
    fn register_builtin(&mut self, builtin: PlannerBuiltin) -> Result<(), String> {
        if !self.registered.insert(builtin.id) {
            return Err(format!(
                "builtin already registered: {}",
                builtin.id.as_str()
            ));
        }
        Ok(())
    }

    fn invoke_builtin(
        &mut self,
        builtin: PlannerBuiltinId,
        args: PlannerBuiltinArgs,
        site: &PlannerCallSite,
    ) -> Result<PlannerValue, String> {
        if !self.registered.contains(&builtin) {
            return Err(format!("builtin '{}' is not registered", builtin.as_str()));
        }

        match (builtin, args) {
            (PlannerBuiltinId::EmitNode, PlannerBuiltinArgs::EmitNode(args)) => {
                let mut provenance = site.provenance.clone();
                provenance.metadata.extend(args.meta);
                self.plan.nodes.push(RulePlanNode {
                    id: RulePlanNodeId::from(args.id),
                    kind: args.kind,
                    label: args.label,
                    provenance,
                });
                Ok(PlannerValue::Keyword("ok".to_string()))
            }
            (PlannerBuiltinId::EmitEdge, PlannerBuiltinArgs::EmitEdge(args)) => {
                let mut provenance = site.provenance.clone();
                provenance.metadata.extend(args.meta);
                self.plan.edges.push(RulePlanEdge {
                    from: RulePlanNodeId::from(args.from),
                    to: RulePlanNodeId::from(args.to),
                    kind: args.kind,
                    provenance,
                });
                Ok(PlannerValue::Keyword("ok".to_string()))
            }
            (PlannerBuiltinId::EmitFact, PlannerBuiltinArgs::EmitFact(args)) => {
                let mut provenance = site.provenance.clone();
                provenance.metadata.extend(args.meta);
                self.plan.facts.push(RulePlanFact {
                    id: RulePlanFactId::from(args.id),
                    node_id: RulePlanNodeId::from(args.node_id),
                    value: RulePlanValue::from(args.value),
                    provenance,
                });
                Ok(PlannerValue::Keyword("ok".to_string()))
            }
            (PlannerBuiltinId::SortMapKeys, PlannerBuiltinArgs::SortMapKeys(args)) => {
                let keys = args
                    .map
                    .keys()
                    .cloned()
                    .map(PlannerValue::String)
                    .collect::<Vec<_>>();
                Ok(PlannerValue::Vector(keys))
            }
            (PlannerBuiltinId::SortScalars, PlannerBuiltinArgs::SortScalars(mut args)) => {
                args.values.sort();
                Ok(PlannerValue::Vector(
                    args.values
                        .into_iter()
                        .map(PlannerScalar::into_value)
                        .collect(),
                ))
            }
            (PlannerBuiltinId::Get, PlannerBuiltinArgs::Get(args)) => {
                let key_as_string = match &args.key {
                    PlannerValue::String(s) => Some(s.as_str()),
                    PlannerValue::Keyword(s) => Some(s.as_str()),
                    _ => None,
                };

                match args.value {
                    PlannerValue::Map(map) => Ok(key_as_string
                        .and_then(|k| map.get(k).cloned())
                        .unwrap_or(PlannerValue::Null)),
                    PlannerValue::Vector(values) => match args.key {
                        PlannerValue::Int(idx) => {
                            if idx < 0 {
                                return Ok(PlannerValue::Null);
                            }
                            let idx = idx as usize;
                            Ok(values.get(idx).cloned().unwrap_or(PlannerValue::Null))
                        }
                        _ => Err("get(vector) expects key to be an int index".to_string()),
                    },
                    _ => Err("get(value) expects value to be map or vector".to_string()),
                }
            }
            (PlannerBuiltinId::Len, PlannerBuiltinArgs::Len(args)) => match args.value {
                PlannerValue::Vector(v) => Ok(PlannerValue::Int(v.len() as i64)),
                PlannerValue::Map(m) => Ok(PlannerValue::Int(m.len() as i64)),
                PlannerValue::Set(s) => Ok(PlannerValue::Int(s.len() as i64)),
                PlannerValue::String(s) => Ok(PlannerValue::Int(s.chars().count() as i64)),
                _ => Err("len(value) expects value to be string/vector/map/set".to_string()),
            },
            (PlannerBuiltinId::StrCat, PlannerBuiltinArgs::StrCat(args)) => {
                let mut out = String::new();
                for (idx, v) in args.values.iter().enumerate() {
                    let s = match v {
                        PlannerValue::Null => "null".to_string(),
                        PlannerValue::Bool(b) => b.to_string(),
                        PlannerValue::Int(i) => i.to_string(),
                        PlannerValue::String(s) => s.clone(),
                        PlannerValue::Keyword(s) => s.clone(),
                        PlannerValue::Vector(_) | PlannerValue::Set(_) | PlannerValue::Map(_) => {
                            return Err(format!(
                                "str-cat values[{idx}] must be a scalar, got {v:?}"
                            ));
                        }
                    };
                    out.push_str(&s);
                }
                Ok(PlannerValue::String(out))
            }
            (PlannerBuiltinId::RustCrateGraph, PlannerBuiltinArgs::RustCrateGraph) => Err(
                "rust.crate_graph is host-provided and has no default implementation".to_string(),
            ),
            (PlannerBuiltinId::RustRoot, PlannerBuiltinArgs::RustRoot(args)) => {
                rust_root(&args.graph)
            }
            (PlannerBuiltinId::RustSelect, PlannerBuiltinArgs::RustSelect(args)) => {
                rust_select(&args.graph, &args.name, &args.crate_type)
            }
            (
                PlannerBuiltinId::RustTransitiveClosure,
                PlannerBuiltinArgs::RustTransitiveClosure(args),
            ) => rust_transitive_closure(&args.graph, &args.roots),
            (PlannerBuiltinId::EnvProfile, PlannerBuiltinArgs::None) => self
                .env_profile
                .clone()
                .map(PlannerValue::String)
                .ok_or_else(|| "env.profile is not set".to_string()),
            (PlannerBuiltinId::EnvTargetTriple, PlannerBuiltinArgs::None) => self
                .env_target_triple
                .clone()
                .map(PlannerValue::String)
                .ok_or_else(|| "env.target_triple is not set".to_string()),
            (PlannerBuiltinId::ClockNow, PlannerBuiltinArgs::None) => {
                Ok(PlannerValue::Int(1_734_810_000))
            }
            (PlannerBuiltinId::RandomInt, PlannerBuiltinArgs::None) => Ok(PlannerValue::Int(4)),
            (PlannerBuiltinId::NetRequest, PlannerBuiltinArgs::None)
            | (PlannerBuiltinId::FsRead, PlannerBuiltinArgs::None)
            | (PlannerBuiltinId::ProcessSpawn, PlannerBuiltinArgs::None) => Err(format!(
                "builtin '{}' has no implementation",
                builtin.as_str()
            )),
            (_, received) => Err(format!(
                "builtin '{}' received incompatible args: {:?}",
                builtin.as_str(),
                received
            )),
        }
    }
}

fn parse_builtin_args(
    builtin: PlannerBuiltinId,
    args: &[PlannerValue],
) -> Result<PlannerBuiltinArgs, String> {
    match builtin {
        PlannerBuiltinId::EmitNode => {
            let map = exactly_one_map_arg(builtin, args)?;
            let id = required_string(map, "id", builtin)?.to_owned();
            let kind = parse_node_kind(required_string(map, "kind", builtin)?)?;
            let label = required_string(map, "label", builtin)?.to_owned();
            let meta = optional_meta(map, builtin)?;
            Ok(PlannerBuiltinArgs::EmitNode(EmitNodeArgs {
                id,
                kind,
                label,
                meta,
            }))
        }
        PlannerBuiltinId::EmitEdge => {
            let map = exactly_one_map_arg(builtin, args)?;
            let from = required_string(map, "from", builtin)?.to_owned();
            let to = required_string(map, "to", builtin)?.to_owned();
            let kind = parse_edge_kind(required_string(map, "kind", builtin)?)?;
            let meta = optional_meta(map, builtin)?;
            Ok(PlannerBuiltinArgs::EmitEdge(EmitEdgeArgs {
                from,
                to,
                kind,
                meta,
            }))
        }
        PlannerBuiltinId::EmitFact => {
            let map = exactly_one_map_arg(builtin, args)?;
            let id = required_string(map, "id", builtin)?.to_owned();
            let node_id = required_string(map, "node-id", builtin)?.to_owned();
            let value = required_string(map, "value", builtin)?.to_owned();
            let meta = optional_meta(map, builtin)?;
            Ok(PlannerBuiltinArgs::EmitFact(EmitFactArgs {
                id,
                node_id,
                value,
                meta,
            }))
        }
        PlannerBuiltinId::SortMapKeys => {
            let map = exactly_one_map_arg(builtin, args)?;
            Ok(PlannerBuiltinArgs::SortMapKeys(SortMapKeysArgs {
                map: map.clone(),
            }))
        }
        PlannerBuiltinId::SortScalars => {
            let vector = exactly_one_vector_arg(builtin, args)?;
            let mut values = Vec::with_capacity(vector.len());
            for (idx, value) in vector.iter().enumerate() {
                let scalar = PlannerScalar::from_value(value).ok_or_else(|| {
                    format!(
                        "builtin '{}' vector item {} must be a scalar",
                        builtin.as_str(),
                        idx
                    )
                })?;
                values.push(scalar);
            }
            Ok(PlannerBuiltinArgs::SortScalars(SortScalarsArgs { values }))
        }
        PlannerBuiltinId::Get => {
            let map = exactly_one_map_arg(builtin, args)?;
            let value = map.get("value").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "value"
                )
            })?;
            let key = map.get("key").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "key"
                )
            })?;
            Ok(PlannerBuiltinArgs::Get(GetArgs {
                value: value.clone(),
                key: key.clone(),
            }))
        }
        PlannerBuiltinId::Len => {
            let map = exactly_one_map_arg(builtin, args)?;
            let value = map.get("value").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "value"
                )
            })?;
            Ok(PlannerBuiltinArgs::Len(LenArgs {
                value: value.clone(),
            }))
        }
        PlannerBuiltinId::StrCat => {
            let map = exactly_one_map_arg(builtin, args)?;
            let values = map.get("values").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "values"
                )
            })?;
            let PlannerValue::Vector(values) = values else {
                return Err(format!(
                    "builtin '{}' key 'values' must be a vector",
                    builtin.as_str()
                ));
            };
            Ok(PlannerBuiltinArgs::StrCat(StrCatArgs {
                values: values.clone(),
            }))
        }
        PlannerBuiltinId::RustCrateGraph => {
            if args.is_empty() {
                Ok(PlannerBuiltinArgs::RustCrateGraph)
            } else {
                Err(format!(
                    "builtin '{}' expected no arguments but got {}",
                    builtin.as_str(),
                    args.len()
                ))
            }
        }
        PlannerBuiltinId::RustRoot => {
            let map = exactly_one_map_arg(builtin, args)?;
            let graph = map.get("graph").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "graph"
                )
            })?;
            Ok(PlannerBuiltinArgs::RustRoot(RustRootArgs {
                graph: graph.clone(),
            }))
        }
        PlannerBuiltinId::RustSelect => {
            let map = exactly_one_map_arg(builtin, args)?;
            let graph = map.get("graph").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "graph"
                )
            })?;
            let name = required_string(map, "name", builtin)?.to_owned();
            let crate_type = required_string(map, "crate_type", builtin)?.to_owned();
            Ok(PlannerBuiltinArgs::RustSelect(RustSelectArgs {
                graph: graph.clone(),
                name,
                crate_type,
            }))
        }
        PlannerBuiltinId::RustTransitiveClosure => {
            let map = exactly_one_map_arg(builtin, args)?;
            let graph = map.get("graph").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "graph"
                )
            })?;
            let roots = map.get("roots").ok_or_else(|| {
                format!(
                    "builtin '{}' missing required key '{}'",
                    builtin.as_str(),
                    "roots"
                )
            })?;
            let PlannerValue::Vector(roots) = roots else {
                return Err(format!(
                    "builtin '{}' key 'roots' must be a vector",
                    builtin.as_str()
                ));
            };
            Ok(PlannerBuiltinArgs::RustTransitiveClosure(
                RustTransitiveClosureArgs {
                    graph: graph.clone(),
                    roots: roots.clone(),
                },
            ))
        }
        PlannerBuiltinId::EnvProfile | PlannerBuiltinId::EnvTargetTriple => {
            if args.is_empty() {
                Ok(PlannerBuiltinArgs::None)
            } else {
                Err(format!(
                    "builtin '{}' expected no arguments but got {}",
                    builtin.as_str(),
                    args.len()
                ))
            }
        }
        PlannerBuiltinId::ClockNow
        | PlannerBuiltinId::RandomInt
        | PlannerBuiltinId::NetRequest
        | PlannerBuiltinId::FsRead
        | PlannerBuiltinId::ProcessSpawn => {
            if args.is_empty() {
                Ok(PlannerBuiltinArgs::None)
            } else {
                Err(format!(
                    "builtin '{}' expected no arguments but got {}",
                    builtin.as_str(),
                    args.len()
                ))
            }
        }
    }
}

fn optional_meta(
    map: &BTreeMap<String, PlannerValue>,
    builtin: PlannerBuiltinId,
) -> Result<BTreeMap<String, String>, String> {
    let Some(meta_value) = map.get("meta") else {
        return Ok(BTreeMap::new());
    };

    let PlannerValue::Map(meta_map) = meta_value else {
        return Err(format!(
            "builtin '{}' key 'meta' must be a map",
            builtin.as_str()
        ));
    };

    let mut out = BTreeMap::new();
    for (k, v) in meta_map {
        let s = match v {
            PlannerValue::Null => "null".to_string(),
            PlannerValue::Bool(b) => b.to_string(),
            PlannerValue::Int(i) => i.to_string(),
            PlannerValue::String(s) => s.clone(),
            PlannerValue::Keyword(s) => s.clone(),
            PlannerValue::Vector(_) | PlannerValue::Set(_) | PlannerValue::Map(_) => {
                return Err(format!(
                    "builtin '{}' key 'meta' value for '{k}' must be a scalar",
                    builtin.as_str()
                ));
            }
        };
        out.insert(k.clone(), s);
    }
    Ok(out)
}

fn rust_root(graph: &PlannerValue) -> Result<PlannerValue, String> {
    let (id_to_crate, topo, root_id) = rust_parse_graph(graph)?;
    let _ = topo;
    id_to_crate
        .get(&root_id)
        .cloned()
        .ok_or_else(|| format!("rust graph root id '{root_id}' was not found in crates"))
}

fn rust_select(graph: &PlannerValue, name: &str, crate_type: &str) -> Result<PlannerValue, String> {
    let (id_to_crate, topo, _root_id) = rust_parse_graph(graph)?;
    let mut out = Vec::new();
    for id in topo {
        let Some(PlannerValue::Map(map)) = id_to_crate.get(&id) else {
            continue;
        };

        let matches_name = map
            .get("name")
            .and_then(PlannerScalar::from_value)
            .and_then(|s| match s {
                PlannerScalar::String(s) | PlannerScalar::Keyword(s) => Some(s),
                _ => None,
            })
            .is_some_and(|s| s == name);

        let matches_type = map
            .get("type")
            .and_then(PlannerScalar::from_value)
            .and_then(|s| match s {
                PlannerScalar::String(s) | PlannerScalar::Keyword(s) => Some(s),
                _ => None,
            })
            .is_some_and(|s| s == crate_type);

        if matches_name && matches_type {
            out.push(PlannerValue::Map(map.clone()));
        }
    }
    Ok(PlannerValue::Vector(out))
}

fn rust_transitive_closure(
    graph: &PlannerValue,
    roots: &[PlannerValue],
) -> Result<PlannerValue, String> {
    let (id_to_crate, topo, _root_id) = rust_parse_graph(graph)?;

    let mut roots_ids = Vec::new();
    for (idx, r) in roots.iter().enumerate() {
        let id = rust_root_id_from_value(r).ok_or_else(|| {
            format!(
                "rust.transitive_closure roots[{idx}] must be a crate map (with 'id') or an id string"
            )
        })?;
        roots_ids.push(id);
    }

    let mut deps_by_id = BTreeMap::<String, Vec<String>>::new();
    for (id, crate_value) in &id_to_crate {
        let PlannerValue::Map(map) = crate_value else {
            continue;
        };
        let deps = rust_deps_from_crate_map(map)?;
        deps_by_id.insert(id.clone(), deps);
    }

    let mut reachable = BTreeSet::<String>::new();
    let mut stack = roots_ids;
    while let Some(id) = stack.pop() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        if let Some(deps) = deps_by_id.get(&id) {
            for dep in deps {
                stack.push(dep.clone());
            }
        }
    }

    let mut out = Vec::new();
    for id in topo {
        if !reachable.contains(&id) {
            continue;
        }
        if let Some(v) = id_to_crate.get(&id) {
            out.push(v.clone());
        }
    }

    Ok(PlannerValue::Vector(out))
}

type RustGraphParsed = (BTreeMap<String, PlannerValue>, Vec<String>, String);

fn rust_parse_graph(graph: &PlannerValue) -> Result<RustGraphParsed, String> {
    let PlannerValue::Map(map) = graph else {
        return Err("rust graph must be a map".to_string());
    };

    let topo = match map.get("topo") {
        Some(PlannerValue::Vector(v)) => v
            .iter()
            .enumerate()
            .map(|(idx, v)| {
                rust_root_id_from_value(v)
                    .ok_or_else(|| format!("rust graph topo[{idx}] must be a string/keyword id"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("rust graph must contain key 'topo' as vector".to_string()),
    };

    let root_id = match map.get("root") {
        Some(v) => rust_root_id_from_value(v)
            .ok_or_else(|| "rust graph key 'root' must be a string/keyword id".to_string())?,
        None => return Err("rust graph must contain key 'root'".to_string()),
    };

    let crates = match map.get("crates") {
        Some(PlannerValue::Vector(v)) => v,
        _ => return Err("rust graph must contain key 'crates' as vector".to_string()),
    };

    let mut id_to_crate = BTreeMap::new();
    for (idx, c) in crates.iter().enumerate() {
        let PlannerValue::Map(crate_map) = c else {
            return Err(format!("rust graph crates[{idx}] must be a map"));
        };
        let Some(id) = crate_map
            .get("id")
            .and_then(rust_root_id_from_value)
            .or_else(|| crate_map.get("crate_id").and_then(rust_root_id_from_value))
        else {
            return Err(format!("rust graph crates[{idx}] must have string 'id'"));
        };
        id_to_crate.insert(id, PlannerValue::Map(crate_map.clone()));
    }

    Ok((id_to_crate, topo, root_id))
}

fn rust_root_id_from_value(value: &PlannerValue) -> Option<String> {
    match value {
        PlannerValue::String(s) => Some(s.clone()),
        PlannerValue::Keyword(s) => Some(s.clone()),
        PlannerValue::Map(map) => map
            .get("id")
            .and_then(rust_root_id_from_value)
            .or_else(|| map.get("crate_id").and_then(rust_root_id_from_value)),
        _ => None,
    }
}

fn rust_deps_from_crate_map(map: &BTreeMap<String, PlannerValue>) -> Result<Vec<String>, String> {
    let deps = match map.get("deps") {
        None => return Ok(Vec::new()),
        Some(v) => v,
    };
    let PlannerValue::Vector(deps) = deps else {
        return Err("crate map key 'deps' must be a vector".to_string());
    };
    let mut out = Vec::with_capacity(deps.len());
    for (idx, dep) in deps.iter().enumerate() {
        let Some(id) = rust_root_id_from_value(dep) else {
            return Err(format!("crate deps[{idx}] must be a string/keyword id"));
        };
        out.push(id);
    }
    Ok(out)
}

fn exactly_one_map_arg(
    builtin: PlannerBuiltinId,
    args: &[PlannerValue],
) -> Result<&BTreeMap<String, PlannerValue>, String> {
    if args.len() != 1 {
        return Err(format!(
            "builtin '{}' expected exactly one map argument but got {}",
            builtin.as_str(),
            args.len()
        ));
    }

    match &args[0] {
        PlannerValue::Map(map) => Ok(map),
        _ => Err(format!(
            "builtin '{}' expected a map argument",
            builtin.as_str()
        )),
    }
}

fn exactly_one_vector_arg(
    builtin: PlannerBuiltinId,
    args: &[PlannerValue],
) -> Result<&Vec<PlannerValue>, String> {
    if args.len() != 1 {
        return Err(format!(
            "builtin '{}' expected exactly one vector argument but got {}",
            builtin.as_str(),
            args.len()
        ));
    }

    match &args[0] {
        PlannerValue::Vector(values) => Ok(values),
        _ => Err(format!(
            "builtin '{}' expected a vector argument",
            builtin.as_str()
        )),
    }
}

fn required_string<'a>(
    map: &'a BTreeMap<String, PlannerValue>,
    key: &str,
    builtin: PlannerBuiltinId,
) -> Result<&'a str, String> {
    let value = map.get(key).ok_or_else(|| {
        format!(
            "builtin '{}' missing required key '{}'",
            builtin.as_str(),
            key
        )
    })?;

    match value {
        PlannerValue::String(s) => Ok(s.as_str()),
        PlannerValue::Keyword(s) => Ok(s.as_str()),
        _ => Err(format!(
            "builtin '{}' key '{}' must be a string/keyword",
            builtin.as_str(),
            key
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
            builtin: PlannerBuiltinId::EmitNode,
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
            builtin: PlannerBuiltinId::EmitEdge,
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
            builtin: PlannerBuiltinId::EmitFact,
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
        let mut runtime =
            PlannerRuntime::new(host, PlannerPolicy::deterministic(), planner_builtins())
                .expect("runtime should build");

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
            let mut runtime =
                PlannerRuntime::new(host, PlannerPolicy::deterministic(), planner_builtins())
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
    fn capability_violation_matrix_has_source_mapped_diagnostics() {
        let denied = [
            (
                PlannerBuiltinId::ClockNow,
                "rule.time.read",
                PlannerCapability::Time,
            ),
            (
                PlannerBuiltinId::RandomInt,
                "rule.random.read",
                PlannerCapability::Random,
            ),
            (
                PlannerBuiltinId::NetRequest,
                "rule.net.read",
                PlannerCapability::Network,
            ),
            (
                PlannerBuiltinId::FsRead,
                "rule.fs.read",
                PlannerCapability::Filesystem,
            ),
            (
                PlannerBuiltinId::ProcessSpawn,
                "rule.process.spawn",
                PlannerCapability::Process,
            ),
        ];

        for (idx, (builtin, rule_id, capability)) in denied.iter().enumerate() {
            let line = 40 + idx as u32;
            let program = PlannerProgram {
                forms: vec![PlannerExpr::Call(PlannerCall {
                    builtin: *builtin,
                    args: Vec::new(),
                    provenance: provenance(rule_id, line),
                })],
            };

            let host = RulePlanHost::with_sources(vec![default_source()]);
            let mut runtime =
                PlannerRuntime::new(host, PlannerPolicy::deterministic(), planner_builtins())
                    .expect("runtime should build");

            let err = runtime
                .evaluate_program(&program)
                .expect_err("capability should be denied");

            assert!(
                err.diagnostic
                    .message
                    .contains(&format!("denied capability '{}'", capability.as_str()))
            );
            assert_eq!(err.diagnostic.rule_id.as_str(), *rule_id);
            assert_eq!(err.diagnostic.source_id.as_str(), "runtime");
            assert_eq!(err.diagnostic.line, line);
            assert_eq!(err.diagnostic.column, 1);
        }
    }

    #[test]
    fn deterministic_transform_builtins_have_stable_ordering() {
        let program = PlannerProgram {
            forms: vec![
                PlannerExpr::Call(PlannerCall {
                    builtin: PlannerBuiltinId::SortMapKeys,
                    args: vec![PlannerExpr::Value(PlannerValue::Map(BTreeMap::from([
                        ("z".to_string(), PlannerValue::Int(1)),
                        ("a".to_string(), PlannerValue::Int(2)),
                        ("m".to_string(), PlannerValue::Int(3)),
                    ])))],
                    provenance: provenance("rule.sort.map-keys", 1),
                }),
                PlannerExpr::Call(PlannerCall {
                    builtin: PlannerBuiltinId::SortScalars,
                    args: vec![PlannerExpr::Value(PlannerValue::Vector(vec![
                        PlannerValue::Keyword("k2".to_string()),
                        PlannerValue::Int(7),
                        PlannerValue::Keyword("k1".to_string()),
                        PlannerValue::Int(-1),
                    ]))],
                    provenance: provenance("rule.sort.scalars", 2),
                }),
            ],
        };

        let run_once = || {
            let host = RulePlanHost::with_sources(vec![default_source()]);
            let mut runtime =
                PlannerRuntime::new(host, PlannerPolicy::deterministic(), planner_builtins())
                    .expect("runtime should build");
            runtime
                .evaluate_program(&program)
                .expect("program should evaluate")
        };

        let out1 = run_once();
        let out2 = run_once();
        assert_eq!(out1, out2);

        assert_eq!(
            out1[0],
            PlannerValue::Vector(vec![
                PlannerValue::String("a".to_string()),
                PlannerValue::String("m".to_string()),
                PlannerValue::String("z".to_string())
            ])
        );

        assert_eq!(
            out1[1],
            PlannerValue::Vector(vec![
                PlannerValue::Int(-1),
                PlannerValue::Int(7),
                PlannerValue::Keyword("k1".to_string()),
                PlannerValue::Keyword("k2".to_string())
            ])
        );
    }
}
