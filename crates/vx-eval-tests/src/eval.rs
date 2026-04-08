//! Demand-driven evaluator for slice 0 + slice 1 programs.
//!
//! Hybrid approach: strict functions (pure scalar computation) are compiled
//! to machine code by Kajit. Lazy scaffolding (products, thunks, closures,
//! enums, match) stays interpreted via AST walking.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kajit::CompiledFunction;
use kajit_hir::VixenTypedFunction;
use vx_jit::StrictFragmentValue;
use vx_jit::legacy::{KernelKind, SingleSourceBridge, build_minimal_analysis};
use vx_parser::{
    AstNode, BinaryExpr, BindingPattern, BlockExpr, CallExpr, ClosureExpr, EnumDecl, Expr, FnDecl,
    IfExpr, IndexExpr, Item, LetStmt, LiteralExpr, MatchExpr, NameRefExpr, Parameter, Pattern,
    Stmt, StructLiteralExpr, SyntaxKind, SyntaxToken, TupleExpr, VariantPattern, parse_source_file,
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
    let program = match vx_jit::legacy::lower::lower_executable_program(&analysis, &bridge) {
        Ok(p) => p,
        Err(_) => {
            return CompiledEnv {
                fns: HashMap::new(),
            };
        }
    };

    let lowerer = vx_jit::legacy::lower_strict::StrictLowerer::new();
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
    /// An enum variant — tagged struct with enum_name + variant name.
    Variant {
        enum_name: String,
        variant: String,
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
        variant_map: &VariantMap,
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
                    eval_expr(expr, &mut env, functions, compiled, variant_map, trace)
                }
                ThunkInner::Ready(v) => Ok(v.clone()),
            }
        }?;
        *self.0.borrow_mut() = ThunkInner::Ready(val.clone());
        Ok(val)
    }
}

type Env = HashMap<String, SharedThunk>;

