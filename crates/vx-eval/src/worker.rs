//! Demand-driven evaluation.
//!
//! Expressions build lazy value graphs. Forcing only happens at the
//! external boundary or inside a node whose operation truly needs a
//! resolved dependency.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::instr::{self, BinOp, UnaryOp};
use crate::node::{Node, NodeId, NodeJob, NodeKey, Outcome, Waiter};
use crate::poison::{DemandChain, DemandEntry, PoisonError, PoisonKind};
use crate::value::{FunctionValue, StructValue, Value};

thread_local! {
    static ACTIVE_NODES: RefCell<Vec<NodeId>> = const { RefCell::new(Vec::new()) };
}

enum BuildResult {
    Value(Value),
    Returned(Value),
    Poisoned(PoisonError),
}

/// The evaluation engine.
pub struct Engine {
    nodes: DashMap<NodeId, Arc<Node>>,
    call_memo: DashMap<NodeKey, Arc<Node>>,
    functions: HashMap<String, Vec<Arc<instr::FnDef>>>,
}

impl Engine {
    /// Create a new engine from a lowered module.
    pub fn from_module(module: &instr::Module) -> Self {
        let mut functions = HashMap::new();
        for func in &module.functions {
            functions
                .entry(func.name.clone())
                .or_insert_with(Vec::new)
                .push(func.clone());
        }

        Self {
            nodes: DashMap::new(),
            call_memo: DashMap::new(),
            functions,
        }
    }

