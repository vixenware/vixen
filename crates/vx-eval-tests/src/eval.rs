//! Demand-driven evaluator for slice 0 programs.
//!
//! Hybrid approach: strict functions (pure scalar computation) are compiled
//! to machine code by Kajit. Lazy scaffolding (products, thunks, closures)
//! stays interpreted via AST walking.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kajit::CompiledFunction;
use kajit_hir::VixenTypedFunction;
use vx_analysis::{KernelKind, SingleSourceBridge, build_minimal_analysis};
use vx_jit::StrictFragmentValue;
use vx_parser::{
    AstNode, BinaryExpr, BindingPattern, BlockExpr, CallExpr, ClosureExpr, Expr, FnDecl, IfExpr,
    IndexExpr, Item, LetStmt, LiteralExpr, NameRefExpr, Parameter, Stmt, StructLiteralExpr,
    SyntaxKind, SyntaxToken, TupleExpr, parse_source_file,
};

use crate::{EvalResult, EvalTrace};

// ---------------------------------------------------------------------------
// Compiled function cache — strict functions compiled by Kajit
// ---------------------------------------------------------------------------

struct CompiledFn {
    vtf: VixenTypedFunction,
    compiled: CompiledFunction,
}

struct CompiledEnv {
    fns: HashMap<String, CompiledFn>,
}

fn build_compiled_env(source: &str) -> CompiledEnv {
    let bridge = SingleSourceBridge::from_source_text(source);
    let analysis = build_minimal_analysis(&bridge);
    let program = match vx_analysis::lower::lower_executable_program(&analysis, &bridge) {
        Ok(p) => p,
        Err(_) => {
            return CompiledEnv {
                fns: HashMap::new(),
            };
        }
    };

    let lowerer = vx_analysis::lower_strict::StrictLowerer::new();
    let mut fns = HashMap::new();

    for body in program.bodies.values() {
        if let KernelKind::Strict { vtf } = &body.kernel_kind {
            let name = bridge
                .bodies
                .iter()
                .find(|b| b.id == body.id)
                .map(|b| b.name.clone());
            if let Some(name) = name {
                if let Ok(compiled) = vx_jit::compile_strict_fragment(vtf, &lowerer.module) {
                    fns.insert(
                        name,
                        CompiledFn {
                            vtf: vtf.clone(),
                            compiled,
                        },
                    );
                }
            }
        }
    }
    CompiledEnv { fns }
}

// ---------------------------------------------------------------------------
// Value conversion helpers
// ---------------------------------------------------------------------------

fn value_to_sfv(val: &Value) -> Result<StrictFragmentValue, String> {
    match val {
        Value::U64(v) => Ok(StrictFragmentValue::U64(*v)),
        Value::Bool(v) => Ok(StrictFragmentValue::Bool(*v)),
        Value::String(v) => Ok(StrictFragmentValue::String(v.clone())),
        _ => Err("cannot pass non-scalar to compiled function".into()),
    }
}

fn sfv_to_value(sfv: StrictFragmentValue) -> Value {
    match sfv {
        StrictFragmentValue::U64(v) => Value::U64(v),
        StrictFragmentValue::Bool(v) => Value::Bool(v),
        StrictFragmentValue::String(v) => Value::String(v),
    }
}

// ---------------------------------------------------------------------------
// Runtime values and thunks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Value {
    U64(u64),
    Bool(bool),
    String(String),
    /// A lazy product — fields are shared thunks (positional).
    Product(Vec<SharedThunk>),
    /// A struct value — named fields are shared thunks (lazy).
    Struct {
        name: String,
        fields: Vec<(String, SharedThunk)>,
    },
    /// A closure with captured environment.
    Closure {
        params: Vec<String>,
        body: Expr,
        env: Env,
    },
}

#[derive(Debug)]
enum ThunkInner {
    Ready(Value),
    Suspended { expr: Expr, env: Env },
}

#[derive(Debug, Clone)]
struct SharedThunk(Rc<RefCell<ThunkInner>>);

