//! Demand-driven evaluation.
//!
//! Expressions build lazy value graphs. Forcing only happens at the
//! external boundary or inside a node whose operation truly needs a
//! resolved dependency.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;
#[cfg(test)]
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tempfile::TempDir;

use crate::exec::{ExecArgValue, ExecMount, ExecRequest, SharedExecHandler};
use crate::instr::{self, BinOp, UnaryOp};
use crate::node::{
    Node, NodeId, NodeJob, NodeKey, Outcome, TemplateKind, TemplatePartValue, Waiter,
};
use crate::poison::{DemandChain, DemandEntry, PoisonError, PoisonKind};
use crate::transport::{ShippedFunction, TransportModule, TransportValue};
use crate::value::{
    BlobValue, BuiltinFunction, BuiltinMethod, BuiltinNamespace, BuiltinValue, ExecPathValue,
    FunctionValue, HostValue, MapValue, PathValue, StructValue, TreeValue, Value,
};

thread_local! {
    static DEMAND_STACK: RefCell<Vec<NodeId>> = const { RefCell::new(Vec::new()) };
}

const DEFAULT_MAX_DEMAND_DEPTH: usize = 1_024;
const IMPLICIT_EXEC_MOUNT_PREFIX: &str = ".vx/implicit";

enum BuildResult {
    Value(Value),
    Returned(Value),
    Poisoned(PoisonError),
}

#[derive(Clone)]
struct ParsedExecRequest {
    args: Vec<ExecArgValue>,
    env: Vec<(String, ExecArgValue)>,
    mounts: Vec<(PathValue, Value)>,
    writable_roots: Vec<PathValue>,
    observer: Value,
    verifier: Option<Value>,
}

/// The evaluation engine.
pub struct Engine {
    module: instr::Module,
    nodes: DashMap<NodeId, Arc<Node>>,
    call_memo: DashMap<NodeKey, Arc<Node>>,
    functions: HashMap<String, Vec<Arc<instr::FnDef>>>,
    function_identities: HashMap<usize, Arc<str>>,
    max_demand_depth: usize,
    exec_handler: Option<SharedExecHandler>,
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
        let function_identities = compute_function_identities(&module.functions);

