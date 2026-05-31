//! Kernel dispatch: deciding whether a scalar (sub-)expression can run as a kernel
//! and building the plan to do so. Carved out of `eval.rs` — this owns the
//! per-body demand graphs, the per-expression plan cache, the soundness gates, and
//! the kernel-running. `eval`'s `dispatch_scalar` is the thin hook that, at each
//! expression, asks for a `DispatchPlan`, forces the parameter bindings from its
//! environment, and runs it (see `run_kernel`).
//!
//! This module never depends on `eval` — it speaks VIR, the demand graph, and the
//! kernel IR. (`scalar_word`, which reads an `eval::Value`, stays in `eval`.)

use std::cell::RefCell;
use std::collections::HashMap;

use vx_ir::build::VirProgram;
use vx_ir::ids::{BindingId, BodyId, ExprId};
use vx_types::{TypeEnv, TypeId};

use crate::analyze::build_graph_indexed;
use crate::graph::{DemandGraph, NodeId};
use crate::kernel;
use crate::lower::lower_node;

/// A scalar (sub-)expression that lowers to a kernel, with what's needed to run it:
/// the kernel, the bindings feeding its parameter slots (in order — a slot reads
/// the forced value of `param_bindings[i]`), and whether to read the returned word
/// as `Bool` (vs `U64`).
#[derive(Clone)]
pub(crate) struct DispatchPlan {
    pub(crate) kernel: kernel::Kernel,
    pub(crate) param_bindings: Vec<BindingId>,
    pub(crate) returns_bool: bool,
}

/// One body's demand graph plus the indices that map an evaluated expression to its
/// node, and a parameter node back to the binding it came from.
struct BodyGraph {
    graph: DemandGraph,
    exprmap: HashMap<ExprId, NodeId>,
    bindmap: HashMap<NodeId, BindingId>,
}

/// The kernel-dispatch decider: per-body demand graphs and per-expression plans,
/// both cached. Owned by the evaluator; consulted once per (sub-)expression.
#[derive(Default)]
pub(crate) struct Dispatcher {
    /// Per-body demand graph + indices, built once on first dispatch in that body.
    body_graphs: RefCell<HashMap<BodyId, BodyGraph>>,
    /// Per-expression decision: `Some` = run as a kernel, `None` = tree-walk.
    plan_cache: RefCell<HashMap<ExprId, Option<DispatchPlan>>>,
}

impl Dispatcher {
    /// The dispatch plan for expression `e` in `body` (cached, decided once). The
    /// plan is cloned out so the cache borrow isn't held across the caller's
    /// argument forcing (which recurses into evaluation).
    pub(crate) fn plan(
        &self,
        program: &VirProgram,
        env: &TypeEnv,
        e: ExprId,
        body: BodyId,
    ) -> Option<DispatchPlan> {
        let mut cache = self.plan_cache.borrow_mut();
        cache
            .entry(e)
            .or_insert_with(|| self.build_plan(program, env, e, body))
            .clone()
    }

    /// Build this body's demand graph (with indices) on first use, caching it.
    fn ensure_body_graph(&self, program: &VirProgram, env: &TypeEnv, body: BodyId) {
        if !self.body_graphs.borrow().contains_key(&body) {
            let (graph, exprmap, bindmap) = build_graph_indexed(program, env, body);
            self.body_graphs.borrow_mut().insert(
                body,
                BodyGraph {
                    graph,
                    exprmap,
                    bindmap,
                },
            );
        }
    }

    /// Decide whether expression `e` (in `body`) can run as a kernel, lowering it
    /// and resolving its parameters to bindings if so. The soundness gates that make
    /// dispatch outcome-identical to the tree-walker:
    ///
    /// - **scalar result**: `e`'s type is concretely `U64`/`Bool`, so the returned word wraps back to the right `Value`.
    /// - **lowers**: the region anchored at `e` lowers (only scalar-safe word ops; non-scalar constants and ops like `++` are already `NotLowerable`).
    /// - **strict params**: every kernel parameter is read in the entry block — used unconditionally — so forcing its binding eagerly can't detonate a path the lazy walker would skip.
    ///
    /// The fourth — scalar arguments — is checked per evaluation by `eval`, where a
    /// non-scalar binding falls back to the tree-walker.
    fn build_plan(
        &self,
        program: &VirProgram,
        env: &TypeEnv,
        e: ExprId,
        body: BodyId,
    ) -> Option<DispatchPlan> {
        let ety = program.arena[e].ty?;
        let returns_bool = matches!(scalar_kind(env, ety)?, ScalarKind::Bool);
        self.ensure_body_graph(program, env, body);
        let graphs = self.body_graphs.borrow();
        let bg = graphs.get(&body)?;
        let node = *bg.exprmap.get(&e)?;
        let (kernel, param_nodes) = lower_node(&bg.graph, node).ok()?;
        if !all_params_read_in_entry(&kernel) {
            return None;
        }
        let param_bindings: Vec<BindingId> = param_nodes
            .iter()
            .map(|n| bg.bindmap.get(n).copied())
            .collect::<Option<_>>()?;
        Some(DispatchPlan {
            kernel,
            param_bindings,
            returns_bool,
        })
    }
}

enum ScalarKind {
    U64,
    Bool,
}

/// The scalar kind of a type, by its rendered name (`None` if not `U64`/`Bool`).
fn scalar_kind(env: &TypeEnv, ty: TypeId) -> Option<ScalarKind> {
    match format!("{}", env.get_ty(ty).display_with_env(env)).as_str() {
        "U64" => Some(ScalarKind::U64),
        "Bool" => Some(ScalarKind::Bool),
        _ => None,
    }
}

/// Whether every kernel parameter (slots `0..n_params`) is read in the entry block —
/// i.e. used unconditionally, before any branch (the laziness gate).
fn all_params_read_in_entry(k: &kernel::Kernel) -> bool {
    use kernel::{OpKind, Term};
    if k.n_params == 0 {
        return true;
    }
    let entry = &k.blocks[0];
    let mut read = std::collections::HashSet::new();
    for op in &entry.ops {
        match op.kind {
            OpKind::Const => {}
            OpKind::Copy | OpKind::Not => {
                read.insert(op.a);
            }
            OpKind::Fail => {}
            _ => {
                read.insert(op.a);
                read.insert(op.b);
            }
        }
    }
    match entry.term {
        Term::Return(s) => {
            read.insert(s);
        }
        Term::BranchIf { cond, .. } => {
            read.insert(cond);
        }
        Term::Jump(_) => {}
    }
    (0..k.n_params).all(|p| read.contains(&p))
}

/// Run a kernel, preferring the native JIT and falling back to the interpreter
/// (and on non-native targets, the interpreter is the only path).
pub(crate) fn run_kernel(k: &kernel::Kernel, args: &[u64]) -> Result<u64, kernel::Poison> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if let Some(r) = crate::jit::jit_run(k, args) {
        return r;
    }
    kernel::run(k, args)
}