impl SharedThunk {
    fn ready(val: Value) -> Self {
        SharedThunk(Rc::new(RefCell::new(ThunkInner::Ready(val))))
    }

    fn suspended(expr: Expr, env: Env) -> Self {
        SharedThunk(Rc::new(RefCell::new(ThunkInner::Suspended { expr, env })))
    }

    fn force(
        &self,
        functions: &HashMap<String, FnDecl>,
        compiled: &CompiledEnv,
        trace: &mut EvalTrace,
    ) -> Result<Value, String> {
        {
            let inner = self.0.borrow();
            if let ThunkInner::Ready(v) = &*inner {
                return Ok(v.clone());
            }
        }
        let val = {
            let inner = self.0.borrow();
            match &*inner {
                ThunkInner::Suspended { expr, env } => {
                    let mut env = env.clone();
                    eval_expr(expr, &mut env, functions, compiled, trace)
                }
                ThunkInner::Ready(v) => Ok(v.clone()),
            }
        }?;
        *self.0.borrow_mut() = ThunkInner::Ready(val.clone());
        Ok(val)
    }
}

type Env = HashMap<String, SharedThunk>;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn evaluate(source: &str, entry: &str) -> Result<(EvalResult, EvalTrace), String> {
    let compiled = build_compiled_env(source);

    let parsed = parse_source_file(source);
    let file = parsed.tree();

    let mut functions: HashMap<String, FnDecl> = HashMap::new();
    for item in file.items() {
        if let Item::FnDecl(fn_decl) = item {
            if let Some(name) = fn_decl.name() {
                functions.insert(name.text().to_string(), fn_decl);
            }
        }
    }

    let mut trace = EvalTrace::default();
    trace.record_enter(entry);

    // If the entry function itself is compiled, call it directly
    if let Some(cfn) = compiled.fns.get(entry) {
        let result = unsafe {
            vx_jit::execute_compiled_copying_strict_fragment_strings(&cfn.vtf, &cfn.compiled, &[])
        }
        .map_err(|e| format!("compiled call: {e}"))?;
        let value = sfv_to_value(result);
        let result = match value {
            Value::U64(v) => EvalResult::U64(v),
            Value::Bool(v) => EvalResult::Bool(v),
            Value::String(v) => EvalResult::String(v),
            other => return Err(format!("unsupported result type: {other:?}")),
        };
        return Ok((result, trace));
    }

    let fn_decl = functions
        .get(entry)
        .ok_or_else(|| format!("function `{entry}` not found"))?
        .clone();

    let env = Env::new();
    let value = eval_fn_body(&fn_decl, &env, &functions, &compiled, &mut trace)?;

    let result = match value {
        Value::U64(v) => EvalResult::U64(v),
        Value::Bool(v) => EvalResult::Bool(v),
        Value::String(v) => EvalResult::String(v),
        other => return Err(format!("unsupported result type: {other:?}")),
    };

    Ok((result, trace))
}

// ---------------------------------------------------------------------------
// AST evaluation (lazy path)
// ---------------------------------------------------------------------------

fn eval_fn_body(
    fn_decl: &FnDecl,
    args: &Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    if let Some(block) = fn_decl.body() {
        return eval_block(&block, args, functions, compiled, trace);
    }
    if let Some(body_expr) = fn_decl.body_expr() {
        let mut env = args.clone();
        return eval_expr(&body_expr, &mut env, functions, compiled, trace);
    }
    Err("function has no body".to_string())
}

fn eval_block(
    block: &BlockExpr,
    args: &Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let mut env = args.clone();
    let mut last_value = None;

    for stmt in block.statements() {
        match stmt {
            Stmt::Let(let_stmt) => {
                eval_let(&let_stmt, &mut env)?;
            }
            Stmt::Expr(expr) => {
                last_value = Some(eval_expr(&expr, &mut env, functions, compiled, trace)?);
            }
            // Struct declarations are type-level — no runtime action needed.
            Stmt::StructDecl(_) => {}
            _ => return Err(format!("unsupported statement: {:?}", stmt.syntax().kind())),
        }
    }

    for expr in block.exprs() {
        last_value = Some(eval_expr(&expr, &mut env, functions, compiled, trace)?);
    }

    last_value.ok_or_else(|| "empty block".to_string())
}