    // r[impl eval.forcing.boundary]
    /// Evaluate a top-level function by name at the external forcing boundary.
    pub fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        let function = FunctionValue {
            def: self.resolve_function(name, &args)?,
            captures: Arc::from([]),
        };
        let value = self.build_call(Value::Function(function), args);
        self.force_value(value)
    }

    #[cfg(test)]
    pub(crate) fn call_unforced(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        let function = FunctionValue {
            def: self.resolve_function(name, &args)?,
            captures: Arc::from([]),
        };
        Ok(self.build_call(Value::Function(function), args))
    }

    #[cfg(test)]
    pub(crate) fn force_shallow(&self, value: Value) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => self.force_node(node_id),
            other => Ok(other),
        }
    }

    #[cfg(test)]
    pub(crate) fn force_fully(&self, value: Value) -> Result<Value, PoisonError> {
        self.force_value(value)
    }

    #[cfg(test)]
    pub(crate) fn poison_value(&self, message: &str) -> Value {
        let error = PoisonError {
            kind: PoisonKind::UserFail,
            message: message.into(),
            demand_chain: DemandChain::new(),
        };
        self.pending_value(NodeJob::Poison(error), format!("poison({message})"))
    }

    #[cfg(test)]
    pub(crate) fn memoized_call_count(&self) -> usize {
        self.call_memo.len()
    }

    // r[impl eval.pending]
    // r[impl eval.memoization.pending]
    fn build_call(&self, function: Value, args: Vec<Value>) -> Value {
        if let Value::Function(function_value) = &function
            && function_value.key().is_some()
            && args.iter().all(|value| value.key().is_some())
        {
            return Value::Pending(self.memoized_invoke_node(function_value.clone(), args));
        }

        self.pending_value(
            NodeJob::Call {
                function,
                args: Arc::from(args),
            },
            "call(..)".into(),
        )
    }

    fn memoized_invoke_node(&self, function: FunctionValue, args: Vec<Value>) -> NodeId {
        let key = NodeKey {
            identity: format!(
                "call:{}({})",
                function.key().unwrap_or_else(|| "<unresolved>".into()),
                args.iter()
                    .map(|value| value.key().unwrap_or_else(|| "<pending>".into()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        };

        match self.call_memo.entry(key) {
            Entry::Occupied(entry) => entry.get().id,
            Entry::Vacant(entry) => {
                let node = Arc::new(Node::new(
                    NodeJob::Invoke {
                        function,
                        args: Arc::from(args),
                    },
                    "invoke(..)".into(),
                ));
                let node_id = node.id;
                self.nodes.insert(node_id, node.clone());
                entry.insert(node);
                node_id
            }
        }
    }

    fn pending_value(&self, job: NodeJob, description: String) -> Value {
        let node = Arc::new(Node::new(job, description));
        let node_id = node.id;
        self.nodes.insert(node_id, node);
        Value::Pending(node_id)
    }

    // r[impl eval.forcing.boundary]
    fn force_value(&self, value: Value) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => {
                let value = self.force_node(node_id)?;
                self.force_value(value)
            }
            Value::Tuple(elems) => {
                let forced = self.force_many(elems.iter().cloned().collect())?;
                Ok(Value::Tuple(Arc::from(forced)))
            }
            Value::Struct(struct_value) => {
                let names = struct_value
                    .fields
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                let values = struct_value
                    .fields
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect::<Vec<_>>();
                let forced = self.force_many(values)?;
                let fields = names.into_iter().zip(forced).collect::<Vec<_>>();
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
            other => Ok(other),
        }
    }

    // r[impl eval.concurrency]
    fn force_many(&self, values: Vec<Value>) -> Result<Vec<Value>, PoisonError> {
        if values.len() <= 1 {
            return values
                .into_iter()
                .map(|value| self.force_value(value))
                .collect();
        }

        thread::scope(|scope| {
            let mut iter = values.into_iter();
            let first = iter.next().expect("checked length above");
            let handles = iter
                .map(|value| scope.spawn(move || self.force_value(value)))
                .collect::<Vec<_>>();

            let mut forced = vec![self.force_value(first)?];
            for handle in handles {
                let value = handle
                    .join()
                    .map_err(|_| self.internal_error("force worker panicked"))??;
                forced.push(value);
            }
            Ok(forced)
        })
    }

    fn force_node(&self, node_id: NodeId) -> Result<Value, PoisonError> {
        if ACTIVE_NODES.with(|stack| stack.borrow().contains(&node_id)) {
            return Err(PoisonError {
                kind: PoisonKind::CircularDependency,
                message: format!("circular dependency while demanding node {}", node_id.0),
                demand_chain: self.current_demand_chain(Some(node_id)),
            });
        }

        let node = self
            .nodes
            .get(&node_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| self.internal_error(&format!("missing node {}", node_id.0)))?;
        let (sender, receiver) = crossbeam_channel::bounded(1);

        match node.try_start_or_wait(Waiter { sender }) {
            Ok(()) => {
                let result = self.with_active_node(node_id, || self.execute_node(&node));
                match result {
                    Ok(value) => {
                        node.resolve(value.clone());
                        Ok(value)
                    }
                    Err(error) => {
                        node.poison(error.clone());
                        Err(error)
                    }
                }
            }
            Err(Some(Outcome::Value(value))) => Ok(value),
            Err(Some(Outcome::Poison(error))) => Err(error),
            Err(None) => match receiver.recv() {
                Ok((_node_id, Outcome::Value(value))) => Ok(value),
                Ok((_node_id, Outcome::Poison(error))) => Err(error),
                Err(_) => Err(self.internal_error("all waiters dropped")),
            },
        }
    }

    fn execute_node(&self, node: &Arc<Node>) -> Result<Value, PoisonError> {
        match &node.job {
            NodeJob::Binary { op, left, right } => {
                let mut values = self.force_many(vec![left.clone(), right.clone()])?;
                let right = values.pop().expect("pair has two values");
                let left = values.pop().expect("pair has two values");
                self.compute_binary(*op, &left, &right)
            }
            NodeJob::Unary { op, operand } => {
                let operand = self.force_value(operand.clone())?;
                self.compute_unary(*op, &operand)
            }
            NodeJob::If {
                cond,
                then_branch,
                else_branch,
                bindings,
            } => {
                let cond = self.force_value(cond.clone())?;
                match cond {
                    Value::Bool(true) => {
                        self.eval_block(then_branch, bindings.as_ref()).into_value()
                    }
                    Value::Bool(false) => {
                        self.eval_block(else_branch, bindings.as_ref()).into_value()
                    }
                    other => Err(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            NodeJob::Call { function, args } => self.execute_call(function.clone(), args.clone()),
            NodeJob::Invoke { function, args } => self.invoke_function(function, args.clone()),
            NodeJob::Field { receiver, field } => {
                let receiver = self.force_value(receiver.clone())?;
                self.extract_field(&receiver, field)
            }
            #[cfg(test)]
            NodeJob::Poison(error) => Err(error.clone()),
        }
    }

    // r[impl eval.enter-functions]
    fn execute_call(&self, function: Value, args: Arc<[Value]>) -> Result<Value, PoisonError> {
        let function = match self.force_value(function)? {
            Value::Function(function) => function,
            other => {
                return Err(self.type_error(&format!("cannot call {}", other.type_name())));
            }
        };

        if function.key().is_some() && args.iter().all(|value| value.key().is_some()) {
            return self
                .force_node(self.memoized_invoke_node(function, args.iter().cloned().collect()));
        }

        self.invoke_function(&function, args)
    }

    // r[impl eval.enter-functions]
    fn invoke_function(
        &self,
        function: &FunctionValue,
        args: Arc<[Value]>,
    ) -> Result<Value, PoisonError> {
        let mut bindings = HashMap::new();

        for (name, value) in function.captures.iter() {
            bindings.insert(name.to_string(), value.clone());
        }

        for (param, arg) in function.def.params.iter().zip(args.iter()) {
            bindings.insert(param.name.clone(), arg.clone());
        }

        for (name, overloads) in &self.functions {
            if !bindings.contains_key(name) {
                bindings.insert(
                    name.clone(),
                    Value::Function(FunctionValue {
                        def: overloads[0].clone(),
                        captures: Arc::from([]),
                    }),
                );
            }
        }

        self.eval_block(&function.def.body, &bindings).into_value()
    }

    fn eval_block(&self, block: &instr::Block, bindings: &HashMap<String, Value>) -> BuildResult {
        let mut local_bindings = bindings.clone();

        for (index, stmt) in block.stmts.iter().enumerate() {
            let is_last = index + 1 == block.stmts.len();
            match stmt {
                instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                    match self.eval_expr(value, &local_bindings) {
                        BuildResult::Value(value) => {
                            local_bindings.insert(name.clone(), value);
                        }
                        other => return other,
                    }
                }
                instr::Stmt::Return(expr) => match self.eval_expr(expr, &local_bindings) {
                    BuildResult::Value(value) | BuildResult::Returned(value) => {
                        return BuildResult::Returned(value);
                    }
                    BuildResult::Poisoned(error) => return BuildResult::Poisoned(error),
                },
                instr::Stmt::FnDecl(def) => {
                    local_bindings.insert(
                        def.name.clone(),
                        Value::Function(FunctionValue {
                            def: def.clone(),
                            captures: capture_bindings(&local_bindings),
                        }),
                    );
                }
                instr::Stmt::Expr(expr) => match self.eval_expr(expr, &local_bindings) {
                    BuildResult::Value(value) => {
                        if is_last {
                            return BuildResult::Value(value);
                        }
                    }
                    other => return other,
                },
            }
        }

        BuildResult::Value(Value::Unit)
    }

    // r[impl eval.forcing]
    fn eval_expr(&self, expr: &instr::Expr, bindings: &HashMap<String, Value>) -> BuildResult {
        match expr {
            instr::Expr::LitInt(n) => BuildResult::Value(Value::U64(*n)),
            instr::Expr::LitFloat(n) => BuildResult::Value(Value::F64(*n)),
            instr::Expr::LitBool(b) => BuildResult::Value(Value::Bool(*b)),
            instr::Expr::LitString(s) => BuildResult::Value(Value::String(Arc::from(s.as_str()))),
            instr::Expr::Unit => BuildResult::Value(Value::Unit),
            instr::Expr::Var(name) => {
                if let Some(value) = bindings.get(name) {
                    BuildResult::Value(value.clone())
                } else if let Some(overloads) = self.functions.get(name) {
                    BuildResult::Value(Value::Function(FunctionValue {
                        def: overloads[0].clone(),
                        captures: Arc::from([]),
                    }))
                } else {
                    BuildResult::Poisoned(self.unresolved_name(name))
                }
            }
            instr::Expr::Binary { op, left, right } => {
                let left = match self.eval_expr(left, bindings) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                let right = match self.eval_expr(right, bindings) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                if left.key().is_some() && right.key().is_some() {
                    BuildResult::from_result(self.compute_binary(*op, &left, &right))
                } else {
                    BuildResult::Value(self.pending_value(
                        NodeJob::Binary {
                            op: *op,
                            left,
                            right,
                        },
                        format!("binary({op:?})"),
                    ))
                }
            }
            instr::Expr::Unary { op, operand } => {
                let operand = match self.eval_expr(operand, bindings) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                if operand.key().is_some() {
                    BuildResult::from_result(self.compute_unary(*op, &operand))
                } else {
                    BuildResult::Value(self.pending_value(
                        NodeJob::Unary { op: *op, operand },
                        format!("unary({op:?})"),
                    ))
                }
            }
            instr::Expr::Block(block) => self.eval_block(block, bindings),
            // r[impl eval.control-demand]
            instr::Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond = match self.eval_expr(cond, bindings) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                match cond {
                    Value::Bool(true) => self.eval_block(then_branch, bindings),
                    Value::Bool(false) => self.eval_block(else_branch, bindings),
                    Value::Pending(_) => BuildResult::Value(self.pending_value(
                        NodeJob::If {
                            cond,
                            then_branch: then_branch.clone(),
                            else_branch: else_branch.clone(),
                            bindings: Arc::new(bindings.clone()),
                        },
                        "if(..)".into(),
                    )),
                    other => BuildResult::Poisoned(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            instr::Expr::Call { func, args } => {
                let function = match self.eval_expr(func, bindings) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    match self.eval_expr(arg, bindings) {
                        BuildResult::Value(value) => arg_values.push(value),
                        other => return other,
                    }
                }
                BuildResult::Value(self.build_call(function, arg_values))
            }
            instr::Expr::StructLit { name, fields } => {
                let mut values = Vec::with_capacity(fields.len());
                for (field_name, field_expr) in fields {
                    match self.eval_expr(field_expr, bindings) {
                        BuildResult::Value(value) => {
                            values.push((Arc::from(field_name.as_str()), value));
                        }
                        other => return other,
                    }
                }
                BuildResult::Value(Value::Struct(StructValue {
                    type_name: Arc::from(name.as_str()),
                    fields: Arc::from(values),
                }))
            }
            instr::Expr::Field { receiver, field } => {
                let receiver = match self.eval_expr(receiver, bindings) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                match &receiver {
                    Value::Struct(_) | Value::Tuple(_) => {
                        BuildResult::from_result(self.extract_field(&receiver, field))
                    }
                    Value::Pending(_) => BuildResult::Value(self.pending_value(
                        NodeJob::Field {
                            receiver,
                            field: Arc::from(field.as_str()),
                        },
                        format!("field(.{field})"),
                    )),
                    other => BuildResult::Poisoned(self.type_error(&format!(
                        "cannot access field '{}' on {}",
                        field,
                        other.type_name()
                    ))),
                }
            }
            instr::Expr::Tuple(elems) => {
                let mut values = Vec::with_capacity(elems.len());
                for elem in elems {
                    match self.eval_expr(elem, bindings) {
                        BuildResult::Value(value) => values.push(value),
                        other => return other,
                    }
                }
                BuildResult::Value(Value::Tuple(Arc::from(values)))
            }
        }
    }

    fn extract_field(&self, receiver: &Value, field: &str) -> Result<Value, PoisonError> {
        match receiver {
            Value::Struct(struct_value) => struct_value
                .fields
                .iter()
                .find(|(name, _)| name.as_ref() == field)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| {
                    self.type_error(&format!(
                        "no field '{}' on struct {}",
                        field, struct_value.type_name
                    ))
                }),
            Value::Tuple(elems) => {
                if let Some(index) = field.strip_prefix('_')
                    && let Ok(index) = index.parse::<usize>()
                    && let Some(value) = elems.get(index)
                {
                    return Ok(value.clone());
                }
                Err(self.type_error(&format!("invalid tuple field '{}'", field)))
            }
            other => Err(self.type_error(&format!(
                "cannot access field '{}' on {}",
                field,
                other.type_name()
            ))),
        }
    }

    fn compute_binary(&self, op: BinOp, left: &Value, right: &Value) -> Result<Value, PoisonError> {
        match (op, left, right) {
            (BinOp::Add, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_add(*b))),
            (BinOp::Sub, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_sub(*b))),
            (BinOp::Mul, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_mul(*b))),
            (BinOp::Div, Value::U64(_), Value::U64(0)) => {
                Err(self.arithmetic_error("division by zero"))
            }
            (BinOp::Div, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a / b)),
            (BinOp::Mod, Value::U64(_), Value::U64(0)) => {
                Err(self.arithmetic_error("modulo by zero"))
            }
            (BinOp::Mod, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a % b)),

            (BinOp::Add, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_add(*b))),
            (BinOp::Sub, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_sub(*b))),
            (BinOp::Mul, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_mul(*b))),
            (BinOp::Div, Value::I64(_), Value::I64(0)) => {
                Err(self.arithmetic_error("division by zero"))
            }
            (BinOp::Div, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a / b)),

            (BinOp::Add, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a + b)),
            (BinOp::Sub, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a - b)),
            (BinOp::Mul, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a * b)),
            (BinOp::Div, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a / b)),

            (BinOp::Eq, a, b) => Ok(Value::Bool(a == b)),
            (BinOp::Ne, a, b) => Ok(Value::Bool(a != b)),

            (BinOp::Lt, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::Lt, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::Lt, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::And, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
            (BinOp::Or, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),

            (BinOp::Add, Value::String(a), Value::String(b)) => {
                let mut value = String::with_capacity(a.len() + b.len());
                value.push_str(a);
                value.push_str(b);
                Ok(Value::String(Arc::from(value)))
            }

            _ => Err(self.type_error(&format!(
                "cannot apply {:?} to {} and {}",
                op,
                left.type_name(),
                right.type_name()
            ))),
        }
    }

    fn compute_unary(&self, op: UnaryOp, value: &Value) -> Result<Value, PoisonError> {
        match (op, value) {
            (UnaryOp::Neg, Value::I64(n)) => Ok(Value::I64(-n)),
            (UnaryOp::Neg, Value::F64(n)) => Ok(Value::F64(-n)),
            (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            _ => Err(self.type_error(&format!("cannot apply {:?} to {}", op, value.type_name()))),
        }
    }

    fn resolve_function(
        &self,
        name: &str,
        _args: &[Value],
    ) -> Result<Arc<instr::FnDef>, PoisonError> {
        self.functions
            .get(name)
            .and_then(|overloads| overloads.first().cloned())
            .ok_or_else(|| self.unresolved_function(name))
    }

    fn unresolved_name(&self, name: &str) -> PoisonError {
        PoisonError {
            kind: PoisonKind::UnresolvedName,
            message: format!("unresolved name: {name}"),
            demand_chain: self.current_demand_chain(None),
        }
    }

    fn unresolved_function(&self, name: &str) -> PoisonError {
        PoisonError {
            kind: PoisonKind::UnresolvedName,
            message: format!("no function named '{name}'"),
            demand_chain: self.current_demand_chain(None),
        }
    }

    fn type_error(&self, message: &str) -> PoisonError {
        PoisonError {
            kind: PoisonKind::TypeError,
            message: message.into(),
            demand_chain: self.current_demand_chain(None),
        }
    }

    fn arithmetic_error(&self, message: &str) -> PoisonError {
        PoisonError {
            kind: PoisonKind::ArithmeticError,
            message: message.into(),
            demand_chain: self.current_demand_chain(None),
        }
    }

    fn internal_error(&self, message: &str) -> PoisonError {
        PoisonError {
            kind: PoisonKind::Internal,
            message: message.into(),
            demand_chain: self.current_demand_chain(None),
        }
    }

    fn current_demand_chain(&self, next: Option<NodeId>) -> DemandChain {
        let mut entries = Vec::new();
        ACTIVE_NODES.with(|stack| {
            for node_id in stack.borrow().iter().copied().chain(next) {
                if let Some(node) = self.nodes.get(&node_id) {
                    entries.push(DemandEntry {
                        node_id,
                        description: node.description.clone(),
                    });
                }
            }
        });
        DemandChain { entries }
    }

    fn with_active_node<T>(&self, node_id: NodeId, f: impl FnOnce() -> T) -> T {
        ACTIVE_NODES.with(|stack| stack.borrow_mut().push(node_id));
        let result = f();
        ACTIVE_NODES.with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some(node_id));
        });
        result
    }
}

impl BuildResult {
    fn from_result(result: Result<Value, PoisonError>) -> Self {
        match result {
            Ok(value) => Self::Value(value),
            Err(error) => Self::Poisoned(error),
        }
    }

    fn into_value(self) -> Result<Value, PoisonError> {
        match self {
            BuildResult::Value(value) | BuildResult::Returned(value) => Ok(value),
            BuildResult::Poisoned(error) => Err(error),
        }
    }
}

fn capture_bindings(bindings: &HashMap<String, Value>) -> Arc<[(Arc<str>, Value)]> {
    let mut captures = bindings
        .iter()
        .map(|(name, value)| (Arc::<str>::from(name.as_str()), value.clone()))
        .collect::<Vec<_>>();
    captures.sort_by(|left, right| left.0.cmp(&right.0));
    Arc::from(captures)
}