/// Map from variant name → enum_name for runtime variant detection.
type VariantMap = HashMap<String, String>;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn evaluate(source: &str, entry: &str) -> Result<(EvalResult, EvalTrace), String> {
    let compiled = build_compiled_env(source);

    let parsed = parse_source_file(source);
    let file = parsed.tree();

    let mut functions: HashMap<String, FnDecl> = HashMap::new();
    let mut variant_map: VariantMap = HashMap::new();
    for item in file.items() {
        match item {
            Item::FnDecl(fn_decl) => {
                if let Some(name) = fn_decl.name() {
                    functions.insert(name.text().to_string(), fn_decl);
                }
            }
            Item::EnumDecl(enum_decl) => {
                collect_enum_variants(&enum_decl, &mut variant_map);
            }
            _ => {}
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
    let value = eval_fn_body(
        &fn_decl,
        &env,
        &functions,
        &compiled,
        &variant_map,
        &mut trace,
    )?;

    let result = match value {
        Value::U64(v) => EvalResult::U64(v),
        Value::Bool(v) => EvalResult::Bool(v),
        Value::String(v) => EvalResult::String(v),
        other => return Err(format!("unsupported result type: {other:?}")),
    };

    Ok((result, trace))
}

fn collect_enum_variants(enum_decl: &EnumDecl, variant_map: &mut VariantMap) {
    let enum_name = match enum_decl.name() {
        Some(n) => n.text().to_string(),
        None => return,
    };
    for variant in enum_decl.variants() {
        if let Some(vname) = variant.name() {
            variant_map.insert(vname.text().to_string(), enum_name.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// AST evaluation (lazy path)
// ---------------------------------------------------------------------------

fn eval_fn_body(
    fn_decl: &FnDecl,
    args: &Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    if let Some(block) = fn_decl.body() {
        return eval_block(&block, args, functions, compiled, variant_map, trace);
    }
    if let Some(body_expr) = fn_decl.body_expr() {
        let mut env = args.clone();
        return eval_expr(
            &body_expr,
            &mut env,
            functions,
            compiled,
            variant_map,
            trace,
        );
    }
    Err("function has no body".to_string())
}

fn eval_block(
    block: &BlockExpr,
    args: &Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    variant_map: &VariantMap,
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
                last_value = Some(eval_expr(
                    &expr,
                    &mut env,
                    functions,
                    compiled,
                    variant_map,
                    trace,
                )?);
            }
            // Type declarations are type-level — no runtime action needed.
            Stmt::StructDecl(_) | Stmt::EnumDecl(_) => {}
            _ => return Err(format!("unsupported statement: {:?}", stmt.syntax().kind())),
        }
    }

    for expr in block.exprs() {
        last_value = Some(eval_expr(
            &expr,
            &mut env,
            functions,
            compiled,
            variant_map,
            trace,
        )?);
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
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    match expr {
        Expr::Literal(lit) => eval_literal(lit),
        Expr::NameRef(name_ref) => {
            eval_name_ref(name_ref, env, functions, compiled, variant_map, trace)
        }
        Expr::Binary(binary) => eval_binary(binary, env, functions, compiled, variant_map, trace),
        Expr::Paren(paren) => {
            let inner = paren.expr().ok_or("empty parens")?;
            eval_expr(&inner, env, functions, compiled, variant_map, trace)
        }
        Expr::Call(call) => eval_call(call, env, functions, compiled, variant_map, trace),
        Expr::If(if_expr) => eval_if(if_expr, env, functions, compiled, variant_map, trace),
        Expr::Tuple(tuple) => eval_tuple(tuple, env),
        Expr::Index(index) => eval_index(index, env, functions, compiled, variant_map, trace),
        Expr::Field(field) => eval_field(field, env, functions, compiled, variant_map, trace),
        Expr::StructLiteral(sl) => eval_struct_literal(&sl, env, variant_map),
        Expr::Match(match_expr) => {
            eval_match(match_expr, env, functions, compiled, variant_map, trace)
        }
        Expr::Block(block) => eval_block(block, env, functions, compiled, variant_map, trace),
        Expr::Closure(closure) => eval_closure(closure, env),
        Expr::Fail(fail) => {
            let msg = if let Some(arg) = fail.message() {
                eval_expr(&arg, env, functions, compiled, variant_map, trace)?
            } else {
                Value::String("explicit failure".into())
            };
            Err(format!("fail: {msg:?}"))
        }
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
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let name = name_ref
        .token()
        .ok_or("name ref without token")?
        .text()
        .to_string();

    if let Some(thunk) = env.get(&name) {
        return thunk.force(functions, compiled, variant_map, trace);
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
    variant_map: &VariantMap,
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

    let lhs_val = eval_expr(&lhs, env, functions, compiled, variant_map, trace)?;
    let rhs_val = eval_expr(&rhs, env, functions, compiled, variant_map, trace)?;

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
    variant_map: &VariantMap,
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
            let closure_val = thunk.force(functions, compiled, variant_map, trace)?;
            if let Value::Closure {
                params,
                body,
                env: closure_env,
            } = closure_val
            {
                let mut call_env: Env = closure_env;
                for (param, arg_expr) in params.iter().zip(args.iter()) {
                    let arg_val =
                        eval_expr(arg_expr, env, functions, compiled, variant_map, trace)?;
                    call_env.insert(param.to_string(), SharedThunk::ready(arg_val));
                }
                return eval_expr(
                    &body,
                    &mut call_env,
                    functions,
                    compiled,
                    variant_map,
                    trace,
                );
            }
        }

        // Compiled function call — Kajit fast path
        if let Some(cfn) = compiled.fns.get(&name) {
            trace.record_enter(&name);
            let mut abi_args = Vec::new();
            for arg_expr in args.iter() {
                let val = eval_expr(arg_expr, env, functions, compiled, variant_map, trace)?;
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
                let arg_val = eval_expr(arg_expr, env, functions, compiled, variant_map, trace)?;
                call_env.insert(param.to_string(), SharedThunk::ready(arg_val));
            }

            return eval_fn_body(&fn_decl, &call_env, functions, compiled, variant_map, trace);
        }

        return Err(format!("unknown function: `{name}`"));
    }

    // Closure call
    let callee_val = eval_expr(&callee, env, functions, compiled, variant_map, trace)?;
    if let Value::Closure {
        params,
        body,
        env: closure_env,
    } = callee_val
    {
        let mut call_env: Env = closure_env;
        for (param, arg_expr) in params.iter().zip(args.iter()) {
            let arg_val = eval_expr(arg_expr, env, functions, compiled, variant_map, trace)?;
            call_env.insert(param.to_string(), SharedThunk::ready(arg_val));
        }
        return eval_expr(
            &body,
            &mut call_env,
            functions,
            compiled,
            variant_map,
            trace,
        );
    }

    Err(format!("callee is not callable: {callee_val:?}"))
}

fn eval_if(
    if_expr: &IfExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let condition = if_expr.condition().ok_or("if without condition")?;
    let cond_val = eval_expr(&condition, env, functions, compiled, variant_map, trace)?;

    let is_true = match cond_val {
        Value::Bool(b) => b,
        _ => return Err(format!("if condition is not bool: {cond_val:?}")),
    };

    if is_true {
        let then_branch = if_expr.then_branch().ok_or("if without then")?;
        eval_expr(&then_branch, env, functions, compiled, variant_map, trace)
    } else {
        let else_branch = if_expr.else_branch().ok_or("if without else")?;
        eval_expr(&else_branch, env, functions, compiled, variant_map, trace)
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
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let receiver = index.receiver().ok_or("index without receiver")?;
    let idx_expr = index.index().ok_or("index without index")?;

    let recv_val = eval_expr(&receiver, env, functions, compiled, variant_map, trace)?;
    let idx_val = eval_expr(&idx_expr, env, functions, compiled, variant_map, trace)?;

    let idx = match idx_val {
        Value::U64(v) => v as usize,
        _ => return Err(format!("index is not u64: {idx_val:?}")),
    };

    match recv_val {
        Value::Product(fields) => {
            if idx < fields.len() {
                fields[idx].force(functions, compiled, variant_map, trace)
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
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let receiver = field.receiver().ok_or("field without receiver")?;
    let field_token = field.field_name().ok_or("field without name")?;
    let field_text = field_token.text().to_string();

    // Try positional access first (_0, _1, ...)
    let idx_str = field_text.strip_prefix('_').unwrap_or(&field_text);
    if let Ok(idx) = idx_str.parse::<usize>() {
        let recv_val = eval_expr(&receiver, env, functions, compiled, variant_map, trace)?;
        return match recv_val {
            Value::Product(fields) => {
                if idx < fields.len() {
                    fields[idx].force(functions, compiled, variant_map, trace)
                } else {
                    Err(format!("field index {idx} out of bounds"))
                }
            }
            _ => Err(format!("field access on non-product: {recv_val:?}")),
        };
    }

    // Named field access — works on structs and variants
    let recv_val = eval_expr(&receiver, env, functions, compiled, variant_map, trace)?;
    match recv_val {
        Value::Struct { name, fields }
        | Value::Variant {
            variant: name,
            fields,
            ..
        } => {
            for (fname, thunk) in &fields {
                if *fname == field_text {
                    return thunk.force(functions, compiled, variant_map, trace);
                }
            }
            Err(format!(
                "`{name}` has no field `{field_text}` (fields: {})",
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

fn eval_struct_literal(
    sl: &StructLiteralExpr,
    env: &Env,
    variant_map: &VariantMap,
) -> Result<Value, String> {
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

    // If this name is a known enum variant, produce Value::Variant
    if let Some(enum_name) = variant_map.get(&name) {
        Ok(Value::Variant {
            enum_name: enum_name.clone(),
            variant: name,
            fields,
        })
    } else {
        Ok(Value::Struct { name, fields })
    }
}

// ---------------------------------------------------------------------------
// Match evaluation
// ---------------------------------------------------------------------------

fn eval_match(
    match_expr: &MatchExpr,
    env: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<Value, String> {
    let scrutinee_expr = match_expr.scrutinee().ok_or("match without scrutinee")?;
    let scrutinee = eval_expr(
        &scrutinee_expr,
        env,
        functions,
        compiled,
        variant_map,
        trace,
    )?;

    for arm in match_expr.arms() {
        let pattern = arm.pattern().ok_or("match arm without pattern")?;
        let mut bindings = Env::new();

        if match_pattern(
            &pattern,
            &scrutinee,
            &mut bindings,
            functions,
            compiled,
            variant_map,
            trace,
        )? {
            // Pattern matched — evaluate the arm body with captured bindings
            let body = arm.expr().ok_or("match arm without body")?;
            let mut arm_env = env.clone();
            arm_env.extend(bindings);
            return eval_expr(&body, &mut arm_env, functions, compiled, variant_map, trace);
        }
    }

    Err("non-exhaustive match: no arm matched".to_string())
}

/// Try to match a value against a pattern, populating `bindings` on success.
/// Returns Ok(true) if the pattern matches, Ok(false) if it doesn't.
/// Field thunks are NOT forced during matching — only bound lazily. This
/// preserves the demand-driven property: dead fields in a matched variant
/// are never evaluated.
fn match_pattern(
    pattern: &Pattern,
    value: &Value,
    bindings: &mut Env,
    functions: &HashMap<String, FnDecl>,
    compiled: &CompiledEnv,
    variant_map: &VariantMap,
    trace: &mut EvalTrace,
) -> Result<bool, String> {
    match pattern {
        // `_` matches anything, binds nothing
        Pattern::Wildcard(_) => Ok(true),

        // `x` matches anything, binds the value
        Pattern::Binding(bp) => {
            let name = bp
                .name()
                .ok_or("binding pattern without name")?
                .text()
                .to_string();
            if name == "_" {
                // underscore-named binding is a wildcard
                return Ok(true);
            }
            bindings.insert(name, SharedThunk::ready(value.clone()));
            Ok(true)
        }

        // Variant pattern: `Some { val }` or `None {}`
        Pattern::Variant(vp) => match_variant_pattern(vp, value, bindings),

        // Struct pattern: `{ x, y }` — matches a struct value by field names
        Pattern::Struct(sp) => {
            let fields = match value {
                Value::Struct { fields, .. } => fields,
                Value::Variant { fields, .. } => fields,
                _ => return Ok(false),
            };
            for field_pat in sp.fields() {
                let fname = field_pat
                    .name()
                    .ok_or("struct pattern field without name")?
                    .text()
                    .to_string();

                // Find the corresponding field thunk in the value
                let thunk = fields.iter().find(|(n, _)| *n == fname).map(|(_, t)| t);

                let Some(thunk) = thunk else {
                    return Ok(false);
                };

                // If the field pattern has a nested pattern, force and recurse.
                // Otherwise, bind the thunk lazily (preserving demand-driven eval).
                if let Some(nested) = field_pat.pattern() {
                    let forced = thunk.force(functions, compiled, variant_map, trace)?;
                    if !match_pattern(
                        &nested,
                        &forced,
                        bindings,
                        functions,
                        compiled,
                        variant_map,
                        trace,
                    )? {
                        return Ok(false);
                    }
                } else {
                    // Shorthand: `{ val }` means bind `val` to the field's thunk
                    bindings.insert(fname, thunk.clone());
                }
            }
            Ok(true)
        }

        // Literal pattern: `42`, `"hello"`, `true`
        Pattern::Literal(lp) => {
            let token = lp.token().ok_or("literal pattern without token")?;
            match (token.kind(), value) {
                (SyntaxKind::INT, Value::U64(v)) => {
                    let pat_val: u64 = token
                        .text()
                        .parse()
                        .map_err(|e| format!("bad int pattern: {e}"))?;
                    Ok(*v == pat_val)
                }
                (SyntaxKind::STRING, Value::String(v)) => {
                    let text = token.text();
                    let inner = &text[1..text.len() - 1];
                    Ok(*v == inner)
                }
                (SyntaxKind::TRUE_KW, Value::Bool(v)) => Ok(*v),
                (SyntaxKind::FALSE_KW, Value::Bool(v)) => Ok(!*v),
                _ => Ok(false),
            }
        }

        // Unit pattern: `()` — matches nothing useful yet, treat as wildcard
        Pattern::Unit(_) => Ok(true),

        // Parenthesized pattern: `(p)` — unwrap and recurse
        Pattern::Paren(pp) => {
            let inner = pp.inner().ok_or("paren pattern without inner")?;
            match_pattern(
                &inner,
                value,
                bindings,
                functions,
                compiled,
                variant_map,
                trace,
            )
        }

        _ => Err(format!(
            "unsupported pattern kind: {:?}",
            pattern.syntax().kind()
        )),
    }
}

/// Match a variant pattern like `Some { val }` or `None {}` against a value.
fn match_variant_pattern(
    vp: &VariantPattern,
    value: &Value,
    bindings: &mut Env,
) -> Result<bool, String> {
    // Collect the path segments (e.g., ["Some"] or ["Option", "Some"])
    let segments: Vec<String> = vp.segments().map(|t| t.text().to_string()).collect();
    let variant_name = segments.last().ok_or("variant pattern without name")?;

    // Check if the value is a Variant with a matching variant name
    let (value_fields, matches) = match value {
        Value::Variant {
            variant, fields, ..
        } => (fields, variant == variant_name),
        // Also allow matching a Struct by name (for single-variant enums or
        // patterns that name a struct directly)
        Value::Struct { name, fields } => (fields, name == variant_name),
        _ => return Ok(false),
    };

    if !matches {
        return Ok(false);
    }

    // If the variant pattern has struct fields, bind them
    if let Some(struct_pat) = vp.fields() {
        for field_pat in struct_pat.fields() {
            let fname = field_pat
                .name()
                .ok_or("variant field pattern without name")?
                .text()
                .to_string();

            let thunk = value_fields
                .iter()
                .find(|(n, _)| *n == fname)
                .map(|(_, t)| t);

            let Some(thunk) = thunk else {
                return Ok(false);
            };

            // Shorthand field binding — lazy, does not force the thunk
            bindings.insert(fname, thunk.clone());
        }
    }

    Ok(true)
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
