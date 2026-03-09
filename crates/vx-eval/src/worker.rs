//! The evaluation engine: worker pool + demand-driven evaluation.
//!
//! The engine manages a pool of worker threads and a memo table of nodes.
//! Evaluation is demand-driven: `demand(node_id)` checks the memo table,
//! and if the value isn't there, evaluates it. When a function body hits
//! a dependency that isn't ready, it parks (saves its position in a
//! SuspendedFrame) and resumes when the dependency resolves.

use std::collections::HashMap;
use std::sync::Arc;

use crossbeam_channel::Sender;
use dashmap::DashMap;

use crate::capability::CapabilitySet;
use crate::instr::{self, BinOp, UnaryOp};
use crate::node::{Node, NodeId, Outcome, Waiter};
use crate::poison::{DemandChain, PoisonError, PoisonKind};
use crate::value::{FunctionValue, StructValue, Value};

/// The evaluation engine.
pub struct Engine {
    /// All nodes in the dataflow graph, keyed by NodeId.
    nodes: DashMap<NodeId, Arc<Node>>,
    /// The function table: name → list of definitions (for overloading).
    functions: HashMap<String, Vec<Arc<instr::FnDef>>>,
}

/// A task submitted to the work queue.
struct Task {
    node: Arc<Node>,
    frame: SuspendedFrame,
}

/// The saved state of a paused function evaluation.
///
/// When evaluation hits a pending dependency, we save where we are
/// (which statement in the block, what bindings we've accumulated)
/// and resume from exactly that point when the dependency resolves.
#[derive(Clone)]
struct SuspendedFrame {
    /// The function being evaluated.
    func: Arc<instr::FnDef>,
    /// Arguments passed to this call.
    args: Vec<Value>,
    /// Local bindings accumulated so far (name → value).
    bindings: HashMap<String, Value>,
    /// Which statement index in the body we're about to execute.
    stmt_index: usize,
    /// Capabilities available to this evaluation.
    capabilities: CapabilitySet,
    /// The demand chain for debugging.
    demand_chain: DemandChain,
}

/// The result of trying to evaluate an expression: either we got a value,
/// or we need to wait for some nodes before we can continue.
enum EvalResult {
    /// Got a value.
    Ready(Value),
    /// A `return` escaped the current expression context.
    Returned(Value),
    /// Need to wait for these nodes. The SuspendedFrame should be saved.
    Blocked(Vec<NodeId>),
    /// Evaluation poisoned.
    Poisoned(PoisonError),
}

impl Engine {
    /// Create a new engine from a lowered module.
    pub fn from_module(module: &instr::Module) -> Self {
        let mut functions: HashMap<String, Vec<Arc<instr::FnDef>>> = HashMap::new();
        for func in &module.functions {
            functions
                .entry(func.name.clone())
                .or_default()
                .push(func.clone());
        }

        Self {
            nodes: DashMap::new(),
            functions,
        }
    }

    /// Evaluate a top-level function by name with the given arguments.
    /// This is the main entry point — it demands the result and blocks
    /// until it's available (this is a forcing boundary).
    pub fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, PoisonError> {
        let func = self.resolve_function(name, &args)?;

        let node = Arc::new(Node::new(format!("{}(..)", name)));
        let node_id = node.id;
        self.nodes.insert(node_id, node.clone());

        // Start evaluation directly on this thread
        let frame = SuspendedFrame {
            func,
            args,
            bindings: HashMap::new(),
            stmt_index: 0,
            capabilities: CapabilitySet::NONE,
            demand_chain: DemandChain::new(),
        };

        self.run_to_completion(node, frame)
    }