fn eval_let(let_stmt: &LetStmt, env: &mut Env) -> Result<(), String> {
    let name = let_stmt
        .name()
        .ok_or("let without name")?
        .text()
        .to_string();
    let expr = let_stmt.expr().ok_or("let without value")?;
    env.insert(name, SharedThunk::suspended(expr, env.clone()));
    Ok(())
}

fn eval_expr(
    expr: &Expr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    match expr {
        Expr::Literal(lit) => eval_literal(lit),
        Expr::NameRef(name_ref) => eval_name_ref(name_ref, env, functions, compiled, trace),
        Expr::Binary(binary) => eval_binary(binary, env, functions, compiled, trace),
        Expr::Paren(paren) => {
            let inner = paren.expr().ok_or("empty parens")?;
            eval_expr(&inner, env, functions, compiled, trace)
        }
        Expr::Call(call) => eval_call(call, env, functions, compiled, trace),
        Expr::If(if_expr) => eval_if(if_expr, env, functions, compiled, trace),
        Expr::Tuple(tuple) => eval_tuple(tuple, env),
        Expr::Index(index) => eval_index(index, env, functions, compiled, trace),
        Expr::Field(field) => eval_field(field, env, functions, compiled, trace),
        Expr::StructLiteral(sl) => eval_struct_literal(&sl, env),
        Expr::Block(block) => eval_block(block, env, functions, compiled, trace),
        Expr::Closure(closure) => eval_closure(closure, env),
        _ => Err(format!(
            "unsupported expression: {:?}",
            expr.syntax().kind()
        )),
    }
}

fn eval_literal(lit: &LiteralExpr) -> Result<Value, String> {
    let token = lit.token().ok_or("literal without token")?;
    match token.kind() {
        SyntaxKind::INT => {
            let text = token.text();
            let v: u64 = text
                .parse()
                .map_err(|e| format!("invalid integer `{text}`: {e}"))?;
            Ok(Value::U64(v))
        }
        SyntaxKind::STRING => {
            let text = token.text();
            let inner = &text[1..text.len() - 1];
            Ok(Value::String(inner.to_string()))
        }
        SyntaxKind::TRUE_KW => Ok(Value::Bool(true)),
        SyntaxKind::FALSE_KW => Ok(Value::Bool(false)),
        _ => Err(format!("unsupported literal kind: {:?}", token.kind())),
    }
}

fn eval_name_ref(
    name_ref: &NameRefExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let name = name_ref
        .token()
        .ok_or("name ref without token")?
        .text()
        .to_string();

    if let Some(thunk) = env.get(&name) {
        return thunk.force(functions, compiled, trace);
    }

    if functions.contains_key(&name) {
        let fn_decl = &functions[&name];
        let params: Vec<String> = fn_decl
            .parameters()
            .filter_map(|p: Parameter| p.name().map(|n: SyntaxToken| n.text().to_string()))
            .collect();
        if let Some(body) = fn_decl.body_expr() {
            return Ok(Value::Closure {
                params,
                body,
                env: Env::new(),
            });
        }
    }

    Err(format!("unresolved name: `{name}`"))
}

