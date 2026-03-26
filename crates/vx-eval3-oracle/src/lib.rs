//! Tiny executable oracle for the `eval3` slice-0 semantics.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnvId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClosureId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldKey {
    Index(usize),
    Name(String),
}

impl FieldKey {
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obs {
    NoObs,
    NeedValue,
    NeedCallable,
    NeedProduct(BTreeMap<FieldKey, Obs>),
}

impl Obs {
    pub fn need_field(field: FieldKey, obs: Self) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert(field, obs);
        Self::NeedProduct(fields)
    }

    pub fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::NoObs, rhs) => rhs.clone(),
            (lhs, Self::NoObs) => lhs.clone(),
            (Self::NeedValue, Self::NeedValue) => Self::NeedValue,
            (Self::NeedCallable, Self::NeedCallable) => Self::NeedCallable,
            (Self::NeedProduct(lhs), Self::NeedProduct(rhs)) => {
                let mut joined = lhs.clone();
                for (field, rhs_obs) in rhs {
                    joined
                        .entry(field.clone())
                        .and_modify(|lhs_obs| *lhs_obs = lhs_obs.join(rhs_obs))
                        .or_insert_with(|| rhs_obs.clone());
                }
                Self::normalize_product(joined)
            }
            _ => panic!("observation join mismatch: {self:?} vs {other:?}"),
        }
    }

    pub fn leq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::NoObs, _) => true,
            (_, Self::NoObs) => false,
            (Self::NeedValue, Self::NeedValue) => true,
            (Self::NeedCallable, Self::NeedCallable) => true,
            (Self::NeedProduct(lhs), Self::NeedProduct(rhs)) => lhs
                .iter()
                .all(|(field, lhs_obs)| rhs.get(field).is_some_and(|rhs_obs| lhs_obs.leq(rhs_obs))),
            _ => false,
        }
    }

    fn normalize_product(fields: BTreeMap<FieldKey, Obs>) -> Self {
        if fields.is_empty() {
            Self::NoObs
        } else {
            Self::NeedProduct(fields)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    U64(u64),
    Bool(bool),
    Local(String),
    Let {
        name: String,
        value: Box<Expr>,
        body: Box<Expr>,
    },
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    Tuple(Vec<Expr>),
    Struct(Vec<(String, Expr)>),
    Field {
        base: Box<Expr>,
        field: FieldKey,
    },
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    CallNamed {
        name: String,
        args: Vec<Expr>,
    },
    Add(Box<Expr>, Box<Expr>),
    Trap(String),
}

impl Expr {
    fn label(&self) -> String {
        match self {
            Self::U64(value) => format!("u64({value})"),
            Self::Bool(value) => format!("bool({value})"),
            Self::Local(name) => format!("local {name}"),
            Self::Let { name, .. } => format!("let {name}"),
            Self::If { .. } => "if".to_owned(),
            Self::Tuple(_) => "tuple".to_owned(),
            Self::Struct(_) => "struct".to_owned(),
            Self::Field { field, .. } => match field {
                FieldKey::Index(index) => format!("field .{index}"),
                FieldKey::Name(name) => format!("field .{name}"),
            },
            Self::Lambda { .. } => "lambda".to_owned(),
            Self::Call { .. } => "call".to_owned(),
            Self::CallNamed { name, .. } => format!("call {name}"),
            Self::Add(_, _) => "add".to_owned(),
            Self::Trap(message) => format!("trap {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDef {
    pub name: String,
    pub params: Vec<String>,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub functions: BTreeMap<String, FunctionDef>,
}

impl Program {
    pub fn new(functions: impl IntoIterator<Item = FunctionDef>) -> Self {
        Self {
            functions: functions
                .into_iter()
                .map(|function| (function.name.clone(), function))
                .collect(),
        }
    }

    pub fn run_entry(
        &self,
        entry: &str,
        args: Vec<Ref>,
        obs: Obs,
    ) -> Result<OracleRun, OracleError> {
        let mut machine = OracleMachine::new(self.clone());
        let result =
            machine.call_callable(CallableValue::Function(entry.to_owned()), args, &obs)?;
        Ok(machine.finish(result))
    }

    pub fn canonical_program_a() -> Self {
        Self::new([
            FunctionDef {
                name: "bomb".to_owned(),
                params: vec![],
                body: Expr::Trap("bomb".to_owned()),
            },
            FunctionDef {
                name: "main".to_owned(),
                params: vec![],
                body: Expr::Let {
                    name: "pair".to_owned(),
                    value: Box::new(Expr::Tuple(vec![
                        Expr::Add(Box::new(Expr::U64(41)), Box::new(Expr::U64(1))),
                        Expr::CallNamed {
                            name: "bomb".to_owned(),
                            args: vec![],
                        },
                    ])),
                    body: Box::new(Expr::Field {
                        base: Box::new(Expr::Local("pair".to_owned())),
                        field: FieldKey::index(0),
                    }),
                },
            },
        ])
    }

    pub fn canonical_program_b() -> Self {
        Self::new([FunctionDef {
            name: "main".to_owned(),
            params: vec!["n".to_owned()],
            body: Expr::Let {
                name: "add_n".to_owned(),
                value: Box::new(Expr::Lambda {
                    params: vec!["x".to_owned()],
                    body: Box::new(Expr::Add(
                        Box::new(Expr::Local("x".to_owned())),
                        Box::new(Expr::Local("n".to_owned())),
                    )),
                }),
                body: Box::new(Expr::Let {
                    name: "pair".to_owned(),
                    value: Box::new(Expr::Tuple(vec![
                        Expr::Call {
                            callee: Box::new(Expr::Local("add_n".to_owned())),
                            args: vec![Expr::U64(1)],
                        },
                        Expr::Call {
                            callee: Box::new(Expr::Local("add_n".to_owned())),
                            args: vec![Expr::U64(2)],
                        },
                    ])),
                    body: Box::new(Expr::Field {
                        base: Box::new(Expr::Local("pair".to_owned())),
                        field: FieldKey::index(1),
                    }),
                }),
            },
        }])
    }

    pub fn canonical_program_c() -> Self {
        Self::new([
            FunctionDef {
                name: "plus_one".to_owned(),
                params: vec!["n".to_owned()],
                body: Expr::Add(
                    Box::new(Expr::Local("n".to_owned())),
                    Box::new(Expr::U64(1)),
                ),
            },
            FunctionDef {
                name: "main".to_owned(),
                params: vec!["n".to_owned()],
                body: Expr::Let {
                    name: "x".to_owned(),
                    value: Box::new(Expr::CallNamed {
                        name: "plus_one".to_owned(),
                        args: vec![Expr::Local("n".to_owned())],
                    }),
                    body: Box::new(Expr::Add(
                        Box::new(Expr::Local("x".to_owned())),
                        Box::new(Expr::Local("x".to_owned())),
                    )),
                },
            },
        ])
    }

    pub fn canonical_program_d() -> Self {
        Self::new([
            FunctionDef {
                name: "apply".to_owned(),
                params: vec!["f".to_owned(), "x".to_owned()],
                body: Expr::Call {
                    callee: Box::new(Expr::Local("f".to_owned())),
                    args: vec![Expr::Local("x".to_owned())],
                },
            },
            FunctionDef {
                name: "main".to_owned(),
                params: vec!["n".to_owned()],
                body: Expr::Let {
                    name: "add_n".to_owned(),
                    value: Box::new(Expr::Lambda {
                        params: vec!["x".to_owned()],
                        body: Box::new(Expr::Add(
                            Box::new(Expr::Local("x".to_owned())),
                            Box::new(Expr::Local("n".to_owned())),
                        )),
                    }),
                    body: Box::new(Expr::CallNamed {
                        name: "apply".to_owned(),
                        args: vec![Expr::Local("add_n".to_owned()), Expr::U64(41)],
                    }),
                },
            },
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    Ready(ReadyValue),
    Node(NodeId),
}

impl Ref {
    pub fn ready_u64(value: u64) -> Self {
        Self::Ready(ReadyValue::U64(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyValue {
    U64(u64),
    Bool(bool),
    Product(ProductId),
    Callable(CallableValue),
}

impl ReadyValue {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallableValue {
    Function(String),
    Closure(ClosureId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceEvent {
    NodeAllocated {
        node: NodeId,
        label: String,
    },
    NodeEntered {
        node: NodeId,
        label: String,
        obs: Obs,
    },
    NodeReused {
        node: NodeId,
        label: String,
        obs: Obs,
    },
    ObservationWidened {
        node: NodeId,
        label: String,
        from: Obs,
        to: Obs,
    },
    ProductAllocated {
        product: ProductId,
        field_count: usize,
    },
    ProductFieldProjected {
        product: ProductId,
        field: FieldKey,
    },
    EnvAllocated {
        env: EnvId,
        captures: Vec<String>,
    },
    ClosureAllocated {
        closure: ClosureId,
        env: Option<EnvId>,
    },
    TrapEntered {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSnapshot {
    pub id: NodeId,
    pub label: String,
    pub strongest_obs: Option<Obs>,
    pub enter_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleRun {
    pub result: ReadyValue,
    pub trace: Vec<TraceEvent>,
    pub nodes: Vec<NodeSnapshot>,
}

impl OracleRun {
    pub fn result_u64(&self) -> Option<u64> {
        self.result.as_u64()
    }

    pub fn node_named(&self, label: &str) -> Option<&NodeSnapshot> {
        self.nodes.iter().find(|node| node.label == label)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OracleError {
    #[error("missing local {name}")]
    MissingLocal { name: String },

    #[error("missing function {name}")]
    MissingFunction { name: String },

    #[error("arity mismatch for {name}: expected {expected}, got {actual}")]
    ArityMismatch {
        name: String,
        expected: usize,
        actual: usize,
    },

    #[error("invalid observation {obs:?} for value kind {kind}")]
    InvalidObservation { obs: Obs, kind: &'static str },

    #[error("reentrant observation of node {node:?}")]
    ReentrantNode { node: NodeId },

    #[error("trap entered: {message}")]
    Trap { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProductObject {
    fields: BTreeMap<FieldKey, Ref>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvObject {
    captures: BTreeMap<String, Ref>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClosureObject {
    params: Vec<String>,
    body: Expr,
    env: Option<EnvId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeState {
    Unevaluated,
    Evaluating,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThunkNode {
    label: String,
    expr: Expr,
    env: LocalEnv,
    state: NodeState,
    strongest_obs: Option<Obs>,
    cached: Option<ReadyValue>,
    enter_count: usize,
}

type LocalEnv = BTreeMap<String, Ref>;

#[derive(Debug, Clone)]
struct OracleMachine {
    program: Program,
    nodes: Vec<ThunkNode>,
    products: Vec<ProductObject>,
    envs: Vec<EnvObject>,
    closures: Vec<ClosureObject>,
    trace: Vec<TraceEvent>,
}

impl OracleMachine {
    fn new(program: Program) -> Self {
        Self {
            program,
            nodes: Vec::new(),
            products: Vec::new(),
            envs: Vec::new(),
            closures: Vec::new(),
            trace: Vec::new(),
        }
    }

    fn finish(self, result: ReadyValue) -> OracleRun {
        OracleRun {
            result,
            trace: self.trace,
            nodes: self
                .nodes
                .into_iter()
                .enumerate()
                .map(|(index, node)| NodeSnapshot {
                    id: NodeId(index),
                    label: node.label,
                    strongest_obs: node.strongest_obs,
                    enter_count: node.enter_count,
                })
                .collect(),
        }
    }

    fn alloc_node(&mut self, expr: Expr, env: LocalEnv) -> NodeId {
        let id = NodeId(self.nodes.len());
        let label = expr.label();
        self.nodes.push(ThunkNode {
            label: label.clone(),
            expr,
            env,
            state: NodeState::Unevaluated,
            strongest_obs: None,
            cached: None,
            enter_count: 0,
        });
        self.trace
            .push(TraceEvent::NodeAllocated { node: id, label });
        id
    }

    fn alloc_product(&mut self, fields: BTreeMap<FieldKey, Ref>) -> ProductId {
        let id = ProductId(self.products.len());
        self.products.push(ProductObject { fields });
        self.trace.push(TraceEvent::ProductAllocated {
            product: id,
            field_count: self.products[id.0].fields.len(),
        });
        id
    }

    fn alloc_env(&mut self, captures: BTreeMap<String, Ref>) -> Option<EnvId> {
        if captures.is_empty() {
            return None;
        }

        let id = EnvId(self.envs.len());
        let capture_names = captures.keys().cloned().collect::<Vec<_>>();
        self.envs.push(EnvObject { captures });
        self.trace.push(TraceEvent::EnvAllocated {
            env: id,
            captures: capture_names,
        });
        Some(id)
    }

    fn alloc_closure(&mut self, params: Vec<String>, body: Expr, env: Option<EnvId>) -> ClosureId {
        let id = ClosureId(self.closures.len());
        self.closures.push(ClosureObject { params, body, env });
        self.trace
            .push(TraceEvent::ClosureAllocated { closure: id, env });
        id
    }

    fn materialize_expr(&mut self, expr: &Expr, env: &LocalEnv) -> Result<Ref, OracleError> {
        match expr {
            Expr::U64(value) => Ok(Ref::ready_u64(*value)),
            Expr::Bool(value) => Ok(Ref::Ready(ReadyValue::Bool(*value))),
            Expr::Local(name) => env
                .get(name)
                .cloned()
                .ok_or_else(|| OracleError::MissingLocal { name: name.clone() }),
            Expr::Let { name, value, body } => {
                let value_ref = self.materialize_expr(value, env)?;
                let mut extended = env.clone();
                extended.insert(name.clone(), value_ref);
                self.materialize_expr(body, &extended)
            }
            other => Ok(Ref::Node(self.alloc_node(other.clone(), env.clone()))),
        }
    }

    fn observe_ref(&mut self, reference: Ref, obs: &Obs) -> Result<ReadyValue, OracleError> {
        match reference {
            Ref::Ready(value) => self.satisfy_ready(value, obs),
            Ref::Node(node) => self.observe_node(node, obs),
        }
    }

    fn satisfy_ready(&mut self, value: ReadyValue, obs: &Obs) -> Result<ReadyValue, OracleError> {
        match (&value, obs) {
            (_, Obs::NoObs) => Ok(value),
            (ReadyValue::U64(_), Obs::NeedValue) => Ok(value),
            (ReadyValue::Bool(_), Obs::NeedValue) => Ok(value),
            (ReadyValue::Callable(_), Obs::NeedCallable) => Ok(value),
            (ReadyValue::Product(_), Obs::NeedProduct(_)) => Ok(value),
            (ReadyValue::U64(_), _) => Err(OracleError::InvalidObservation {
                obs: obs.clone(),
                kind: "u64",
            }),
            (ReadyValue::Bool(_), _) => Err(OracleError::InvalidObservation {
                obs: obs.clone(),
                kind: "bool",
            }),
            (ReadyValue::Callable(_), _) => Err(OracleError::InvalidObservation {
                obs: obs.clone(),
                kind: "callable",
            }),
            (ReadyValue::Product(_), _) => Err(OracleError::InvalidObservation {
                obs: obs.clone(),
                kind: "product",
            }),
        }
    }

    fn observe_node(&mut self, node: NodeId, obs: &Obs) -> Result<ReadyValue, OracleError> {
        let (label, expr, env, cached, state, strongest_obs) = {
            let thunk = &self.nodes[node.0];
            (
                thunk.label.clone(),
                thunk.expr.clone(),
                thunk.env.clone(),
                thunk.cached.clone(),
                thunk.state.clone(),
                thunk.strongest_obs.clone(),
            )
        };

        if let Some(existing) = strongest_obs {
            if !obs.leq(&existing) {
                let widened = existing.join(obs);
                if widened != existing {
                    self.trace.push(TraceEvent::ObservationWidened {
                        node,
                        label: label.clone(),
                        from: existing.clone(),
                        to: widened.clone(),
                    });
                    self.nodes[node.0].strongest_obs = Some(widened);
                }
            }
        } else {
            self.nodes[node.0].strongest_obs = Some(obs.clone());
        }

        match state {
            NodeState::Ready => {
                self.trace.push(TraceEvent::NodeReused {
                    node,
                    label,
                    obs: obs.clone(),
                });
                return Ok(cached.expect("ready node missing cached value"));
            }
            NodeState::Evaluating => {
                return Err(OracleError::ReentrantNode { node });
            }
            NodeState::Unevaluated => {}
        }

        {
            let thunk = &mut self.nodes[node.0];
            thunk.state = NodeState::Evaluating;
            thunk.enter_count += 1;
        }

        self.trace.push(TraceEvent::NodeEntered {
            node,
            label: label.clone(),
            obs: obs.clone(),
        });

        let result = self.eval_expr_obs(&expr, &env, obs)?;

        {
            let thunk = &mut self.nodes[node.0];
            thunk.state = NodeState::Ready;
            thunk.cached = Some(result.clone());
        }

        Ok(result)
    }

    fn eval_expr_obs(
        &mut self,
        expr: &Expr,
        env: &LocalEnv,
        obs: &Obs,
    ) -> Result<ReadyValue, OracleError> {
        match expr {
            Expr::U64(value) => self.satisfy_ready(ReadyValue::U64(*value), obs),
            Expr::Bool(value) => self.satisfy_ready(ReadyValue::Bool(*value), obs),
            Expr::Local(name) => {
                let reference = env
                    .get(name)
                    .cloned()
                    .ok_or_else(|| OracleError::MissingLocal { name: name.clone() })?;
                self.observe_ref(reference, obs)
            }
            Expr::Let { name, value, body } => {
                let value_ref = self.materialize_expr(value, env)?;
                let mut extended = env.clone();
                extended.insert(name.clone(), value_ref);
                self.eval_expr_obs(body, &extended, obs)
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition_ref = self.materialize_expr(condition, env)?;
                let condition_value = self.observe_ref(condition_ref, &Obs::NeedValue)?;
                match condition_value {
                    ReadyValue::Bool(true) => self.eval_expr_obs(then_branch, env, obs),
                    ReadyValue::Bool(false) => self.eval_expr_obs(else_branch, env, obs),
                    _ => Err(OracleError::InvalidObservation {
                        obs: Obs::NeedValue,
                        kind: "non-bool-if-condition",
                    }),
                }
            }
            Expr::Tuple(fields) => self.eval_product_obs(
                fields
                    .iter()
                    .enumerate()
                    .map(|(index, expr)| (FieldKey::Index(index), expr))
                    .collect(),
                env,
                obs,
            ),
            Expr::Struct(fields) => self.eval_product_obs(
                fields
                    .iter()
                    .map(|(name, expr)| (FieldKey::Name(name.clone()), expr))
                    .collect(),
                env,
                obs,
            ),
            Expr::Field { base, field } => {
                let base_ref = self.materialize_expr(base, env)?;
                let base_value =
                    self.observe_ref(base_ref, &Obs::need_field(field.clone(), obs.clone()))?;
                let product = match base_value {
                    ReadyValue::Product(product) => product,
                    _ => {
                        return Err(OracleError::InvalidObservation {
                            obs: Obs::need_field(field.clone(), obs.clone()),
                            kind: "non-product-field-base",
                        });
                    }
                };
                self.trace.push(TraceEvent::ProductFieldProjected {
                    product,
                    field: field.clone(),
                });
                let field_ref = self.products[product.0]
                    .fields
                    .get(field)
                    .cloned()
                    .ok_or_else(|| OracleError::InvalidObservation {
                        obs: obs.clone(),
                        kind: "missing-product-field",
                    })?;
                self.observe_ref(field_ref, obs)
            }
            Expr::Lambda { params, body } => {
                if !matches!(obs, Obs::NeedCallable | Obs::NoObs) {
                    return Err(OracleError::InvalidObservation {
                        obs: obs.clone(),
                        kind: "lambda",
                    });
                }

                let captures = free_locals(body, params)
                    .into_iter()
                    .map(|name| {
                        let reference = env
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| OracleError::MissingLocal { name: name.clone() })?;
                        Ok((name, reference))
                    })
                    .collect::<Result<BTreeMap<_, _>, OracleError>>()?;
                let env_id = self.alloc_env(captures);
                let closure = self.alloc_closure(params.clone(), (**body).clone(), env_id);
                Ok(ReadyValue::Callable(CallableValue::Closure(closure)))
            }
            Expr::Call { callee, args } => {
                let callee_ref = self.materialize_expr(callee, env)?;
                let callee_value = self.observe_ref(callee_ref, &Obs::NeedCallable)?;
                let callable = match callee_value {
                    ReadyValue::Callable(callable) => callable,
                    _ => {
                        return Err(OracleError::InvalidObservation {
                            obs: Obs::NeedCallable,
                            kind: "non-callable",
                        });
                    }
                };
                let arg_refs = args
                    .iter()
                    .map(|arg| self.materialize_expr(arg, env))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call_callable(callable, arg_refs, obs)
            }
            Expr::CallNamed { name, args } => {
                let arg_refs = args
                    .iter()
                    .map(|arg| self.materialize_expr(arg, env))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call_callable(CallableValue::Function(name.clone()), arg_refs, obs)
            }
            Expr::Add(lhs, rhs) => {
                if !matches!(obs, Obs::NeedValue | Obs::NoObs) {
                    return Err(OracleError::InvalidObservation {
                        obs: obs.clone(),
                        kind: "add",
                    });
                }
                let lhs_ref = self.materialize_expr(lhs, env)?;
                let rhs_ref = self.materialize_expr(rhs, env)?;
                let lhs_value = self.observe_ref(lhs_ref, &Obs::NeedValue)?;
                let rhs_value = self.observe_ref(rhs_ref, &Obs::NeedValue)?;
                match (lhs_value, rhs_value) {
                    (ReadyValue::U64(lhs), ReadyValue::U64(rhs)) => Ok(ReadyValue::U64(lhs + rhs)),
                    _ => Err(OracleError::InvalidObservation {
                        obs: Obs::NeedValue,
                        kind: "non-u64-add",
                    }),
                }
            }
            Expr::Trap(message) => {
                if !matches!(obs, Obs::NoObs) {
                    self.trace.push(TraceEvent::TrapEntered {
                        message: message.clone(),
                    });
                    return Err(OracleError::Trap {
                        message: message.clone(),
                    });
                }
                Err(OracleError::InvalidObservation {
                    obs: obs.clone(),
                    kind: "trap",
                })
            }
        }
    }

    fn eval_product_obs(
        &mut self,
        fields: Vec<(FieldKey, &Expr)>,
        env: &LocalEnv,
        obs: &Obs,
    ) -> Result<ReadyValue, OracleError> {
        if !matches!(obs, Obs::NeedProduct(_) | Obs::NoObs) {
            return Err(OracleError::InvalidObservation {
                obs: obs.clone(),
                kind: "product",
            });
        }

        let refs = fields
            .into_iter()
            .map(|(field, expr)| Ok((field, self.materialize_expr(expr, env)?)))
            .collect::<Result<BTreeMap<_, _>, OracleError>>()?;
        let product = self.alloc_product(refs);
        Ok(ReadyValue::Product(product))
    }

    fn call_callable(
        &mut self,
        callable: CallableValue,
        args: Vec<Ref>,
        obs: &Obs,
    ) -> Result<ReadyValue, OracleError> {
        match callable {
            CallableValue::Function(name) => {
                let function = self
                    .program
                    .functions
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| OracleError::MissingFunction { name: name.clone() })?;
                if function.params.len() != args.len() {
                    return Err(OracleError::ArityMismatch {
                        name,
                        expected: function.params.len(),
                        actual: args.len(),
                    });
                }
                let env = function
                    .params
                    .into_iter()
                    .zip(args)
                    .collect::<BTreeMap<_, _>>();
                self.eval_expr_obs(&function.body, &env, obs)
            }
            CallableValue::Closure(closure) => {
                let closure_obj = self.closures[closure.0].clone();
                if closure_obj.params.len() != args.len() {
                    return Err(OracleError::ArityMismatch {
                        name: "<closure>".to_owned(),
                        expected: closure_obj.params.len(),
                        actual: args.len(),
                    });
                }
                let mut env = closure_obj
                    .env
                    .map_or_else(BTreeMap::new, |env_id| self.envs[env_id.0].captures.clone());
                env.extend(closure_obj.params.into_iter().zip(args));
                self.eval_expr_obs(&closure_obj.body, &env, obs)
            }
        }
    }
}

fn free_locals(expr: &Expr, params: &[String]) -> BTreeSet<String> {
    let mut free = BTreeSet::new();
    let mut bound = params.iter().cloned().collect::<BTreeSet<_>>();
    collect_free_locals(expr, &mut bound, &mut free);
    free
}

fn collect_free_locals(expr: &Expr, bound: &mut BTreeSet<String>, free: &mut BTreeSet<String>) {
    match expr {
        Expr::U64(_) | Expr::Bool(_) | Expr::Trap(_) => {}
        Expr::Local(name) => {
            if !bound.contains(name) {
                free.insert(name.clone());
            }
        }
        Expr::Let { name, value, body } => {
            collect_free_locals(value, bound, free);
            let inserted = bound.insert(name.clone());
            collect_free_locals(body, bound, free);
            if inserted {
                bound.remove(name);
            }
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_free_locals(condition, bound, free);
            collect_free_locals(then_branch, bound, free);
            collect_free_locals(else_branch, bound, free);
        }
        Expr::Tuple(fields) => {
            for field in fields {
                collect_free_locals(field, bound, free);
            }
        }
        Expr::Struct(fields) => {
            for (_, field) in fields {
                collect_free_locals(field, bound, free);
            }
        }
        Expr::Field { base, .. } => collect_free_locals(base, bound, free),
        Expr::Lambda { params, body } => {
            let mut inner_bound = bound.clone();
            inner_bound.extend(params.iter().cloned());
            collect_free_locals(body, &mut inner_bound, free);
        }
        Expr::Call { callee, args } => {
            collect_free_locals(callee, bound, free);
            for arg in args {
                collect_free_locals(arg, bound, free);
            }
        }
        Expr::CallNamed { args, .. } => {
            for arg in args {
                collect_free_locals(arg, bound, free);
            }
        }
        Expr::Add(lhs, rhs) => {
            collect_free_locals(lhs, bound, free);
            collect_free_locals(rhs, bound, free);
        }
    }
}

#[cfg(test)]
mod tests;