    /// Run a task to completion, handling park/wake cycles.
    /// This is the forcing boundary — it blocks the calling thread
    /// until the node resolves.
    fn run_to_completion(
        &self,
        node: Arc<Node>,
        mut frame: SuspendedFrame,
    ) -> Result<Value, PoisonError> {
        // Channel for receiving wake notifications
        let (wake_tx, wake_rx) = crossbeam_channel::unbounded();

        loop {
            match self.evaluate_frame(&mut frame, &wake_tx) {
                EvalResult::Ready(value) | EvalResult::Returned(value) => {
                    node.resolve(value.clone());
                    return Ok(value);
                }
                EvalResult::Poisoned(err) => {
                    node.poison(err.clone());
                    return Err(err);
                }
                EvalResult::Blocked(deps) => {
                    // Register as waiter on all pending deps
                    let mut pending_count = 0;
                    for &dep_id in &deps {
                        if let Some(dep_node) = self.nodes.get(&dep_id) {
                            match dep_node.outcome() {
                                Some(Outcome::Value(_)) => {
                                    // Already resolved, no need to wait
                                }
                                Some(Outcome::Poison(err)) => {
                                    // Dependency poisoned — propagate
                                    node.poison(err.clone());
                                    return Err(err);
                                }
                                None => {
                                    // Still evaluating — register as waiter
                                    let mut state = dep_node.state.lock();
                                    match &mut *state {
                                        crate::node::NodeState::Evaluating { waiters } => {
                                            waiters.push(Waiter {
                                                node_id: node.id,
                                                sender: wake_tx.clone(),
                                            });
                                            pending_count += 1;
                                        }
                                        crate::node::NodeState::Resolved(_) => {}
                                        crate::node::NodeState::Poisoned(err) => {
                                            let err = err.clone();
                                            drop(state);
                                            node.poison(err.clone());
                                            return Err(err);
                                        }
                                        crate::node::NodeState::Pending => {
                                            // This shouldn't happen — we should have
                                            // started evaluation on the dep
                                            pending_count += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if pending_count == 0 {
                        // All deps actually resolved while we were checking,
                        // just loop and re-evaluate
                        continue;
                    }

                    // Wait for at least one dependency to resolve
                    match wake_rx.recv() {
                        Ok((_resolved_id, Outcome::Poison(err))) => {
                            node.poison(err.clone());
                            return Err(err);
                        }
                        Ok((_resolved_id, Outcome::Value(_))) => {
                            // A dep resolved — loop and continue evaluation
                            // from where the frame left off
                            continue;
                        }
                        Err(_) => {
                            let err = PoisonError {
                                kind: PoisonKind::Internal,
                                message: "all dependency senders dropped".into(),
                                demand_chain: frame.demand_chain.clone(),
                            };
                            node.poison(err.clone());
                            return Err(err);
                        }
                    }
                }
            }
        }
    }

    /// Evaluate a suspended frame as far as possible.
    ///
    /// Returns Ready if the function completes, Blocked if it needs to
    /// wait for dependencies, or Poisoned if something went wrong.
    fn evaluate_frame(
        &self,
        frame: &mut SuspendedFrame,
        wake_tx: &Sender<(NodeId, Outcome)>,
    ) -> EvalResult {
        let stmts = &frame.func.body.stmts;

        while frame.stmt_index < stmts.len() {
            let is_last = frame.stmt_index == stmts.len() - 1;
            let stmt = &stmts[frame.stmt_index];

            match stmt {
                instr::Stmt::Let { name, value } => {
                    match self.eval_expr(value, &frame.bindings, wake_tx) {
                        EvalResult::Ready(val) => {
                            frame.bindings.insert(name.clone(), val);
                            frame.stmt_index += 1;
                        }
                        EvalResult::Returned(val) => return EvalResult::Ready(val),
                        other => return other,
                    }
                }
                instr::Stmt::Assign { name, value } => {
                    match self.eval_expr(value, &frame.bindings, wake_tx) {
                        EvalResult::Ready(val) => {
                            frame.bindings.insert(name.clone(), val);
                            frame.stmt_index += 1;
                        }
                        EvalResult::Returned(val) => return EvalResult::Ready(val),
                        other => return other,
                    }
                }
                instr::Stmt::Return(expr) => {
                    return match self.eval_expr(expr, &frame.bindings, wake_tx) {
                        EvalResult::Ready(val) | EvalResult::Returned(val) => {
                            EvalResult::Ready(val)
                        }
                        other => other,
                    };
                }
                instr::Stmt::FnDecl(def) => {
                    // Local function — add to bindings as a function value
                    let func_val = Value::Function(FunctionValue {
                        def: def.clone(),
                        captures: Arc::from([]),
                    });
                    frame.bindings.insert(def.name.clone(), func_val);
                    frame.stmt_index += 1;
                }
                instr::Stmt::Expr(expr) => {
                    match self.eval_expr(expr, &frame.bindings, wake_tx) {
                        EvalResult::Ready(val) => {
                            if is_last {
                                return EvalResult::Ready(val);
                            }
                            // Not last statement — discard the value
                            frame.stmt_index += 1;
                        }
                        EvalResult::Returned(val) => return EvalResult::Ready(val),
                        other => return other,
                    }
                }
            }
        }

        // Empty body or all statements were non-expression → Unit
        EvalResult::Ready(Value::Unit)
    }

    /// Evaluate an expression in the current binding environment.
    fn eval_expr(
        &self,
        expr: &instr::Expr,
        bindings: &HashMap<String, Value>,
        wake_tx: &Sender<(NodeId, Outcome)>,
    ) -> EvalResult {
        match expr {
            instr::Expr::LitInt(n) => EvalResult::Ready(Value::U64(*n)),
            instr::Expr::LitFloat(n) => EvalResult::Ready(Value::F64(*n)),
            instr::Expr::LitBool(b) => EvalResult::Ready(Value::Bool(*b)),
            instr::Expr::LitString(s) => EvalResult::Ready(Value::String(Arc::from(s.as_str()))),
            instr::Expr::Unit => EvalResult::Ready(Value::Unit),

            instr::Expr::Var(name) => {
                if let Some(val) = bindings.get(name) {
                    EvalResult::Ready(val.clone())
                } else if let Some(overloads) = self.functions.get(name) {
                    // Return the function as a value
                    EvalResult::Ready(Value::Function(FunctionValue {
                        def: overloads[0].clone(),
                        captures: Arc::from([]),
                    }))
                } else {
                    EvalResult::Poisoned(PoisonError {
                        kind: PoisonKind::UnresolvedName,
                        message: format!("unresolved name: {name}"),
                        demand_chain: DemandChain::new(),
                    })
                }
            }

            instr::Expr::Binary { op, left, right } => {
                let lhs = match self.eval_expr(left, bindings, wake_tx) {
                    EvalResult::Ready(v) => v,
                    EvalResult::Returned(v) => return EvalResult::Returned(v),
                    other => return other,
                };
                let rhs = match self.eval_expr(right, bindings, wake_tx) {
                    EvalResult::Ready(v) => v,
                    EvalResult::Returned(v) => return EvalResult::Returned(v),
                    other => return other,
                };
                eval_binary_op(*op, &lhs, &rhs)
            }

            instr::Expr::Unary { op, operand } => {
                let val = match self.eval_expr(operand, bindings, wake_tx) {
                    EvalResult::Ready(v) => v,
                    EvalResult::Returned(v) => return EvalResult::Returned(v),
                    other => return other,
                };
                eval_unary_op(*op, &val)
            }

            instr::Expr::Block(block) => {
                let mut local_bindings = bindings.clone();
                let stmts = &block.stmts;
                for (i, stmt) in stmts.iter().enumerate() {
                    let is_last = i == stmts.len() - 1;
                    match stmt {
                        instr::Stmt::Let { name, value } => {
                            match self.eval_expr(value, &local_bindings, wake_tx) {
                                EvalResult::Ready(val) => {
                                    local_bindings.insert(name.clone(), val);
                                }
                                EvalResult::Returned(val) => {
                                    return EvalResult::Returned(val);
                                }
                                other => return other,
                            }
                        }
                        instr::Stmt::Assign { name, value } => {
                            match self.eval_expr(value, &local_bindings, wake_tx) {
                                EvalResult::Ready(val) => {
                                    local_bindings.insert(name.clone(), val);
                                }
                                EvalResult::Returned(val) => {
                                    return EvalResult::Returned(val);
                                }
                                other => return other,
                            }
                        }
                        instr::Stmt::Return(expr) => {
                            return match self.eval_expr(expr, &local_bindings, wake_tx) {
                                EvalResult::Ready(val) | EvalResult::Returned(val) => {
                                    EvalResult::Returned(val)
                                }
                                other => other,
                            };
                        }
                        instr::Stmt::FnDecl(def) => {
                            let func_val = Value::Function(FunctionValue {
                                def: def.clone(),
                                captures: Arc::from([]),
                            });
                            local_bindings.insert(def.name.clone(), func_val);
                        }
                        instr::Stmt::Expr(expr) => {
                            match self.eval_expr(expr, &local_bindings, wake_tx) {
                                EvalResult::Ready(val) => {
                                    if is_last {
                                        return EvalResult::Ready(val);
                                    }
                                }
                                EvalResult::Returned(val) => {
                                    return EvalResult::Returned(val);
                                }
                                other => return other,
                            }
                        }
                    }
                }
                EvalResult::Ready(Value::Unit)
            }

            instr::Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_val = match self.eval_expr(cond, bindings, wake_tx) {
                    EvalResult::Ready(v) => v,
                    EvalResult::Returned(v) => return EvalResult::Returned(v),
                    other => return other,
                };
                match cond_val {
                    Value::Bool(true) => {
                        let block_expr = instr::Expr::Block(then_branch.clone());
                        self.eval_expr(&block_expr, bindings, wake_tx)
                    }
                    Value::Bool(false) => {
                        let block_expr = instr::Expr::Block(else_branch.clone());
                        self.eval_expr(&block_expr, bindings, wake_tx)
                    }
                    other => EvalResult::Poisoned(PoisonError {
                        kind: PoisonKind::TypeError,
                        message: format!("if condition must be Bool, got {}", other.type_name()),
                        demand_chain: DemandChain::new(),
                    }),
                }
            }

            instr::Expr::Call { func, args } => {
                // Evaluate the callee
                let func_val = match self.eval_expr(func, bindings, wake_tx) {
                    EvalResult::Ready(v) => v,
                    EvalResult::Returned(v) => return EvalResult::Returned(v),
                    other => return other,
                };

                // Evaluate arguments
                let mut arg_vals = Vec::with_capacity(args.len());
                for arg in args {
                    match self.eval_expr(arg, bindings, wake_tx) {
                        EvalResult::Ready(v) => arg_vals.push(v),
                        EvalResult::Returned(v) => return EvalResult::Returned(v),
                        other => return other,
                    }
                }

                match func_val {
                    Value::Function(fv) => self.call_function(&fv.def, arg_vals, wake_tx),
                    other => EvalResult::Poisoned(PoisonError {
                        kind: PoisonKind::TypeError,
                        message: format!("cannot call {}", other.type_name()),
                        demand_chain: DemandChain::new(),
                    }),
                }
            }

            instr::Expr::StructLit { name, fields } => {
                let mut field_vals = Vec::with_capacity(fields.len());
                for (field_name, field_expr) in fields {
                    match self.eval_expr(field_expr, bindings, wake_tx) {
                        EvalResult::Ready(val) => {
                            field_vals.push((Arc::from(field_name.as_str()), val));
                        }
                        EvalResult::Returned(val) => return EvalResult::Returned(val),
                        other => return other,
                    }
                }
                EvalResult::Ready(Value::Struct(StructValue {
                    type_name: Arc::from(name.as_str()),
                    fields: Arc::from(field_vals),
                }))
            }

            instr::Expr::Field { receiver, field } => {
                let recv_val = match self.eval_expr(receiver, bindings, wake_tx) {
                    EvalResult::Ready(v) => v,
                    EvalResult::Returned(v) => return EvalResult::Returned(v),
                    other => return other,
                };
                match &recv_val {
                    Value::Struct(s) => {
                        for (name, val) in s.fields.iter() {
                            if name.as_ref() == field {
                                return EvalResult::Ready(val.clone());
                            }
                        }
                        EvalResult::Poisoned(PoisonError {
                            kind: PoisonKind::TypeError,
                            message: format!("no field '{}' on struct {}", field, s.type_name),
                            demand_chain: DemandChain::new(),
                        })
                    }
                    Value::Tuple(elems) => {
                        // Tuple field access: _0, _1, etc.
                        if let Some(idx_str) = field.strip_prefix('_') {
                            if let Ok(idx) = idx_str.parse::<usize>() {
                                if idx < elems.len() {
                                    return EvalResult::Ready(elems[idx].clone());
                                }
                            }
                        }
                        EvalResult::Poisoned(PoisonError {
                            kind: PoisonKind::TypeError,
                            message: format!("invalid tuple field '{}'", field),
                            demand_chain: DemandChain::new(),
                        })
                    }
                    other => EvalResult::Poisoned(PoisonError {
                        kind: PoisonKind::TypeError,
                        message: format!(
                            "cannot access field '{}' on {}",
                            field,
                            other.type_name()
                        ),
                        demand_chain: DemandChain::new(),
                    }),
                }
            }

            instr::Expr::Tuple(elems) => {
                let mut vals = Vec::with_capacity(elems.len());
                for elem in elems {
                    match self.eval_expr(elem, bindings, wake_tx) {
                        EvalResult::Ready(v) => vals.push(v),
                        EvalResult::Returned(v) => return EvalResult::Returned(v),
                        other => return other,
                    }
                }
                EvalResult::Ready(Value::Tuple(Arc::from(vals)))
            }
        }
    }

    /// Call a function with resolved arguments, evaluating its body.
    fn call_function(
        &self,
        def: &Arc<instr::FnDef>,
        args: Vec<Value>,
        wake_tx: &Sender<(NodeId, Outcome)>,
    ) -> EvalResult {
        // Bind parameters
        let mut bindings = HashMap::new();
        for (param, arg) in def.params.iter().zip(args.iter()) {
            bindings.insert(param.name.clone(), arg.clone());
        }

        // Add all top-level functions to scope
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

        // Evaluate body
        let stmts = &def.body.stmts;
        for (i, stmt) in stmts.iter().enumerate() {
            let is_last = i == stmts.len() - 1;
            match stmt {
                instr::Stmt::Let { name, value } => {
                    match self.eval_expr(value, &bindings, wake_tx) {
                        EvalResult::Ready(val) => {
                            bindings.insert(name.clone(), val);
                        }
                        EvalResult::Returned(val) => return EvalResult::Ready(val),
                        other => return other,
                    }
                }
                instr::Stmt::Assign { name, value } => {
                    match self.eval_expr(value, &bindings, wake_tx) {
                        EvalResult::Ready(val) => {
                            bindings.insert(name.clone(), val);
                        }
                        EvalResult::Returned(val) => return EvalResult::Ready(val),
                        other => return other,
                    }
                }
                instr::Stmt::Return(expr) => {
                    return match self.eval_expr(expr, &bindings, wake_tx) {
                        EvalResult::Ready(val) | EvalResult::Returned(val) => {
                            EvalResult::Ready(val)
                        }
                        other => other,
                    };
                }
                instr::Stmt::FnDecl(local_def) => {
                    let func_val = Value::Function(FunctionValue {
                        def: local_def.clone(),
                        captures: Arc::from([]),
                    });
                    bindings.insert(local_def.name.clone(), func_val);
                }
                instr::Stmt::Expr(expr) => match self.eval_expr(expr, &bindings, wake_tx) {
                    EvalResult::Ready(val) => {
                        if is_last {
                            return EvalResult::Ready(val);
                        }
                    }
                    EvalResult::Returned(val) => return EvalResult::Ready(val),
                    other => return other,
                },
            }
        }
        EvalResult::Ready(Value::Unit)
    }

    /// Resolve a function by name, picking the right overload for the given args.
    /// For 0.1, just picks the first overload.
    fn resolve_function(
        &self,
        name: &str,
        _args: &[Value],
    ) -> Result<Arc<instr::FnDef>, PoisonError> {
        self.functions
            .get(name)
            .and_then(|overloads| overloads.first().cloned())
            .ok_or_else(|| PoisonError {
                kind: PoisonKind::UnresolvedName,
                message: format!("no function named '{name}'"),
                demand_chain: DemandChain::new(),
            })
    }
}

fn eval_binary_op(op: BinOp, lhs: &Value, rhs: &Value) -> EvalResult {
    match (op, lhs, rhs) {
        // U64 arithmetic
        (BinOp::Add, Value::U64(a), Value::U64(b)) => {
            EvalResult::Ready(Value::U64(a.wrapping_add(*b)))
        }
        (BinOp::Sub, Value::U64(a), Value::U64(b)) => {
            EvalResult::Ready(Value::U64(a.wrapping_sub(*b)))
        }
        (BinOp::Mul, Value::U64(a), Value::U64(b)) => {
            EvalResult::Ready(Value::U64(a.wrapping_mul(*b)))
        }
        (BinOp::Div, Value::U64(_), Value::U64(0)) => EvalResult::Poisoned(PoisonError {
            kind: PoisonKind::ArithmeticError,
            message: "division by zero".into(),
            demand_chain: DemandChain::new(),
        }),
        (BinOp::Div, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::U64(a / b)),
        (BinOp::Mod, Value::U64(_), Value::U64(0)) => EvalResult::Poisoned(PoisonError {
            kind: PoisonKind::ArithmeticError,
            message: "modulo by zero".into(),
            demand_chain: DemandChain::new(),
        }),
        (BinOp::Mod, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::U64(a % b)),

        // I64 arithmetic
        (BinOp::Add, Value::I64(a), Value::I64(b)) => {
            EvalResult::Ready(Value::I64(a.wrapping_add(*b)))
        }
        (BinOp::Sub, Value::I64(a), Value::I64(b)) => {
            EvalResult::Ready(Value::I64(a.wrapping_sub(*b)))
        }
        (BinOp::Mul, Value::I64(a), Value::I64(b)) => {
            EvalResult::Ready(Value::I64(a.wrapping_mul(*b)))
        }
        (BinOp::Div, Value::I64(_), Value::I64(0)) => EvalResult::Poisoned(PoisonError {
            kind: PoisonKind::ArithmeticError,
            message: "division by zero".into(),
            demand_chain: DemandChain::new(),
        }),
        (BinOp::Div, Value::I64(a), Value::I64(b)) => EvalResult::Ready(Value::I64(a / b)),

        // F64 arithmetic
        (BinOp::Add, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::F64(a + b)),
        (BinOp::Sub, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::F64(a - b)),
        (BinOp::Mul, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::F64(a * b)),
        (BinOp::Div, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::F64(a / b)),

        // U64 comparisons
        (BinOp::Eq, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::Bool(a == b)),
        (BinOp::Ne, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::Bool(a != b)),
        (BinOp::Lt, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::Bool(a < b)),
        (BinOp::Le, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::Bool(a <= b)),
        (BinOp::Gt, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::Bool(a > b)),
        (BinOp::Ge, Value::U64(a), Value::U64(b)) => EvalResult::Ready(Value::Bool(a >= b)),

        // I64 comparisons
        (BinOp::Eq, Value::I64(a), Value::I64(b)) => EvalResult::Ready(Value::Bool(a == b)),
        (BinOp::Ne, Value::I64(a), Value::I64(b)) => EvalResult::Ready(Value::Bool(a != b)),
        (BinOp::Lt, Value::I64(a), Value::I64(b)) => EvalResult::Ready(Value::Bool(a < b)),
        (BinOp::Le, Value::I64(a), Value::I64(b)) => EvalResult::Ready(Value::Bool(a <= b)),
        (BinOp::Gt, Value::I64(a), Value::I64(b)) => EvalResult::Ready(Value::Bool(a > b)),
        (BinOp::Ge, Value::I64(a), Value::I64(b)) => EvalResult::Ready(Value::Bool(a >= b)),

        // F64 comparisons
        (BinOp::Eq, Value::F64(a), Value::F64(b)) => {
            EvalResult::Ready(Value::Bool(a.to_bits() == b.to_bits()))
        }
        (BinOp::Ne, Value::F64(a), Value::F64(b)) => {
            EvalResult::Ready(Value::Bool(a.to_bits() != b.to_bits()))
        }
        (BinOp::Lt, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::Bool(a < b)),
        (BinOp::Le, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::Bool(a <= b)),
        (BinOp::Gt, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::Bool(a > b)),
        (BinOp::Ge, Value::F64(a), Value::F64(b)) => EvalResult::Ready(Value::Bool(a >= b)),

        // Bool comparisons
        (BinOp::Eq, Value::Bool(a), Value::Bool(b)) => EvalResult::Ready(Value::Bool(a == b)),
        (BinOp::Ne, Value::Bool(a), Value::Bool(b)) => EvalResult::Ready(Value::Bool(a != b)),

        // String comparisons
        (BinOp::Eq, Value::String(a), Value::String(b)) => EvalResult::Ready(Value::Bool(a == b)),
        (BinOp::Ne, Value::String(a), Value::String(b)) => EvalResult::Ready(Value::Bool(a != b)),

        // Logical
        (BinOp::And, Value::Bool(a), Value::Bool(b)) => EvalResult::Ready(Value::Bool(*a && *b)),
        (BinOp::Or, Value::Bool(a), Value::Bool(b)) => EvalResult::Ready(Value::Bool(*a || *b)),

        // String concatenation via +
        (BinOp::Add, Value::String(a), Value::String(b)) => {
            let mut s = String::with_capacity(a.len() + b.len());
            s.push_str(a);
            s.push_str(b);
            EvalResult::Ready(Value::String(Arc::from(s)))
        }

        _ => EvalResult::Poisoned(PoisonError {
            kind: PoisonKind::TypeError,
            message: format!(
                "cannot apply {:?} to {} and {}",
                op,
                lhs.type_name(),
                rhs.type_name()
            ),
            demand_chain: DemandChain::new(),
        }),
    }
}

fn eval_unary_op(op: UnaryOp, val: &Value) -> EvalResult {
    match (op, val) {
        (UnaryOp::Neg, Value::I64(n)) => EvalResult::Ready(Value::I64(-n)),
        (UnaryOp::Neg, Value::F64(n)) => EvalResult::Ready(Value::F64(-n)),
        (UnaryOp::Not, Value::Bool(b)) => EvalResult::Ready(Value::Bool(!b)),
        _ => EvalResult::Poisoned(PoisonError {
            kind: PoisonKind::TypeError,
            message: format!("cannot apply {:?} to {}", op, val.type_name()),
            demand_chain: DemandChain::new(),
        }),
    }
}