fn eval_binary(
    binary: &BinaryExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let op_token = binary
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .find(|token| {
            matches!(
                token.kind(),
                SyntaxKind::PLUS
                    | SyntaxKind::MINUS
                    | SyntaxKind::STAR
                    | SyntaxKind::SLASH
                    | SyntaxKind::EQ2
                    | SyntaxKind::BANG_EQ
            )
        })
        .ok_or_else(|| format!("no operator in binary: {}", binary.syntax().text()))?;

    let lhs = binary.lhs().ok_or("binary without lhs")?;
    let rhs = binary.rhs().ok_or("binary without rhs")?;

    let lhs_val = eval_expr(&lhs, env, functions, compiled, trace)?;
    let rhs_val = eval_expr(&rhs, env, functions, compiled, trace)?;

    match (op_token.kind(), &lhs_val, &rhs_val) {
        (SyntaxKind::PLUS, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_add(*b))),
        (SyntaxKind::MINUS, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_sub(*b))),
        (SyntaxKind::STAR, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_mul(*b))),
        (SyntaxKind::EQ2, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a == b)),
        (SyntaxKind::BANG_EQ, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a != b)),
        _ => Err(format!(
            "unsupported binary op: {:?} on {lhs_val:?} and {rhs_val:?}",
            op_token.kind()
        )),
    }
}

fn eval_call(
    call: &CallExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let callee = call.callee().ok_or("call without callee")?;
    let args = call.positional_args();

    if let Expr::NameRef(name_ref) = &callee {
        let name = name_ref
            .token()
            .ok_or("callee without token")?
            .text()
            .to_string();

        if name == "trap" {
            return Err("trap reached".to_string());
        }

        // Check if it's a local closure binding
        if let Some(thunk) = env.get(&name) {
            let closure_val = thunk.force(functions, compiled, trace)?;
            if let Value::Closure {
                params,
                body,
                env: closure_env,
            } = closure_val
            {
                let mut call_env: Env = closure_env;
                for (param, arg_expr) in params.iter().zip(args.iter()) {
                    let arg_val = eval_expr(arg_expr, env, functions, compiled, trace)?;
                    call_env.insert(param.to_string(), SharedThunk::ready(arg_val));
                }
                return eval_expr(&body, &mut call_env, functions, compiled, trace);
            }
        }

        // Compiled function call — Kajit fast path
        if let Some(cfn) = compiled.fns.get(&name) {
            trace.record_enter(&name);
            let mut abi_args = Vec::new();
            for arg_expr in args.iter() {
                let val = eval_expr(arg_expr, env, functions, compiled, trace)?;
                abi_args.push(value_to_sfv(&val)?);
            }
            let result = unsafe {
                vx_jit::execute_compiled_copying_strict_fragment_strings(
                    &cfn.vtf,
                    &cfn.compiled,
                    &abi_args,
                )
            }
            .map_err(|e| format!("compiled call: {e}"))?;
            return Ok(sfv_to_value(result));
        }

        // Interpreted function call — AST walk
        if let Some(fn_decl) = functions.get(&name) {
            trace.record_enter(&name);
            let fn_decl: FnDecl = fn_decl.clone();
            let param_names: Vec<String> = fn_decl
                .parameters()
                .filter_map(|p: Parameter| p.name().map(|n: SyntaxToken| n.text().to_string()))
                .collect();

            let mut call_env: Env = Env::new();
            for (param, arg_expr) in param_names.iter().zip(args.iter()) {
                let arg_val = eval_expr(arg_expr, env, functions, compiled, trace)?;
                call_env.insert(param.to_string(), SharedThunk::ready(arg_val));
            }

            return eval_fn_body(&fn_decl, &call_env, functions, compiled, trace);
        }

        return Err(format!("unknown function: `{name}`"));
    }

    // Closure call
    let callee_val = eval_expr(&callee, env, functions, compiled, trace)?;
    if let Value::Closure {
        params,
        body,
        env: closure_env,
    } = callee_val
    {
        let mut call_env: Env = closure_env;
        for (param, arg_expr) in params.iter().zip(args.iter()) {
            let arg_val = eval_expr(arg_expr, env, functions, compiled, trace)?;
            call_env.insert(param.to_string(), SharedThunk::ready(arg_val));
        }
        return eval_expr(&body, &mut call_env, functions, compiled, trace);
    }

    Err(format!("callee is not callable: {callee_val:?}"))
}