        Self {
            module: module.clone(),
            nodes: DashMap::new(),
            call_memo: DashMap::new(),
            functions,
            function_identities,
            max_demand_depth: DEFAULT_MAX_DEMAND_DEPTH,
            exec_handler: None,
        }
    }

    // r[impl eval.demand-depth]
    pub fn with_max_demand_depth(mut self, max_demand_depth: usize) -> Self {
        self.max_demand_depth = max_demand_depth;
        self
    }

    pub fn with_exec_handler(mut self, exec_handler: SharedExecHandler) -> Self {
        self.exec_handler = Some(exec_handler);
        self
    }

    // r[impl eval.forcing.boundary]
    /// Evaluate a top-level function by name at the external forcing boundary.
    pub fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        let function = self.global_function_value(self.resolve_function(name, &args)?);
        let value = self.build_call(Value::Function(function), args);
        self.force_value(value)
    }

    pub fn transport_module(&self) -> TransportModule {
        TransportModule::from(&self.module)
    }

    pub fn ship_value(&self, value: Value) -> Result<TransportValue, PoisonError> {
        let value = self.force_value(value)?;
        TransportValue::try_from(&value).map_err(|message| self.type_error(&message))
    }

    pub fn ship_function(&self, value: Value) -> Result<ShippedFunction, PoisonError> {
        let value = self.force_value(value)?;
        let Value::Function(function) = value else {
            return Err(self.type_error("shipped observers and verifiers must be functions"));
        };
        ShippedFunction::try_from(&function).map_err(|message| self.type_error(&message))
    }

    pub fn invoke_shipped(
        module: &TransportModule,
        function: &ShippedFunction,
        args: Vec<Value>,
    ) -> Result<Value, PoisonError> {
        let module: instr::Module = module.clone().into();
        let engine = Self::from_module(&module);
        let function = function
            .to_runtime()
            .map_err(|message| engine.type_error(&message))?;
        let value =
            engine.execute_call(Value::Function(function), Arc::from(args), Arc::from([]))?;
        engine.force_value(value)
    }

    #[cfg(test)]
    pub(crate) fn call_unforced(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        let function = self.global_function_value(self.resolve_function(name, &args)?);
        Ok(self.build_call(Value::Function(function), args))
    }

    #[cfg(test)]
    pub(crate) fn force_shallow(&self, value: Value) -> Result<Value, PoisonError> {
        self.force_value_shallow(value)
    }

    #[cfg(test)]
    pub(crate) fn force_fully(&self, value: Value) -> Result<Value, PoisonError> {
        self.force_value(value)
    }

    #[cfg(test)]
    pub(crate) fn poison_value(&self, message: &str) -> Value {
        let error = PoisonError::new(PoisonKind::UserFail, message, DemandChain::new());
        self.pending_value(NodeJob::Poison(error), format!("poison({message})"))
    }

    #[cfg(test)]
    pub(crate) fn memoized_call_count(&self) -> usize {
        self.call_memo.len()
    }

    #[cfg(test)]
    pub(crate) fn sleep_value(&self, millis: u64, value: Value) -> Value {
        self.pending_value(
            NodeJob::Sleep { millis, value },
            format!("sleep({millis}ms)"),
        )
    }

    #[cfg(test)]
    pub(crate) fn function_key(&self, name: &str) -> Result<String, PoisonError> {
        self.global_function_value(self.resolve_function(name, &[])?)
            .key()
            .ok_or_else(|| self.internal_error("top-level function key should be resolved"))
    }

    // r[impl eval.entry]
    pub fn call_entry(
        &self,
        name: &str,
        workspace: impl AsRef<str>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    ) -> Result<Value, PoisonError> {
        let workspace = Value::Path(PathValue::new(workspace.as_ref()));
        let args = Value::Array(Arc::from(
            args.into_iter()
                .map(|arg| Value::String(Arc::from(arg)))
                .collect::<Vec<_>>(),
        ));
        let env = Value::Map(MapValue {
            entries: Arc::from(
                env.into_iter()
                    .map(|(key, value)| {
                        (
                            Value::String(Arc::from(key)),
                            Value::String(Arc::from(value)),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        });

        let value = self.call(name, vec![workspace, args, env])?;
        match value {
            Value::Tree(_) => Ok(value),
            other => Err(self.type_error(&format!(
                "build entry must return Tree, got {}",
                other.type_name()
            ))),
        }
    }

    fn global_function_value(&self, def: Arc<instr::FnDef>) -> FunctionValue {
        FunctionValue {
            code_identity: self.function_identity(&def),
            def,
            captures: Arc::from([]),
        }
    }

    fn local_function_value(
        &self,
        def: Arc<instr::FnDef>,
        captures: Arc<[(Arc<str>, Value)]>,
    ) -> FunctionValue {
        FunctionValue {
            code_identity: Arc::from(stable_fn_repr(&def)),
            def,
            captures,
        }
    }

    fn closure_function_value(
        &self,
        def: Arc<instr::FnDef>,
        code_identity: Arc<str>,
        captures: Arc<[(Arc<str>, Value)]>,
    ) -> FunctionValue {
        FunctionValue {
            code_identity,
            def,
            captures,
        }
    }

    fn function_identity(&self, def: &Arc<instr::FnDef>) -> Arc<str> {
        self.function_identities
            .get(&fn_ptr(def))
            .cloned()
            .unwrap_or_else(|| Arc::from(stable_fn_repr(def)))
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
                kwargs: Arc::from([]),
            },
            "call(..)".into(),
        )
    }

    fn build_call_with_kwargs(
        &self,
        function: Value,
        args: Vec<Value>,
        kwargs: Vec<(Arc<str>, Value)>,
    ) -> Value {
        if kwargs.is_empty() {
            return self.build_call(function, args);
        }

        self.pending_value(
            NodeJob::Call {
                function,
                args: Arc::from(args),
                kwargs: Arc::from(kwargs),
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

    fn force_value_shallow(&self, value: Value) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => self.force_pending_shallow(node_id),
            other => Ok(other),
        }
    }

    // r[impl eval.forcing.boundary]
    fn force_value(&self, value: Value) -> Result<Value, PoisonError> {
        match value {
            Value::Pending(node_id) => self.with_demand(node_id, || {
                let value = self.force_node(node_id)?;
                self.force_value(value)
            }),
            Value::Tuple(elems) => {
                let forced = self.force_many(elems.iter().cloned().collect())?;
                Ok(Value::Tuple(Arc::from(forced)))
            }
            Value::Array(items) => {
                let forced = self.force_many(items.iter().cloned().collect())?;
                Ok(Value::Array(Arc::from(forced)))
            }
            Value::Map(map) => {
                let keys = map
                    .entries
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                let values = map
                    .entries
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect::<Vec<_>>();
                let forced_keys = self.force_many(keys)?;
                let forced_values = self.force_many(values)?;
                Ok(Value::Map(MapValue {
                    entries: Arc::from(
                        forced_keys
                            .into_iter()
                            .zip(forced_values)
                            .collect::<Vec<_>>(),
                    ),
                }))
            }
            Value::Tree(tree) => match tree {
                TreeValue::Inline(entries) => {
                    let names = entries
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>();
                    let values = entries
                        .iter()
                        .map(|(_, value)| value.clone())
                        .collect::<Vec<_>>();
                    let forced = self.force_many(values)?;
                    self.validate_tree_entries(&forced)?;
                    Ok(Value::Tree(TreeValue::Inline(Arc::from(
                        names.into_iter().zip(forced).collect::<Vec<_>>(),
                    ))))
                }
                TreeValue::FsDir(path) => Ok(Value::Tree(TreeValue::FsDir(path))),
                TreeValue::Store(root) => Ok(Value::Tree(TreeValue::Store(root))),
            },
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
    // r[impl error.propagation]
    fn force_many(&self, values: Vec<Value>) -> Result<Vec<Value>, PoisonError> {
        if values.len() <= 1 {
            let mut forced = Vec::with_capacity(values.len());
            let mut errors = Vec::new();
            for value in values {
                match self.force_value(value) {
                    Ok(value) => forced.push(value),
                    Err(error) => errors.push(error),
                }
            }
            return if errors.is_empty() {
                Ok(forced)
            } else {
                Err(self.aggregate_poison_errors("forcing dependency", errors))
            };
        }

        thread::scope(|scope| {
            let handles = values
                .into_iter()
                .map(|value| scope.spawn(move || self.force_value(value)))
                .collect::<Vec<_>>();

            let mut forced = Vec::with_capacity(handles.len());
            let mut errors = Vec::new();
            for handle in handles {
                match handle.join() {
                    Ok(Ok(value)) => forced.push(value),
                    Ok(Err(error)) => errors.push(error),
                    Err(_) => errors.push(self.internal_error("force worker panicked")),
                }
            }
            if errors.is_empty() {
                Ok(forced)
            } else {
                Err(self.aggregate_poison_errors("forcing dependencies", errors))
            }
        })
    }

    fn force_node(&self, node_id: NodeId) -> Result<Value, PoisonError> {
        let node = self
            .nodes
            .get(&node_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| self.internal_error(&format!("missing node {}", node_id.0)))?;
        let (sender, receiver) = crossbeam_channel::bounded(1);

        match node.try_start_or_wait(Waiter { sender }) {
            Ok(()) => {
                let result = self.execute_node(&node);
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
            NodeJob::Template { kind, parts } => {
                let values = self.force_template_parts(parts)?;
                match kind {
                    TemplateKind::String => self.interpolate_string_parts(&values),
                    TemplateKind::Path => self.interpolate_path_parts(&values),
                }
            }
            NodeJob::If {
                cond,
                then_branch,
                else_branch,
                bindings,
                scope_identity,
            } => {
                let cond = self.force_value(cond.clone())?;
                match cond {
                    Value::Bool(true) => self
                        .eval_block(then_branch, bindings.as_ref(), scope_identity.as_ref())
                        .into_value(),
                    Value::Bool(false) => self
                        .eval_block(else_branch, bindings.as_ref(), scope_identity.as_ref())
                        .into_value(),
                    other => Err(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            NodeJob::Call {
                function,
                args,
                kwargs,
            } => self.execute_call(function.clone(), args.clone(), kwargs.clone()),
            NodeJob::Invoke { function, args } => self.invoke_function(function, args.clone()),
            NodeJob::Field { receiver, field } => {
                let receiver = self.force_value_shallow(receiver.clone())?;
                self.select_field(&receiver, field)
            }
            NodeJob::Fail { message } => Err(self.user_fail(message.clone())?),
            #[cfg(test)]
            NodeJob::Sleep { millis, value } => {
                thread::sleep(Duration::from_millis(*millis));
                Ok(value.clone())
            }
            #[cfg(test)]
            NodeJob::Poison(error) => Err(error.clone()),
        }
    }

    // r[impl eval.enter-functions]
    fn execute_call(
        &self,
        function: Value,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
    ) -> Result<Value, PoisonError> {
        let function = match self.force_value(function)? {
            Value::Function(function) => function,
            Value::Builtin(builtin) => return self.invoke_builtin(&builtin, &args, &kwargs),
            other => {
                return Err(self.type_error(&format!("cannot call {}", other.type_name())));
            }
        };

        if !kwargs.is_empty() {
            return Err(self.type_error("keyword arguments are only supported for builtins"));
        }

        if function.key().is_some() && args.iter().all(|value| value.key().is_some()) {
            return self.force_pending_shallow(
                self.memoized_invoke_node(function, args.iter().cloned().collect()),
            );
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
                    Value::Function(self.global_function_value(overloads[0].clone())),
                );
            }
        }

        self.eval_block(
            &function.def.body,
            &bindings,
            function.code_identity.as_ref(),
        )
        .into_value()
    }

    fn eval_block(
        &self,
        block: &instr::Block,
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        let mut local_bindings = bindings.clone();

        for (index, stmt) in block.stmts.iter().enumerate() {
            let is_last = index + 1 == block.stmts.len();
            match stmt {
                instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                    match self.eval_expr(value, &local_bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            local_bindings.insert(name.clone(), value);
                        }
                        other => return other,
                    }
                }
                instr::Stmt::Return(expr) => {
                    match self.eval_expr(expr, &local_bindings, current_code_identity) {
                        BuildResult::Value(value) | BuildResult::Returned(value) => {
                            return BuildResult::Returned(value);
                        }
                        BuildResult::Poisoned(error) => return BuildResult::Poisoned(error),
                    }
                }
                instr::Stmt::FnDecl(def) => {
                    local_bindings.insert(
                        def.name.clone(),
                        Value::Function(
                            self.local_function_value(
                                def.clone(),
                                capture_bindings(&local_bindings),
                            ),
                        ),
                    );
                }
                instr::Stmt::Expr(expr) => {
                    match self.eval_expr(expr, &local_bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            if is_last {
                                return BuildResult::Value(value);
                            }
                        }
                        other => return other,
                    }
                }
            }
        }

        BuildResult::Value(Value::Unit)
    }

    // r[impl eval.forcing]
    fn eval_expr(
        &self,
        expr: &instr::Expr,
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        match expr {
            instr::Expr::LitInt(n) => BuildResult::Value(Value::U64(*n)),
            instr::Expr::LitFloat(n) => BuildResult::Value(Value::F64(*n)),
            instr::Expr::LitBool(b) => BuildResult::Value(Value::Bool(*b)),
            instr::Expr::LitString(s) => BuildResult::Value(Value::String(Arc::from(s.as_str()))),
            instr::Expr::StringTemplate(parts) => self.eval_template_expr(
                TemplateKind::String,
                parts,
                bindings,
                current_code_identity,
            ),
            instr::Expr::PathTemplate(parts) => {
                self.eval_template_expr(TemplateKind::Path, parts, bindings, current_code_identity)
            }
            instr::Expr::Unit => BuildResult::Value(Value::Unit),
            instr::Expr::Var(name) => {
                if let Some(value) = bindings.get(name) {
                    BuildResult::Value(value.clone())
                } else if name == "None" {
                    BuildResult::Value(option_none())
                } else if let Some(overloads) = self.functions.get(name) {
                    BuildResult::Value(Value::Function(
                        self.global_function_value(overloads[0].clone()),
                    ))
                } else if let Some(builtin) = builtin_namespace(name) {
                    BuildResult::Value(Value::Builtin(builtin))
                } else {
                    BuildResult::Poisoned(self.unresolved_name(name))
                }
            }
            instr::Expr::Binary { op, left, right } => {
                let left = match self.eval_expr(left, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                let right = match self.eval_expr(right, bindings, current_code_identity) {
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
                let operand = match self.eval_expr(operand, bindings, current_code_identity) {
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
            instr::Expr::Block(block) => self.eval_block(block, bindings, current_code_identity),
            // r[impl eval.control-demand]
            instr::Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond = match self.eval_expr(cond, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                match cond {
                    Value::Bool(true) => {
                        self.eval_block(then_branch, bindings, current_code_identity)
                    }
                    Value::Bool(false) => {
                        self.eval_block(else_branch, bindings, current_code_identity)
                    }
                    Value::Pending(_) => BuildResult::Value(self.pending_value(
                        NodeJob::If {
                            cond,
                            then_branch: then_branch.clone(),
                            else_branch: else_branch.clone(),
                            bindings: Arc::new(bindings.clone()),
                            scope_identity: Arc::from(current_code_identity),
                        },
                        "if(..)".into(),
                    )),
                    other => BuildResult::Poisoned(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            // r[impl fn.closure]
            // r[impl fn.identity.closure]
            instr::Expr::Closure { closure_id, def } => {
                BuildResult::Value(Value::Function(self.closure_function_value(
                    def.clone(),
                    Arc::from(format!("{current_code_identity}::closure#{closure_id}")),
                    capture_bindings(bindings),
                )))
            }
            instr::Expr::Call { func, args, kwargs } => {
                let function = match self.eval_expr(func, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    match self.eval_expr(arg, bindings, current_code_identity) {
                        BuildResult::Value(value) => arg_values.push(value),
                        other => return other,
                    }
                }
                let mut kwarg_values = Vec::with_capacity(kwargs.len());
                for (name, arg) in kwargs {
                    match self.eval_expr(arg, bindings, current_code_identity) {
                        BuildResult::Value(value) => {
                            kwarg_values.push((Arc::from(name.as_str()), value));
                        }
                        other => return other,
                    }
                }
                BuildResult::Value(self.build_call_with_kwargs(function, arg_values, kwarg_values))
            }
            instr::Expr::StructLit { name, fields } => {
                let mut values = Vec::with_capacity(fields.len());
                for (field_name, field_expr) in fields {
                    match self.eval_expr(field_expr, bindings, current_code_identity) {
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
            instr::Expr::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    match self.eval_expr(item, bindings, current_code_identity) {
                        BuildResult::Value(value) => values.push(value),
                        other => return other,
                    }
                }
                BuildResult::Value(Value::Array(Arc::from(values)))
            }
            instr::Expr::Map(entries) => {
                let mut values = Vec::with_capacity(entries.len());
                for (key_expr, value_expr) in entries {
                    let key = match self.eval_expr(key_expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    let value = match self.eval_expr(value_expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    values.push((key, value));
                }
                BuildResult::Value(Value::Map(MapValue {
                    entries: Arc::from(values),
                }))
            }
            instr::Expr::Field { receiver, field } => {
                let receiver = match self.eval_expr(receiver, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };

                if matches!(receiver, Value::Pending(_)) {
                    BuildResult::Value(self.pending_value(
                        NodeJob::Field {
                            receiver,
                            field: Arc::from(field.as_str()),
                        },
                        format!("field(.{field})"),
                    ))
                } else {
                    BuildResult::from_result(self.select_field(&receiver, field))
                }
            }
            // r[impl error.fail]
            instr::Expr::Fail(message) => {
                let message = match self.eval_expr(message, bindings, current_code_identity) {
                    BuildResult::Value(value) => value,
                    other => return other,
                };
                BuildResult::Value(self.pending_value(NodeJob::Fail { message }, "fail(..)".into()))
            }
            instr::Expr::Tuple(elems) => {
                let mut values = Vec::with_capacity(elems.len());
                for elem in elems {
                    match self.eval_expr(elem, bindings, current_code_identity) {
                        BuildResult::Value(value) => values.push(value),
                        other => return other,
                    }
                }
                BuildResult::Value(Value::Tuple(Arc::from(values)))
            }
        }
    }

    fn eval_template_expr(
        &self,
        kind: TemplateKind,
        parts: &[instr::TemplatePart],
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                instr::TemplatePart::Text(text) => {
                    values.push(TemplatePartValue::Text(Arc::from(text.as_str())));
                }
                instr::TemplatePart::Expr(expr) => {
                    let value = match self.eval_expr(expr, bindings, current_code_identity) {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    values.push(TemplatePartValue::Expr(value));
                }
            }
        }

        let needs_node = values.iter().any(|part| match part {
            TemplatePartValue::Text(_) => false,
            TemplatePartValue::Expr(value) => value.key().is_none(),
        });
        if needs_node {
            return BuildResult::Value(self.pending_value(
                NodeJob::Template {
                    kind,
                    parts: Arc::from(values),
                },
                format!("template({kind:?})"),
            ));
        }

        let result = match kind {
            TemplateKind::String => self.interpolate_string_parts(&values),
            TemplateKind::Path => self.interpolate_path_parts(&values),
        };
        BuildResult::from_result(result)
    }

    fn force_template_parts(
        &self,
        parts: &[TemplatePartValue],
    ) -> Result<Vec<TemplatePartValue>, PoisonError> {
        let values = parts
            .iter()
            .filter_map(|part| match part {
                TemplatePartValue::Text(_) => None,
                TemplatePartValue::Expr(value) => Some(value.clone()),
            })
            .collect::<Vec<_>>();
        let forced = self.force_many(values)?;
        let mut forced_iter = forced.into_iter();
        Ok(parts
            .iter()
            .map(|part| match part {
                TemplatePartValue::Text(text) => TemplatePartValue::Text(text.clone()),
                TemplatePartValue::Expr(_) => {
                    TemplatePartValue::Expr(forced_iter.next().expect("forced value for template"))
                }
            })
            .collect())
    }

    // r[expr.literal.interpolation]
    fn interpolate_string_parts(&self, parts: &[TemplatePartValue]) -> Result<Value, PoisonError> {
        let mut value = String::new();
        for part in parts {
            match part {
                TemplatePartValue::Text(text) => value.push_str(text),
                TemplatePartValue::Expr(value_part) => {
                    value.push_str(&self.stringify_interpolated_value(value_part)?);
                }
            }
        }
        Ok(Value::String(Arc::from(value)))
    }

    // r[exec.arg.interpolation]
    fn interpolate_path_parts(&self, parts: &[TemplatePartValue]) -> Result<Value, PoisonError> {
        let mut plain = String::new();
        let mut mounted_tree: Option<(TreeValue, String)> = None;
        let mut mounted_blob: Option<(BlobValue, String)> = None;

        for part in parts {
            match part {
                TemplatePartValue::Text(text) => plain.push_str(text),
                TemplatePartValue::Expr(value) => match value {
                    Value::String(text) => plain.push_str(text),
                    Value::Path(path) => plain.push_str(path.as_str()),
                    Value::Tree(tree) => {
                        if mounted_tree.is_some() || mounted_blob.is_some() {
                            return Err(self.type_error(
                                "path literal cannot interpolate more than one mount-bearing value",
                            ));
                        }
                        mounted_tree = Some((tree.clone(), plain.clone()));
                        plain.clear();
                    }
                    Value::Blob(blob) => {
                        if mounted_tree.is_some() || mounted_blob.is_some() {
                            return Err(self.type_error(
                                "path literal cannot interpolate more than one mount-bearing value",
                            ));
                        }
                        mounted_blob = Some((blob.clone(), plain.clone()));
                        plain.clear();
                    }
                    Value::ExecPath(exec_path) => match exec_path {
                        ExecPathValue::InTree { tree, suffix } => {
                            if mounted_tree.is_some() || mounted_blob.is_some() {
                                return Err(self.type_error(
                                    "path literal cannot interpolate more than one mount-bearing value",
                                ));
                            }
                            plain.push_str(suffix.as_str());
                            mounted_tree = Some((tree.clone(), plain.clone()));
                            plain.clear();
                        }
                        ExecPathValue::InBlob { blob, leaf } => {
                            if mounted_tree.is_some() || mounted_blob.is_some() {
                                return Err(self.type_error(
                                    "path literal cannot interpolate more than one mount-bearing value",
                                ));
                            }
                            plain.push_str(leaf.as_str());
                            mounted_blob = Some((blob.clone(), plain.clone()));
                            plain.clear();
                        }
                    },
                    other => {
                        return Err(self.type_error(&format!(
                            "path interpolation requires String, Path, Tree, Blob, or ExecPath, got {}",
                            other.type_name()
                        )));
                    }
                },
            }
        }

        if let Some((tree, prefix)) = mounted_tree {
            let suffix = PathValue::new(format!("{prefix}{plain}").trim_start_matches('/'));
            return Ok(Value::ExecPath(ExecPathValue::InTree { tree, suffix }));
        }

        if let Some((blob, leaf)) = mounted_blob {
            let leaf = format!("{leaf}{plain}");
            let leaf = leaf.trim_start_matches('/').to_string();
            let normalized = PathValue::new(&leaf);
            if normalized.as_str().contains('/') {
                return Err(self
                    .type_error("path literal cannot append path segments to a blob-backed path"));
            }
            return Ok(Value::ExecPath(ExecPathValue::InBlob {
                blob,
                leaf: normalized,
            }));
        }

        Ok(Value::Path(PathValue::new(plain)))
    }

    fn stringify_interpolated_value(&self, value: &Value) -> Result<String, PoisonError> {
        match value {
            Value::String(text) => Ok(text.to_string()),
            Value::Path(path) => Ok(path.as_str().to_string()),
            Value::Bool(_) | Value::U64(_) | Value::I64(_) | Value::F64(_) | Value::Unit => {
                Ok(value.to_string())
            }
            other => Err(self.type_error(&format!(
                "cannot interpolate {} into String",
                other.type_name()
            ))),
        }
    }

    fn select_field(&self, receiver: &Value, field: &str) -> Result<Value, PoisonError> {
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
            Value::Blob(_) if field == "to_string" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobToString {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Tree(_) if field == "file" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::TreeFile {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "read" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostRead {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "hash" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostHash {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "which" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostWhich {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Blob))
                if field == "from_string" =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::BlobFromString,
                )))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree)) if field == "of" => {
                Ok(Value::Builtin(BuiltinValue::Method(BuiltinMethod::TreeOf)))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree)) if field == "empty" => {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::TreeEmpty,
                )))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree))
                if field == "from_path" =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::TreeFromPath,
                )))
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

            (BinOp::Div, Value::Path(base), Value::String(child)) => {
                Ok(Value::Path(base.join(child.as_ref())))
            }
            (BinOp::Div, Value::Path(base), Value::Path(child)) => {
                Ok(Value::Path(base.join(child.as_str())))
            }

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

    fn invoke_builtin(
        &self,
        builtin: &BuiltinValue,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<Value, PoisonError> {
        match builtin {
            BuiltinValue::Namespace(namespace) => {
                Err(self.type_error(&format!("cannot call builtin namespace {:?}", namespace)))
            }
            BuiltinValue::Function(BuiltinFunction::Exec) => self.exec_builtin(args, kwargs),
            BuiltinValue::Method(BuiltinMethod::BlobFromString) => {
                self.require_no_kwargs("Blob.from_string", kwargs)?;
                self.check_arity("Blob.from_string", args, 1)?;
                let value = &args[0];
                let value = self.force_value(value.clone())?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Blob(BlobValue::from_string(text)))
            }
            BuiltinValue::Method(BuiltinMethod::TreeOf) => {
                self.require_no_kwargs("Tree.of", kwargs)?;
                self.check_arity("Tree.of", args, 1)?;
                let value = &args[0];
                let value = self.force_value_shallow(value.clone())?;
                let Value::Map(map) = value else {
                    return Err(
                        self.type_error(&format!("Tree.of expects Map, got {}", value.type_name()))
                    );
                };
                self.tree_from_map(&map)
            }
            BuiltinValue::Method(BuiltinMethod::TreeEmpty) => {
                self.require_no_kwargs("Tree.empty", kwargs)?;
                self.check_arity("Tree.empty", args, 0)?;
                Ok(Value::Tree(TreeValue::Inline(Arc::from([]))))
            }
            BuiltinValue::Method(BuiltinMethod::TreeFromPath) => {
                self.require_no_kwargs("Tree.from_path", kwargs)?;
                self.check_arity("Tree.from_path", args, 1)?;
                let path = self.force_value(args[0].clone())?;
                let path = self.expect_path_value(&path, "Tree.from_path path")?;
                Ok(Value::Tree(TreeValue::FsDir(path)))
            }
            BuiltinValue::Method(BuiltinMethod::BlobToString { receiver }) => {
                self.require_no_kwargs("blob.to_string", kwargs)?;
                self.check_arity("blob.to_string", args, 0)?;
                let receiver = self.force_value(receiver.as_ref().clone())?;
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.to_string receiver must be Blob"));
                };
                let text = blob.to_utf8_string().map_err(|message| {
                    PoisonError::new(
                        PoisonKind::TypeError,
                        message,
                        self.current_demand_chain(None),
                    )
                })?;
                Ok(Value::String(text))
            }
            BuiltinValue::Method(BuiltinMethod::TreeFile { receiver }) => {
                self.require_no_kwargs("tree.file", kwargs)?;
                self.check_arity("tree.file", args, 1)?;
                let path = &args[0];
                let path = self.force_value(path.clone())?;
                let path = self.expect_relative_path(&path, "tree.file path")?;
                let receiver = self.force_value_shallow(receiver.as_ref().clone())?;
                let Value::Tree(tree) = receiver else {
                    return Err(self.type_error("tree.file receiver must be Tree"));
                };
                self.tree_file(&tree, path)
            }
            BuiltinValue::Method(BuiltinMethod::HostRead { receiver }) => {
                self.require_no_kwargs("host.read", kwargs)?;
                self.check_arity("host.read", args, 1)?;
                let path = self.force_value(args[0].clone())?;
                let path = self.expect_host_path(&path, "host.read path")?;
                let receiver = self.force_value_shallow(receiver.as_ref().clone())?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.read receiver must be Host"));
                };
                self.host_read(&path)
            }
            BuiltinValue::Method(BuiltinMethod::HostHash { receiver }) => {
                self.require_no_kwargs("host.hash", kwargs)?;
                self.check_arity("host.hash", args, 1)?;
                let path = self.force_value(args[0].clone())?;
                let path = self.expect_host_path(&path, "host.hash path")?;
                let receiver = self.force_value_shallow(receiver.as_ref().clone())?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.hash receiver must be Host"));
                };
                self.host_hash(&path)
            }
            BuiltinValue::Method(BuiltinMethod::HostWhich { receiver }) => {
                self.require_no_kwargs("host.which", kwargs)?;
                self.check_arity("host.which", args, 1)?;
                let name = self.force_value(args[0].clone())?;
                let name = self.expect_host_path(&name, "host.which name")?;
                let receiver = self.force_value_shallow(receiver.as_ref().clone())?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.which receiver must be Host"));
                };
                Ok(self.host_which(&name))
            }
        }
    }

    fn tree_from_map(&self, map: &MapValue) -> Result<Value, PoisonError> {
        let mut entries = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self.force_value(key.clone())?;
            let name = self.expect_tree_entry_name(&key)?;
            entries.push((Arc::from(name), value.clone()));
        }
        self.validate_tree_entries(
            &entries
                .iter()
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>(),
        )?;
        Ok(Value::Tree(TreeValue::Inline(Arc::from(entries))))
    }

    fn exec_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<Value, PoisonError> {
        let request = self.parse_exec_request(args, kwargs)?;
        if let Some(exec_handler) = &self.exec_handler {
            let request = self.ship_exec_request(&request)?;
            return exec_handler(request);
        }
        self.exec_builtin_local(request)
    }

    fn parse_exec_request(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<ParsedExecRequest, PoisonError> {
        self.check_arity("exec", args, 1)?;

        let mut env_value = None;
        let mut mounts_value = None;
        let mut observer_value = None;
        let mut verifier_value = None;
        let mut writable_value = None;

        for (name, value) in kwargs {
            match name.as_ref() {
                "env" => env_value = Some(value.clone()),
                "mounts" => mounts_value = Some(value.clone()),
                "observer" => observer_value = Some(value.clone()),
                "verifier" => verifier_value = Some(value.clone()),
                "writable" => writable_value = Some(value.clone()),
                other => {
                    return Err(self
                        .type_error(&format!("exec does not accept keyword argument '{other}'")));
                }
            }
        }

        let args_value = self.force_value_shallow(args[0].clone())?;
        let Value::Array(exec_args_raw) = args_value else {
            return Err(self.type_error(&format!(
                "exec expects Array[String | Path | Blob | Tree] for args, got {}",
                args_value.type_name()
            )));
        };

        let observer =
            observer_value.ok_or_else(|| self.type_error("exec requires an observer function"))?;
        let observer = self.force_value(observer)?;
        match &observer {
            Value::Function(_) | Value::Builtin(_) => {}
            other => {
                return Err(self.type_error(&format!(
                    "exec observer must be callable, got {}",
                    other.type_name()
                )));
            }
        }

        let verifier = verifier_value
            .map(|verifier| {
                let verifier = self.force_value(verifier)?;
                match &verifier {
                    Value::Function(_) | Value::Builtin(_) => Ok(verifier),
                    other => Err(self.type_error(&format!(
                        "exec verifier must be callable, got {}",
                        other.type_name()
                    ))),
                }
            })
            .transpose()?;

        let mut mounts = self.exec_mounts_from_value(mounts_value)?;
        let writable_roots = self.exec_writable_from_value(writable_value)?;
        let env = self.exec_env_from_value(env_value, &mut mounts, &writable_roots)?;
        let exec_args = exec_args_raw
            .iter()
            .map(|value| self.parse_exec_arg(value, &mut mounts, &writable_roots, "exec arg"))
            .collect::<Result<Vec<_>, _>>()?;
        if exec_args.is_empty() {
            return Err(self.type_error("exec requires at least one program argument"));
        }

        Ok(ParsedExecRequest {
            args: exec_args,
            env,
            mounts,
            writable_roots,
            observer,
            verifier,
        })
    }

    fn ship_exec_request(&self, request: &ParsedExecRequest) -> Result<ExecRequest, PoisonError> {
        Ok(ExecRequest {
            args: request.args.clone(),
            env: request.env.clone(),
            mounts: request
                .mounts
                .iter()
                .map(|(path, value)| {
                    Ok(ExecMount {
                        path: path.clone(),
                        value: self.snapshot_value(value.clone())?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            writable_roots: request.writable_roots.clone(),
            module: self.transport_module(),
            observer: self.ship_function(request.observer.clone())?,
            verifier: request
                .verifier
                .clone()
                .map(|verifier| self.ship_function(verifier))
                .transpose()?,
        })
    }

    fn exec_builtin_local(&self, request: ParsedExecRequest) -> Result<Value, PoisonError> {
        let sandbox = TempDir::new().map_err(|error| {
            self.internal_error(&format!("failed to create exec sandbox: {error}"))
        })?;
        let sandbox_root = PathValue::new(sandbox.path().to_string_lossy());

        for (mount_path, value) in &request.mounts {
            self.materialize_mount_value(&sandbox_root, mount_path, value)?;
        }
        for root in &request.writable_roots {
            self.ensure_dir(&sandbox_root.join(root.as_str()))?;
        }

        let verifier_task = request
            .verifier
            .clone()
            .map(|verifier| {
                let module = self.transport_module();
                let verifier = self.ship_function(verifier)?;
                // r[impl exec.verifier.host]
                Ok(thread::spawn(move || {
                    Engine::invoke_shipped(&module, &verifier, vec![Value::Host(HostValue)])
                        .map(|_| ())
                }))
            })
            .transpose()?;

        let rendered_args = request
            .args
            .iter()
            .map(|value| self.render_exec_arg(value, &sandbox_root, "exec arg"))
            .collect::<Result<Vec<_>, _>>()?;

        let mut command = Command::new(&rendered_args[0]);
        command.args(&rendered_args[1..]);
        command.current_dir(sandbox.path());
        for (key, value) in &request.env {
            command.env(
                key,
                self.render_exec_arg(value, &sandbox_root, "exec env value")?,
            );
        }

        let output = command.output().map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to execute {}: {error}", rendered_args[0]),
                self.current_demand_chain(None),
            )
        })?;

        let process = self.exec_process_value(&output);
        let tree = Value::Tree(TreeValue::FsDir(sandbox_root.clone()));
        let observed = self.execute_call(
            request.observer,
            Arc::from(vec![process, tree]),
            Arc::from([]),
        );

        if let Some(verifier_task) = verifier_task {
            match verifier_task.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(PoisonError::new(
                        PoisonKind::VerificationFailure,
                        format!("exec verifier failed: {error}"),
                        self.current_demand_chain(None),
                    ));
                }
                Err(_) => {
                    return Err(PoisonError::new(
                        PoisonKind::Internal,
                        "exec verifier task panicked",
                        self.current_demand_chain(None),
                    ));
                }
            }
        }

        let observed = observed?;
        self.snapshot_value(observed)
    }

    fn exec_mounts_from_value(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<(PathValue, Value)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let value = self.force_value_shallow(value)?;
        let Value::Map(map) = value else {
            return Err(self.type_error(&format!(
                "exec mounts must be Map, got {}",
                value.type_name()
            )));
        };

        let mut mounts = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self.force_value(key.clone())?;
            let path = self.expect_relative_path(&key, "exec mount path")?;
            let value = self.force_value_shallow(value.clone())?;
            match value {
                Value::Blob(_) | Value::Tree(_) => mounts.push((path, value)),
                other => {
                    return Err(self.type_error(&format!(
                        "exec mount values must be Blob or Tree, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        Ok(mounts)
    }

    fn exec_writable_from_value(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<PathValue>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let value = self.force_value_shallow(value)?;
        let Value::Array(items) = value else {
            return Err(self.type_error(&format!(
                "exec writable must be Array[Path], got {}",
                value.type_name()
            )));
        };

        let mut writable = Vec::with_capacity(items.len());
        for item in items.iter() {
            let item = self.force_value(item.clone())?;
            writable.push(self.expect_relative_path(&item, "exec writable path")?);
        }
        Ok(writable)
    }

    fn exec_env_from_value(
        &self,
        value: Option<Value>,
        mounts: &mut Vec<(PathValue, Value)>,
        writable_roots: &[PathValue],
    ) -> Result<Vec<(String, ExecArgValue)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let value = self.force_value_shallow(value)?;
        let Value::Map(map) = value else {
            return Err(
                self.type_error(&format!("exec env must be Map, got {}", value.type_name()))
            );
        };

        let mut env = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self.force_value(key.clone())?;
            let key = match key {
                Value::String(key) => key.to_string(),
                other => {
                    return Err(self.type_error(&format!(
                        "exec env keys must be String, got {}",
                        other.type_name()
                    )));
                }
            };
            let value = self.parse_exec_arg(value, mounts, writable_roots, "exec env value")?;
            env.push((key, value));
        }
        Ok(env)
    }

    fn parse_exec_arg(
        &self,
        value: &Value,
        mounts: &mut Vec<(PathValue, Value)>,
        writable_roots: &[PathValue],
        context: &str,
    ) -> Result<ExecArgValue, PoisonError> {
        let value = self.force_value(value.clone())?;
        match value {
            Value::String(text) => Ok(ExecArgValue::Literal(text.to_string())),
            Value::Path(path) => {
                let path = self.expect_relative_path(&Value::Path(path), context)?;
                if !self.exec_path_is_covered(path.as_str(), mounts, writable_roots) {
                    return Err(self.type_error(&format!(
                        "{context} '{}' is not covered by a mount or writable root",
                        path.as_str()
                    )));
                }
                Ok(ExecArgValue::Path(path))
            }
            Value::ExecPath(path) => self.exec_arg_from_exec_path(mounts, &path),
            Value::Blob(blob) => Ok(ExecArgValue::Path(
                self.ensure_blob_exec_mount(mounts, &blob),
            )),
            Value::Tree(tree) => Ok(ExecArgValue::Path(
                self.ensure_tree_exec_mount(mounts, &tree),
            )),
            other => Err(self.type_error(&format!(
                "{context} must be String, Path, Blob, Tree, or ExecPath, got {}",
                other.type_name()
            ))),
        }
    }

    fn exec_arg_from_exec_path(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        path: &ExecPathValue,
    ) -> Result<ExecArgValue, PoisonError> {
        match path {
            ExecPathValue::InTree { tree, suffix } => Ok(ExecArgValue::Path(
                self.ensure_tree_exec_mount(mounts, tree)
                    .join(suffix.as_str()),
            )),
            ExecPathValue::InBlob { blob, leaf } => Ok(ExecArgValue::Path(self.ensure_exec_mount(
                mounts,
                Value::Blob(blob.clone()),
                Some(leaf.as_str().to_string()),
            ))),
        }
    }

    // r[impl exec.arg.auto-mount]
    fn ensure_tree_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        tree: &TreeValue,
    ) -> PathValue {
        let value = Value::Tree(tree.clone());
        self.ensure_exec_mount(mounts, value, None)
    }

    // r[impl exec.arg.auto-mount]
    fn ensure_blob_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        blob: &BlobValue,
    ) -> PathValue {
        let value = Value::Blob(blob.clone());
        let leaf = self.exec_blob_leaf_name(blob);
        self.ensure_exec_mount(mounts, value, Some(leaf))
    }

    // r[impl exec.mount]
    fn ensure_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        value: Value,
        leaf: Option<String>,
    ) -> PathValue {
        if let Some((path, _)) = mounts.iter().find(|(_, mounted)| *mounted == value) {
            return path.clone();
        }

        let mount_root = self.next_exec_mount_root(mounts);
        let mount_path = match leaf {
            Some(leaf) => mount_root.join(&leaf),
            None => mount_root,
        };
        mounts.push((mount_path.clone(), value));
        mount_path
    }

    fn next_exec_mount_root(&self, mounts: &[(PathValue, Value)]) -> PathValue {
        for index in 0usize.. {
            let candidate = PathValue::new(format!("{IMPLICIT_EXEC_MOUNT_PREFIX}/{index}"));
            if mounts
                .iter()
                .all(|(path, _)| !self.exec_paths_overlap(&candidate, path))
            {
                return candidate;
            }
        }
        unreachable!("implicit exec mount path space is infinite")
    }

    fn exec_paths_overlap(&self, left: &PathValue, right: &PathValue) -> bool {
        if left == right {
            return true;
        }
        let left = left.as_str();
        let right = right.as_str();
        right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
            || left
                .strip_prefix(right)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    fn exec_blob_leaf_name(&self, blob: &BlobValue) -> String {
        match blob {
            BlobValue::FsFile(path) => Path::new(path.as_str())
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| self.synthetic_blob_leaf(blob)),
            BlobValue::Inline(_) | BlobValue::Store(_) => self.synthetic_blob_leaf(blob),
        }
    }

    fn synthetic_blob_leaf(&self, blob: &BlobValue) -> String {
        let digest = match blob {
            BlobValue::Inline(bytes) => blake3::hash(bytes.as_ref()).to_hex().to_string(),
            BlobValue::FsFile(path) => blake3::hash(path.as_str().as_bytes()).to_hex().to_string(),
            BlobValue::Store(blob_ref) => blake3::hash(format!("{blob_ref:?}").as_bytes())
                .to_hex()
                .to_string(),
        };
        format!("blob-{}", &digest[..12])
    }

    fn render_exec_arg(
        &self,
        value: &ExecArgValue,
        sandbox_root: &PathValue,
        _context: &str,
    ) -> Result<String, PoisonError> {
        match value {
            ExecArgValue::Literal(text) => Ok(text.clone()),
            ExecArgValue::Path(path) => Ok(sandbox_root.join(path.as_str()).as_str().to_string()),
        }
    }

    fn exec_path_is_covered(
        &self,
        path: &str,
        mounts: &[(PathValue, Value)],
        writable_roots: &[PathValue],
    ) -> bool {
        mounts
            .iter()
            .any(|(mount_path, _)| self.covers_path(mount_path.as_str(), path))
            || writable_roots
                .iter()
                .any(|root| self.covers_path(root.as_str(), path))
    }

    fn covers_path(&self, prefix: &str, path: &str) -> bool {
        prefix.is_empty()
            || path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    fn materialize_mount_value(
        &self,
        sandbox_root: &PathValue,
        mount_path: &PathValue,
        value: &Value,
    ) -> Result<(), PoisonError> {
        let destination = sandbox_root.join(mount_path.as_str());
        self.materialize_value_to_fs(value, &destination)
    }

    fn materialize_value_to_fs(
        &self,
        value: &Value,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        let value = self.force_value_shallow(value.clone())?;
        match value {
            Value::Blob(blob) => self.write_blob_to_path(&blob, destination),
            Value::Tree(tree) => self.write_tree_to_path(&tree, destination),
            Value::ExecPath(_) => {
                Err(self.type_error("cannot materialize ExecPath into exec sandbox"))
            }
            other => Err(self.type_error(&format!(
                "cannot materialize {} into exec sandbox",
                other.type_name()
            ))),
        }
    }

    fn write_blob_to_path(
        &self,
        blob: &BlobValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        if let Some(parent) = Path::new(destination.as_str()).parent() {
            fs::create_dir_all(parent).map_err(|error| {
                self.internal_error(&format!("failed to create {}: {error}", parent.display()))
            })?;
        }

        match blob {
            BlobValue::Inline(bytes) => {
                fs::write(destination.as_str(), bytes.as_ref()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to write {}: {error}",
                        destination.as_str()
                    ))
                })
            }
            BlobValue::FsFile(path) => {
                fs::copy(path.as_str(), destination.as_str()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to copy {} to {}: {error}",
                        path.as_str(),
                        destination.as_str()
                    ))
                })?;
                Ok(())
            }
            BlobValue::Store(blob_ref) => Err(self.internal_error(&format!(
                "cannot materialize store-backed blob {:?} without an external store resolver",
                blob_ref
            ))),
        }
    }

    fn write_tree_to_path(
        &self,
        tree: &TreeValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        match tree {
            TreeValue::Inline(entries) => {
                self.ensure_dir(destination)?;
                for (name, value) in entries.iter() {
                    self.materialize_value_to_fs(value, &destination.join(name.as_ref()))?;
                }
                Ok(())
            }
            TreeValue::FsDir(path) => self.copy_dir_recursive(path, destination),
            TreeValue::Store(root) => Err(self.internal_error(&format!(
                "cannot materialize store-backed tree {root} without an external store resolver"
            ))),
        }
    }

    fn copy_dir_recursive(
        &self,
        source: &PathValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        self.ensure_dir(destination)?;
        let mut entries = fs::read_dir(source.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", source.as_str()))
        })?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next().transpose().map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", source.as_str()))
        })? {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();

        for name in names {
            let source_path = source.join(&name);
            let destination_path = destination.join(&name);
            let metadata = fs::metadata(source_path.as_str()).map_err(|error| {
                self.internal_error(&format!("failed to stat {}: {error}", source_path.as_str()))
            })?;
            if metadata.is_dir() {
                self.copy_dir_recursive(&source_path, &destination_path)?;
            } else {
                self.write_blob_to_path(&BlobValue::FsFile(source_path), &destination_path)?;
            }
        }
        Ok(())
    }

    fn ensure_dir(&self, path: &PathValue) -> Result<(), PoisonError> {
        fs::create_dir_all(path.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to create {}: {error}", path.as_str()))
        })
    }

    fn expect_host_path(&self, value: &Value, context: &str) -> Result<String, PoisonError> {
        match value {
            Value::String(path) => Ok(path.to_string()),
            other => Err(self.type_error(&format!(
                "{context} must be String, got {}",
                other.type_name()
            ))),
        }
    }

    fn host_read(&self, path: &str) -> Result<Value, PoisonError> {
        let metadata = fs::metadata(path).map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to read host path {path}: {error}"),
                self.current_demand_chain(None),
            )
        })?;
        if metadata.is_dir() {
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("host.read expects a file, got directory {path}"),
                self.current_demand_chain(None),
            ));
        }
        Ok(Value::Blob(BlobValue::FsFile(PathValue::new(path))))
    }

    fn host_hash(&self, path: &str) -> Result<Value, PoisonError> {
        let bytes = fs::read(path).map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to hash host path {path}: {error}"),
                self.current_demand_chain(None),
            )
        })?;
        Ok(Value::Blob(BlobValue::from_string(
            blake3::hash(&bytes).to_hex().to_string(),
        )))
    }

    fn host_which(&self, name: &str) -> Value {
        if host_lookup_candidate(name).is_some_and(host_path_is_executable) {
            return option_some(Value::String(Arc::from(name)));
        }

        let Some(path) = env::var_os("PATH") else {
            return option_none();
        };
        for dir in env::split_paths(&path) {
            let candidate = dir.join(name);
            if host_path_is_executable(&candidate) {
                return option_some(Value::String(Arc::from(candidate.to_string_lossy())));
            }
        }
        option_none()
    }

    fn exec_process_value(&self, output: &std::process::Output) -> Value {
        let exit_code = output.status.code().unwrap_or(1).max(0) as u64;
        Value::Struct(StructValue {
            type_name: Arc::from("Process"),
            fields: Arc::from([
                (Arc::from("exit_code"), Value::U64(exit_code)),
                (
                    Arc::from("stdout"),
                    Value::Blob(BlobValue::Inline(Arc::from(output.stdout.clone()))),
                ),
                (
                    Arc::from("stderr"),
                    Value::Blob(BlobValue::Inline(Arc::from(output.stderr.clone()))),
                ),
            ]),
        })
    }

    fn snapshot_value(&self, value: Value) -> Result<Value, PoisonError> {
        let value = self.force_value_shallow(value)?;
        match value {
            Value::Pending(node_id) => {
                self.snapshot_value(self.force_value(Value::Pending(node_id))?)
            }
            Value::Host(_) => {
                Err(self
                    .internal_error("Host values cannot cross the local exec observer boundary"))
            }
            Value::ExecPath(_) => Err(self
                .internal_error("ExecPath values cannot cross the local exec observer boundary")),
            Value::Blob(BlobValue::Inline(_))
            | Value::Blob(BlobValue::Store(_))
            | Value::Path(_)
            | Value::U64(_)
            | Value::I64(_)
            | Value::F64(_)
            | Value::Bool(_)
            | Value::String(_)
            | Value::Unit
            | Value::Function(_)
            | Value::Builtin(_) => Ok(value),
            Value::Blob(BlobValue::FsFile(path)) => {
                let bytes = fs::read(path.as_str()).map_err(|error| {
                    self.internal_error(&format!("failed to read {}: {error}", path.as_str()))
                })?;
                Ok(Value::Blob(BlobValue::Inline(Arc::from(bytes))))
            }
            Value::Tree(TreeValue::Inline(entries)) => {
                let mut snapshotted = Vec::with_capacity(entries.len());
                for (name, value) in entries.iter() {
                    snapshotted.push((name.clone(), self.snapshot_value(value.clone())?));
                }
                Ok(Value::Tree(TreeValue::Inline(Arc::from(snapshotted))))
            }
            Value::Tree(TreeValue::FsDir(path)) => self.snapshot_fs_tree(&path),
            Value::Tree(TreeValue::Store(root)) => Ok(Value::Tree(TreeValue::Store(root))),
            Value::Tuple(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_value(value.clone())?);
                }
                Ok(Value::Tuple(Arc::from(snapshotted)))
            }
            Value::Array(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_value(value.clone())?);
                }
                Ok(Value::Array(Arc::from(snapshotted)))
            }
            Value::Map(map) => {
                let mut snapshotted = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    snapshotted.push((
                        self.snapshot_value(key.clone())?,
                        self.snapshot_value(value.clone())?,
                    ));
                }
                Ok(Value::Map(MapValue {
                    entries: Arc::from(snapshotted),
                }))
            }
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields.iter() {
                    fields.push((name.clone(), self.snapshot_value(value.clone())?));
                }
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
        }
    }

    fn snapshot_fs_tree(&self, root: &PathValue) -> Result<Value, PoisonError> {
        let mut read_dir = fs::read_dir(root.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", root.as_str()))
        })?;
        let mut names = Vec::new();
        while let Some(entry) = read_dir.next().transpose().map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", root.as_str()))
        })? {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();

        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let child = root.join(&name);
            let metadata = fs::metadata(child.as_str()).map_err(|error| {
                self.internal_error(&format!("failed to stat {}: {error}", child.as_str()))
            })?;
            let value = if metadata.is_dir() {
                self.snapshot_fs_tree(&child)?
            } else {
                let bytes = fs::read(child.as_str()).map_err(|error| {
                    self.internal_error(&format!("failed to read {}: {error}", child.as_str()))
                })?;
                Value::Blob(BlobValue::Inline(Arc::from(bytes)))
            };
            entries.push((Arc::from(name), value));
        }

        Ok(Value::Tree(TreeValue::Inline(Arc::from(entries))))
    }

    // r[impl store.tree.file]
    fn tree_file(&self, tree: &TreeValue, path: PathValue) -> Result<Value, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => self.tree_file_inline(entries, &path),
            TreeValue::FsDir(root) => {
                let full_path = root.join(path.as_str());
                let metadata = fs::metadata(full_path.as_str()).map_err(|error| {
                    PoisonError::new(
                        PoisonKind::UserFail,
                        format!("missing file {}: {error}", full_path.as_str()),
                        self.current_demand_chain(None),
                    )
                })?;
                if metadata.is_dir() {
                    return Err(PoisonError::new(
                        PoisonKind::UserFail,
                        format!("{} is a directory", full_path.as_str()),
                        self.current_demand_chain(None),
                    ));
                }
                Ok(Value::Blob(BlobValue::FsFile(full_path)))
            }
            TreeValue::Store(root) => Err(self.internal_error(&format!(
                "cannot read store-backed tree {root} without an external store resolver"
            ))),
        }
    }

    fn tree_file_inline(
        &self,
        entries: &[(Arc<str>, Value)],
        path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segments = path
            .as_str()
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect::<Vec<_>>();
        if segments.is_empty() {
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                "tree.file path must not be empty",
                self.current_demand_chain(None),
            ));
        }
        self.tree_file_inline_segments(entries, &segments, path)
    }

    fn tree_file_inline_segments(
        &self,
        entries: &[(Arc<str>, Value)],
        segments: &[&str],
        original_path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segment = segments[0];
        let (_, value) = entries
            .iter()
            .find(|(name, _)| name.as_ref() == segment)
            .ok_or_else(|| {
                PoisonError::new(
                    PoisonKind::UserFail,
                    format!("missing file {}", original_path.as_str()),
                    self.current_demand_chain(None),
                )
            })?;
        let value = self.force_value(value.clone())?;

        if segments.len() == 1 {
            return match value {
                Value::Blob(blob) => Ok(Value::Blob(blob)),
                Value::Tree(_) => Err(PoisonError::new(
                    PoisonKind::UserFail,
                    format!("{} points to a directory", original_path.as_str()),
                    self.current_demand_chain(None),
                )),
                other => Err(self.type_error(&format!(
                    "tree entry must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            };
        }

        match value {
            Value::Tree(TreeValue::Inline(entries)) => {
                self.tree_file_inline_segments(entries.as_ref(), &segments[1..], original_path)
            }
            Value::Tree(TreeValue::FsDir(root)) => self.tree_file(
                &TreeValue::FsDir(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Tree(TreeValue::Store(root)) => self.tree_file(
                &TreeValue::Store(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Blob(_) => Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("{} points inside a file", original_path.as_str()),
                self.current_demand_chain(None),
            )),
            other => Err(self.type_error(&format!(
                "tree entry must be Blob or Tree, got {}",
                other.type_name()
            ))),
        }
    }

    fn validate_tree_entries(&self, values: &[Value]) -> Result<(), PoisonError> {
        for value in values {
            match value {
                Value::Blob(_) | Value::Tree(_) | Value::Pending(_) => {}
                other => {
                    return Err(self.type_error(&format!(
                        "tree entries must be Blob or Tree, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        Ok(())
    }

    fn check_arity(&self, name: &str, args: &[Value], expected: usize) -> Result<(), PoisonError> {
        if args.len() != expected {
            return Err(self.type_error(&format!(
                "{name} expects {expected} argument(s), got {}",
                args.len()
            )));
        }
        Ok(())
    }

    fn require_no_kwargs(
        &self,
        name: &str,
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<(), PoisonError> {
        if kwargs.is_empty() {
            return Ok(());
        }
        let names = kwargs
            .iter()
            .map(|(name, _)| name.as_ref())
            .collect::<Vec<_>>()
            .join(", ");
        Err(self.type_error(&format!(
            "{name} does not accept keyword arguments ({names})"
        )))
    }

    fn stringify_scalar(&self, value: &Value) -> Result<String, PoisonError> {
        match value {
            Value::String(text) => Ok(text.to_string()),
            Value::Path(path) => Ok(path.as_str().to_string()),
            Value::Bool(_) | Value::U64(_) | Value::I64(_) | Value::F64(_) | Value::Unit => {
                Ok(value.to_string())
            }
            other => {
                Err(self.type_error(&format!("cannot turn {} into Blob text", other.type_name())))
            }
        }
    }

    fn expect_tree_entry_name<'a>(&self, value: &'a Value) -> Result<&'a str, PoisonError> {
        match value {
            Value::String(name) => Ok(name.as_ref()),
            Value::Path(path) => {
                let name = path.as_str();
                if name == "." || name == "/" || name.contains('/') {
                    Err(self.type_error("Tree.of entry names must be a single path segment"))
                } else {
                    Ok(name)
                }
            }
            other => Err(self.type_error(&format!(
                "Tree.of keys must be String or Path, got {}",
                other.type_name()
            ))),
        }
    }

    fn expect_relative_path(&self, value: &Value, context: &str) -> Result<PathValue, PoisonError> {
        let path = self.expect_path_value(value, context)?;
        if path.as_str().starts_with('/') {
            return Err(self.type_error(&format!("{context} must be relative")));
        }
        Ok(path)
    }

    fn expect_path_value(&self, value: &Value, context: &str) -> Result<PathValue, PoisonError> {
        match value {
            Value::Path(path) => Ok(path.clone()),
            Value::String(path) => Ok(PathValue::new(path.as_ref())),
            other => Err(self.type_error(&format!(
                "{context} must be Path or String, got {}",
                other.type_name()
            ))),
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
        PoisonError::new(
            PoisonKind::UnresolvedName,
            format!("unresolved name: {name}"),
            self.current_demand_chain(None),
        )
    }

    fn unresolved_function(&self, name: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::UnresolvedName,
            format!("no function named '{name}'"),
            self.current_demand_chain(None),
        )
    }

    fn type_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::TypeError,
            message,
            self.current_demand_chain(None),
        )
    }

    fn arithmetic_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::ArithmeticError,
            message,
            self.current_demand_chain(None),
        )
    }

    fn internal_error(&self, message: &str) -> PoisonError {
        PoisonError::new(
            PoisonKind::Internal,
            message,
            self.current_demand_chain(None),
        )
    }

    fn user_fail(&self, message: Value) -> Result<PoisonError, PoisonError> {
        let message = self.force_value(message)?;
        Ok(PoisonError::new(
            PoisonKind::UserFail,
            message.to_string(),
            self.current_demand_chain(None),
        ))
    }

    fn aggregate_poison_errors(&self, context: &str, errors: Vec<PoisonError>) -> PoisonError {
        PoisonError::aggregate(context, self.current_demand_chain(None), errors)
    }

    fn current_demand_chain(&self, next: Option<NodeId>) -> DemandChain {
        let mut entries = Vec::new();
        DEMAND_STACK.with(|stack| {
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

    fn force_pending_shallow(&self, node_id: NodeId) -> Result<Value, PoisonError> {
        self.with_demand(node_id, || self.force_node(node_id))
    }

    // r[impl eval.demand-depth]
    fn with_demand<T>(
        &self,
        node_id: NodeId,
        f: impl FnOnce() -> Result<T, PoisonError>,
    ) -> Result<T, PoisonError> {
        if DEMAND_STACK.with(|stack| stack.borrow().contains(&node_id)) {
            return Err(PoisonError::new(
                PoisonKind::CircularDependency,
                format!("circular dependency while demanding node {}", node_id.0),
                self.current_demand_chain(Some(node_id)),
            ));
        }

        let next_depth = DEMAND_STACK.with(|stack| stack.borrow().len() + 1);
        if next_depth > self.max_demand_depth {
            return Err(PoisonError::new(
                PoisonKind::DemandDepthExceeded,
                format!(
                    "demand chain exceeded configured depth limit of {}",
                    self.max_demand_depth
                ),
                self.current_demand_chain(Some(node_id)),
            ));
        }

        DEMAND_STACK.with(|stack| stack.borrow_mut().push(node_id));
        let result = f();
        DEMAND_STACK.with(|stack| {
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

fn builtin_namespace(name: &str) -> Option<BuiltinValue> {
    match name {
        "exec" => Some(BuiltinValue::Function(BuiltinFunction::Exec)),
        "Blob" => Some(BuiltinValue::Namespace(BuiltinNamespace::Blob)),
        "Tree" => Some(BuiltinValue::Namespace(BuiltinNamespace::Tree)),
        _ => None,
    }
}

fn fn_ptr(def: &Arc<instr::FnDef>) -> usize {
    Arc::as_ptr(def) as usize
}

fn stable_fn_repr(def: &instr::FnDef) -> String {
    format!("{def:?}")
}

fn option_some(value: Value) -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("Some"),
        fields: Arc::from([(Arc::from("value"), value)]),
    })
}

fn option_none() -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("None"),
        fields: Arc::from([]),
    })
}

fn host_lookup_candidate(name: &str) -> Option<&Path> {
    let candidate = Path::new(name);
    if candidate.is_absolute() || name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        Some(candidate)
    } else {
        None
    }
}

fn host_path_is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

// r[impl fn.identity]
// r[impl fn.identity.overloads]
// r[impl fn.identity.scc]
// r[impl fn.identity.stability]
// r[impl eval.memoization]
fn compute_function_identities(functions: &[Arc<instr::FnDef>]) -> HashMap<usize, Arc<str>> {
    let top_level_names = functions
        .iter()
        .map(|function| function.name.clone())
        .collect::<HashSet<_>>();
    let overloads = functions.iter().enumerate().fold(
        BTreeMap::<String, Vec<usize>>::new(),
        |mut overloads, (index, function)| {
            overloads
                .entry(function.name.clone())
                .or_default()
                .push(index);
            overloads
        },
    );

    let edges = functions
        .iter()
        .map(|function| {
            collect_global_callee_names(function, &top_level_names)
                .into_iter()
                .flat_map(|name| overloads.get(&name).into_iter().flatten().copied())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    let (component_of, components) = strongly_connected_components(&edges);
    let mut component_identities = vec![None; components.len()];

    for component_index in 0..components.len() {
        component_identity(
            component_index,
            &components,
            &component_of,
            &edges,
            functions,
            &mut component_identities,
        );
    }

    functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            (
                fn_ptr(function),
                component_identities[component_of[index]]
                    .clone()
                    .expect("component identity should be computed"),
            )
        })
        .collect()
}

fn component_identity(
    component_index: usize,
    components: &[Vec<usize>],
    component_of: &[usize],
    edges: &[BTreeSet<usize>],
    functions: &[Arc<instr::FnDef>],
    component_identities: &mut [Option<Arc<str>>],
) -> Arc<str> {
    if let Some(identity) = &component_identities[component_index] {
        return identity.clone();
    }

    let mut members = components[component_index]
        .iter()
        .map(|&index| stable_fn_repr(&functions[index]))
        .collect::<Vec<_>>();
    members.sort();

    let mut callees = components[component_index]
        .iter()
        .flat_map(|&index| edges[index].iter().copied())
        .map(|callee| component_of[callee])
        .filter(|&callee_component| callee_component != component_index)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|callee_component| {
            component_identity(
                callee_component,
                components,
                component_of,
                edges,
                functions,
                component_identities,
            )
            .to_string()
        })
        .collect::<Vec<_>>();
    callees.sort();

    let identity: Arc<str> = Arc::from(format!(
        "component:[{}]->[{}]",
        members.join("|"),
        callees.join("|")
    ));
    component_identities[component_index] = Some(identity.clone());
    identity
}

fn strongly_connected_components(edges: &[BTreeSet<usize>]) -> (Vec<usize>, Vec<Vec<usize>>) {
    struct Tarjan<'a> {
        edges: &'a [BTreeSet<usize>],
        index: usize,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        indices: Vec<Option<usize>>,
        lowlinks: Vec<usize>,
        component_of: Vec<usize>,
        components: Vec<Vec<usize>>,
    }

    impl<'a> Tarjan<'a> {
        fn visit(&mut self, node: usize) {
            let node_index = self.index;
            self.indices[node] = Some(node_index);
            self.lowlinks[node] = node_index;
            self.index += 1;
            self.stack.push(node);
            self.on_stack[node] = true;

            for &next in &self.edges[node] {
                if self.indices[next].is_none() {
                    self.visit(next);
                    self.lowlinks[node] = self.lowlinks[node].min(self.lowlinks[next]);
                } else if self.on_stack[next] {
                    self.lowlinks[node] =
                        self.lowlinks[node].min(self.indices[next].expect("known index"));
                }
            }

            if self.lowlinks[node] == node_index {
                let component_index = self.components.len();
                let mut component = Vec::new();
                loop {
                    let member = self.stack.pop().expect("stack should not be empty");
                    self.on_stack[member] = false;
                    self.component_of[member] = component_index;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                self.components.push(component);
            }
        }
    }

    let mut tarjan = Tarjan {
        edges,
        index: 0,
        stack: Vec::new(),
        on_stack: vec![false; edges.len()],
        indices: vec![None; edges.len()],
        lowlinks: vec![0; edges.len()],
        component_of: vec![0; edges.len()],
        components: Vec::new(),
    };

    for node in 0..edges.len() {
        if tarjan.indices[node].is_none() {
            tarjan.visit(node);
        }
    }

    (tarjan.component_of, tarjan.components)
}

fn collect_global_callee_names(
    function: &instr::FnDef,
    top_level_names: &HashSet<String>,
) -> BTreeSet<String> {
    let mut visible_locals = function
        .params
        .iter()
        .map(|param| param.name.clone())
        .collect::<HashSet<_>>();
    let mut deps = BTreeSet::new();
    collect_block_callee_names(
        &function.body,
        &mut visible_locals,
        top_level_names,
        &mut deps,
    );
    deps
}

fn collect_block_callee_names(
    block: &instr::Block,
    visible_locals: &mut HashSet<String>,
    top_level_names: &HashSet<String>,
    deps: &mut BTreeSet<String>,
) {
    let mut local_scope = visible_locals.clone();

    for stmt in &block.stmts {
        match stmt {
            instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                collect_expr_callee_names(value, &local_scope, top_level_names, deps);
                local_scope.insert(name.clone());
            }
            instr::Stmt::Expr(expr) | instr::Stmt::Return(expr) => {
                collect_expr_callee_names(expr, &local_scope, top_level_names, deps);
            }
            instr::Stmt::FnDecl(def) => {
                local_scope.insert(def.name.clone());
                let mut fn_scope = local_scope.clone();
                for param in &def.params {
                    fn_scope.insert(param.name.clone());
                }
                collect_block_callee_names(&def.body, &mut fn_scope, top_level_names, deps);
            }
        }
    }
}

fn collect_expr_callee_names(
    expr: &instr::Expr,
    visible_locals: &HashSet<String>,
    top_level_names: &HashSet<String>,
    deps: &mut BTreeSet<String>,
) {
    match expr {
        instr::Expr::LitInt(_)
        | instr::Expr::LitFloat(_)
        | instr::Expr::LitBool(_)
        | instr::Expr::LitString(_)
        | instr::Expr::Unit
        | instr::Expr::Var(_) => {}
        instr::Expr::StringTemplate(parts) | instr::Expr::PathTemplate(parts) => {
            for part in parts {
                if let instr::TemplatePart::Expr(expr) = part {
                    collect_expr_callee_names(expr, visible_locals, top_level_names, deps);
                }
            }
        }
        instr::Expr::Binary { left, right, .. } => {
            collect_expr_callee_names(left, visible_locals, top_level_names, deps);
            collect_expr_callee_names(right, visible_locals, top_level_names, deps);
        }
        instr::Expr::Unary { operand, .. } | instr::Expr::Fail(operand) => {
            collect_expr_callee_names(operand, visible_locals, top_level_names, deps);
        }
        instr::Expr::Block(block) => {
            let mut nested_scope = visible_locals.clone();
            collect_block_callee_names(block, &mut nested_scope, top_level_names, deps);
        }
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_expr_callee_names(cond, visible_locals, top_level_names, deps);
            let mut then_scope = visible_locals.clone();
            collect_block_callee_names(then_branch, &mut then_scope, top_level_names, deps);
            let mut else_scope = visible_locals.clone();
            collect_block_callee_names(else_branch, &mut else_scope, top_level_names, deps);
        }
        instr::Expr::Closure { def, .. } => {
            let mut closure_scope = visible_locals.clone();
            for param in &def.params {
                closure_scope.insert(param.name.clone());
            }
            collect_block_callee_names(&def.body, &mut closure_scope, top_level_names, deps);
        }
        instr::Expr::Call { func, args, kwargs } => {
            if let instr::Expr::Var(name) = func.as_ref()
                && !visible_locals.contains(name)
                && top_level_names.contains(name)
            {
                deps.insert(name.clone());
            }
            collect_expr_callee_names(func, visible_locals, top_level_names, deps);
            for arg in args {
                collect_expr_callee_names(arg, visible_locals, top_level_names, deps);
            }
            for (_, arg) in kwargs {
                collect_expr_callee_names(arg, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::StructLit { fields, .. } => {
            for (_, value) in fields {
                collect_expr_callee_names(value, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::Array(items) => {
            for item in items {
                collect_expr_callee_names(item, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::Map(entries) => {
            for (key, value) in entries {
                collect_expr_callee_names(key, visible_locals, top_level_names, deps);
                collect_expr_callee_names(value, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::Field { receiver, .. } => {
            collect_expr_callee_names(receiver, visible_locals, top_level_names, deps);
        }
        instr::Expr::Tuple(elems) => {
            for elem in elems {
                collect_expr_callee_names(elem, visible_locals, top_level_names, deps);
            }
        }
    }
}