fn eval_if(
    if_expr: &IfExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let condition = if_expr.condition().ok_or("if without condition")?;
    let cond_val = eval_expr(&condition, env, functions, compiled, trace)?;

    let is_true = match cond_val {
        Value::Bool(b) => b,
        _ => return Err(format!("if condition is not bool: {cond_val:?}")),
    };

    if is_true {
        let then_branch = if_expr.then_branch().ok_or("if without then")?;
        eval_expr(&then_branch, env, functions, compiled, trace)
    } else {
        let else_branch = if_expr.else_branch().ok_or("if without else")?;
        eval_expr(&else_branch, env, functions, compiled, trace)
    }
}

fn eval_tuple(tuple: &TupleExpr, env: &Env) -> Result<Value, String> {
    let fields: Vec<SharedThunk> = tuple
        .items()
        .map(|expr| SharedThunk::suspended(expr, env.clone()))
        .collect();
    Ok(Value::Product(fields))
}

fn eval_index(
    index: &IndexExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let receiver = index.receiver().ok_or("index without receiver")?;
    let idx_expr = index.index().ok_or("index without index")?;

    let recv_val = eval_expr(&receiver, env, functions, compiled, trace)?;
    let idx_val = eval_expr(&idx_expr, env, functions, compiled, trace)?;

    let idx = match idx_val {
        Value::U64(v) => v as usize,
        _ => return Err(format!("index is not u64: {idx_val:?}")),
    };

    match recv_val {
        Value::Product(fields) => {
            if idx < fields.len() {
                fields[idx].force(functions, compiled, trace)
            } else {
                Err(format!("index {idx} out of bounds"))
            }
        }
        _ => Err(format!("index on non-product: {recv_val:?}")),
    }
}

fn eval_field(
    field: &vx_parser::FieldExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let receiver = field.receiver().ok_or("field without receiver")?;
    let field_token = field.field_name().ok_or("field without name")?;
    let field_text = field_token.text().to_string();

    // Try positional access first (_0, _1, ...)
    let idx_str = field_text.strip_prefix('_').unwrap_or(&field_text);
    if let Ok(idx) = idx_str.parse::<usize>() {
        let recv_val = eval_expr(&receiver, env, functions, compiled, trace)?;
        return match recv_val {
            Value::Product(fields) => {
                if idx < fields.len() {
                    fields[idx].force(functions, compiled, trace)
                } else {
                    Err(format!("field index {idx} out of bounds"))
                }
            }
            _ => Err(format!("field access on non-product: {recv_val:?}")),
        };
    }

    // Named field access — works on structs
    let recv_val = eval_expr(&receiver, env, functions, compiled, trace)?;
    match recv_val {
        Value::Struct { name, fields } => {
            for (fname, thunk) in &fields {
                if *fname == field_text {
                    return thunk.force(functions, compiled, trace);
                }
            }
            Err(format!(
                "struct `{name}` has no field `{field_text}` (fields: {})",
                fields
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        _ => Err(format!(
            "named field `{field_text}` on non-struct: {recv_val:?}"
        )),
    }
}

fn eval_struct_literal(sl: &StructLiteralExpr, env: &Env) -> Result<Value, String> {
    let name = sl
        .name()
        .ok_or("struct literal without name")?
        .text()
        .to_string();

    let fields: Vec<(String, SharedThunk)> = sl
        .fields()
        .map(|f| {
            let fname = f
                .name()
                .expect("struct literal field without name")
                .text()
                .to_string();
            let expr = f.expr().expect("struct literal field without expression");
            (fname, SharedThunk::suspended(expr, env.clone()))
        })
        .collect();

    Ok(Value::Struct { name, fields })
}

fn eval_closure(closure: &ClosureExpr, env: &Env) -> Result<Value, String> {
    let params: Vec<String> = closure
        .parameters()
        .filter_map(|p: BindingPattern| p.name().map(|n: SyntaxToken| n.text().to_string()))
        .collect();

    let body = closure.body().ok_or("closure without body")?;

    Ok(Value::Closure {
        params,
        body,
        env: env.clone(),
    })
}
