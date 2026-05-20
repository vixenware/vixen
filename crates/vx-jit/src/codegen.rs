use std::collections::HashMap;
use std::rc::Rc;

use cranelift_codegen::Context;
use cranelift_codegen::ir::{
    AbiParam, Function, InstBuilder, Signature, UserFuncName, Value,
    condcodes::IntCC,
    types::{self, I8, I64},
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use thiserror::Error;
use vx_jit_types::{
    LocalId, StrictBinOp, StrictExpr, StrictFunction, StrictLiteral, StrictStmt, StrictType,
};

use crate::fragment::CompiledFragment;

pub struct CompileOutput {
    pub fragment: CompiledFragment,
    pub clif_dump: Option<String>,
}

#[derive(Debug)]
pub struct CompileFailure {
    pub error: CompileError,
    pub clif_dump: Option<String>,
}

#[derive(Debug, Error)]
pub enum CompileError {
    #[error("arity mismatch: expected {expected} args, got {actual}")]
    ArityMismatch { expected: usize, actual: usize },
    #[error("argument {index} has wrong type")]
    ArgTypeMismatch { index: usize },
    #[error("module error: {0}")]
    Module(String),
    #[error("string result was not valid UTF-8")]
    InvalidUtf8,
}

impl From<cranelift_module::ModuleError> for CompileError {
    fn from(e: cranelift_module::ModuleError) -> Self {
        CompileError::Module(e.to_string())
    }
}

/// Compile a StrictFunction into a CompiledFragment.
pub fn compile(func: &StrictFunction) -> Result<CompiledFragment, CompileError> {
    compile_with_dump(func, false)
        .map(|output| output.fragment)
        .map_err(|failure| failure.error)
}

pub fn compile_with_dump(
    func: &StrictFunction,
    include_clif_dump: bool,
) -> Result<CompileOutput, CompileFailure> {
    let mut flag_builder = settings::builder();
    flag_builder.set("use_colocated_libcalls", "false").unwrap();
    flag_builder.set("is_pic", "false").unwrap();
    let isa_flags = settings::Flags::new(flag_builder);
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(isa_flags)
        .unwrap();

    let mut jit_builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    // Register runtime callables used by ConcatStrings.
    jit_builder.symbol("__vx_alloc", vx_alloc as *const u8);
    jit_builder.symbol("__vx_memcpy_advance", vx_memcpy_advance as *const u8);

    let mut module = JITModule::new(jit_builder);

    let PreparedFunction {
        func_id,
        mut context,
        literal_storage,
    } = declare_and_build_context(func, &mut module).map_err(|error| CompileFailure {
        error,
        clif_dump: None,
    })?;

    let clif_dump = include_clif_dump.then(|| context.func.display().to_string());

    module
        .define_function(func_id, &mut context)
        .map_err(|e| CompileFailure {
            error: CompileError::Module(e.to_string()),
            clif_dump: clif_dump.clone(),
        })?;

    module.finalize_definitions().map_err(|e| CompileFailure {
        error: CompileError::Module(e.to_string()),
        clif_dump: clif_dump.clone(),
    })?;

    let func_ptr = module.get_finalized_function(func_id);

    Ok(CompileOutput {
        fragment: CompiledFragment::new(func_ptr, func, Rc::new(module), literal_storage),
        clif_dump,
    })
}

fn build_signature(module: &JITModule, func: &StrictFunction) -> Signature {
    let mut sig = module.make_signature();
    for param in &func.params {
        match param.ty {
            StrictType::U64 => sig.params.push(AbiParam::new(I64)),
            // The raw trampoline passes scalar slots as u64 words, so Bool uses
            // the same word-sized ABI at the function boundary and is narrowed
            // after entry.
            StrictType::Bool => sig.params.push(AbiParam::new(I64)),
            StrictType::String => {
                sig.params.push(AbiParam::new(I64)); // ptr
                sig.params.push(AbiParam::new(I64)); // len
            }
        }
    }
    match func.return_type {
        StrictType::U64 => sig.returns.push(AbiParam::new(I64)),
        StrictType::Bool => sig.returns.push(AbiParam::new(I64)),
        StrictType::String => {
            sig.returns.push(AbiParam::new(I64)); // ptr
            sig.returns.push(AbiParam::new(I64)); // len
        }
    }
    sig
}

struct PreparedFunction {
    func_id: FuncId,
    context: Context,
    literal_storage: Vec<Box<[u8]>>,
}

fn declare_and_build_context(
    func: &StrictFunction,
    module: &mut JITModule,
) -> Result<PreparedFunction, CompileError> {
    let sig = build_signature(module, func);
    let func_id = module.declare_function(&func.name, Linkage::Export, &sig)?;

    // Declare the runtime helpers.
    let alloc_sig = {
        let mut s = module.make_signature();
        s.params.push(AbiParam::new(I64)); // size
        s.params.push(AbiParam::new(I64)); // align
        s.returns.push(AbiParam::new(I64)); // ptr
        s
    };
    let alloc_id = module.declare_function("__vx_alloc", Linkage::Import, &alloc_sig)?;

    let memcpy_sig = {
        let mut s = module.make_signature();
        s.params.push(AbiParam::new(I64)); // dst
        s.params.push(AbiParam::new(I64)); // src
        s.params.push(AbiParam::new(I64)); // len
        s.returns.push(AbiParam::new(I64)); // new dst (dst + len)
        s
    };
    let memcpy_id = module.declare_function("__vx_memcpy_advance", Linkage::Import, &memcpy_sig)?;

    let mut context = module.make_context();
    let mut func_ctx = FunctionBuilderContext::new();

    context.func = Function::with_name_signature(UserFuncName::user(0, func_id.as_u32()), sig);

    let literal_storage = {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut func_ctx);

        // Allocate Cranelift Variables for each local.
        // String locals use two Variables: one for ptr, one for len.
        let mut local_vars: HashMap<LocalId, LocalSlot> = HashMap::new();
        let mut literal_storage = Vec::new();

        let alloc_var = |builder: &mut FunctionBuilder<'_>, ty: types::Type| -> Variable {
            builder.declare_var(ty)
        };

        // Declare variables for params and locals.
        for param in &func.params {
            match param.ty {
                StrictType::U64 => {
                    let v = alloc_var(&mut builder, I64);
                    local_vars.insert(param.local, LocalSlot::Single(v));
                }
                StrictType::Bool => {
                    let v = alloc_var(&mut builder, I8);
                    local_vars.insert(param.local, LocalSlot::Single(v));
                }
                StrictType::String => {
                    let vp = alloc_var(&mut builder, I64);
                    let vl = alloc_var(&mut builder, I64);
                    local_vars.insert(param.local, LocalSlot::String { ptr: vp, len: vl });
                }
            }
        }
        for local in &func.locals {
            match local.ty {
                StrictType::U64 => {
                    let v = alloc_var(&mut builder, I64);
                    local_vars.insert(local.id, LocalSlot::Single(v));
                }
                StrictType::Bool => {
                    let v = alloc_var(&mut builder, I8);
                    local_vars.insert(local.id, LocalSlot::Single(v));
                }
                StrictType::String => {
                    let vp = alloc_var(&mut builder, I64);
                    let vl = alloc_var(&mut builder, I64);
                    local_vars.insert(local.id, LocalSlot::String { ptr: vp, len: vl });
                }
            }
        }

        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Bind params to their Variables.
        let entry_params: Vec<Value> = builder.block_params(entry_block).to_vec();
        let mut param_word_idx = 0;
        for param in &func.params {
            match param.ty {
                StrictType::U64 => {
                    let v = entry_params[param_word_idx];
                    param_word_idx += 1;
                    let slot = local_vars[&param.local];
                    let var = slot.single();
                    builder.def_var(var, v);
                }
                StrictType::Bool => {
                    let v = entry_params[param_word_idx];
                    param_word_idx += 1;
                    let narrowed = builder.ins().ireduce(I8, v);
                    let slot = local_vars[&param.local];
                    let var = slot.single();
                    builder.def_var(var, narrowed);
                }
                StrictType::String => {
                    let vp = entry_params[param_word_idx];
                    let vl = entry_params[param_word_idx + 1];
                    param_word_idx += 2;
                    let slot = local_vars[&param.local];
                    let (pvar, lvar) = slot.string();
                    builder.def_var(pvar, vp);
                    builder.def_var(lvar, vl);
                }
            }
        }

        let alloc_ref = module.declare_func_in_func(alloc_id, builder.func);
        let memcpy_ref = module.declare_func_in_func(memcpy_id, builder.func);

        {
            let mut cx = CodegenCtx {
                builder: &mut builder,
                local_vars: &local_vars,
                alloc_ref,
                memcpy_ref,
                return_type: func.return_type,
                literal_storage: &mut literal_storage,
            };
            emit_stmts(&mut cx, &func.body);
        }

        builder.finalize();
        literal_storage
    };

    Ok(PreparedFunction {
        func_id,
        context,
        literal_storage,
    })
}

/// Either a single Cranelift Variable (U64/Bool) or a (ptr, len) Variable pair (String).
#[derive(Clone, Copy)]
enum LocalSlot {
    Single(Variable),
    String { ptr: Variable, len: Variable },
}

impl LocalSlot {
    fn single(self) -> Variable {
        match self {
            LocalSlot::Single(v) => v,
            LocalSlot::String { .. } => panic!("expected single-word local"),
        }
    }

    fn string(self) -> (Variable, Variable) {
        match self {
            LocalSlot::String { ptr, len } => (ptr, len),
            LocalSlot::Single(_) => panic!("expected string local"),
        }
    }
}

/// A compiled expression value — either one word or a (ptr, len) pair.
enum ExprVal {
    Single(Value),
    String { ptr: Value, len: Value },
}

impl ExprVal {
    fn single(self) -> Value {
        match self {
            ExprVal::Single(v) => v,
            ExprVal::String { .. } => panic!("expected single-word value"),
        }
    }

    fn string(self) -> (Value, Value) {
        match self {
            ExprVal::String { ptr, len } => (ptr, len),
            ExprVal::Single(_) => panic!("expected string value"),
        }
    }
}

struct CodegenCtx<'a, 'b> {
    builder: &'a mut FunctionBuilder<'b>,
    local_vars: &'a HashMap<LocalId, LocalSlot>,
    alloc_ref: cranelift_codegen::ir::FuncRef,
    memcpy_ref: cranelift_codegen::ir::FuncRef,
    return_type: StrictType,
    literal_storage: &'a mut Vec<Box<[u8]>>,
}

fn emit_stmts(cx: &mut CodegenCtx<'_, '_>, stmts: &[StrictStmt]) {
    for stmt in stmts {
        emit_stmt(cx, stmt);
    }
}

fn emit_stmt(cx: &mut CodegenCtx<'_, '_>, stmt: &StrictStmt) {
    match stmt {
        StrictStmt::Let { local, value } => {
            let val = emit_expr(cx, value);
            let slot = cx.local_vars[local];
            match (val, slot) {
                (ExprVal::Single(v), LocalSlot::Single(var)) => {
                    cx.builder.def_var(var, v);
                }
                (
                    ExprVal::String { ptr, len },
                    LocalSlot::String {
                        ptr: pvar,
                        len: lvar,
                    },
                ) => {
                    cx.builder.def_var(pvar, ptr);
                    cx.builder.def_var(lvar, len);
                }
                _ => panic!("type mismatch in Let"),
            }
        }
        StrictStmt::If {
            condition,
            then_body,
            else_body,
        } => {
            let cond_val = emit_expr(cx, condition).single();

            let then_block = cx.builder.create_block();
            let else_block = cx.builder.create_block();
            let merge_block = cx.builder.create_block();

            cx.builder
                .ins()
                .brif(cond_val, then_block, &[], else_block, &[]);

            cx.builder.switch_to_block(then_block);
            cx.builder.seal_block(then_block);
            emit_stmts(cx, then_body);
            let then_terminated = block_is_terminated(cx.builder);
            if !then_terminated {
                cx.builder.ins().jump(merge_block, &[]);
            }

            cx.builder.switch_to_block(else_block);
            cx.builder.seal_block(else_block);
            emit_stmts(cx, else_body);
            let else_terminated = block_is_terminated(cx.builder);
            if !else_terminated {
                cx.builder.ins().jump(merge_block, &[]);
            }

            // Only switch to merge_block if it has at least one predecessor.
            if !then_terminated || !else_terminated {
                cx.builder.switch_to_block(merge_block);
                cx.builder.seal_block(merge_block);
            }
        }
        StrictStmt::Return(expr) => match expr {
            None => {
                cx.builder.ins().return_(&[]);
            }
            Some(e) => {
                let val = emit_expr(cx, e);
                match val {
                    ExprVal::Single(v) if cx.return_type == StrictType::Bool => {
                        let widened = cx.builder.ins().uextend(I64, v);
                        cx.builder.ins().return_(&[widened]);
                    }
                    ExprVal::Single(v) => {
                        cx.builder.ins().return_(&[v]);
                    }
                    ExprVal::String { ptr, len } => {
                        cx.builder.ins().return_(&[ptr, len]);
                    }
                }
            }
        },
    }
}

fn emit_expr(cx: &mut CodegenCtx<'_, '_>, expr: &StrictExpr) -> ExprVal {
    match expr {
        StrictExpr::Literal(lit) => emit_literal(cx, lit),
        StrictExpr::Local(id) => {
            let slot = cx.local_vars[id];
            match slot {
                LocalSlot::Single(var) => ExprVal::Single(cx.builder.use_var(var)),
                LocalSlot::String { ptr, len } => ExprVal::String {
                    ptr: cx.builder.use_var(ptr),
                    len: cx.builder.use_var(len),
                },
            }
        }
        StrictExpr::Binary { op, lhs, rhs } => {
            let lv = emit_expr(cx, lhs).single();
            let rv = emit_expr(cx, rhs).single();
            emit_binary(cx, *op, lv, rv)
        }
        StrictExpr::StringLen { value } => {
            let (_, len) = emit_expr(cx, value).string();
            ExprVal::Single(len)
        }
        StrictExpr::StringSlice { value, start, end } => {
            let (ptr, _) = emit_expr(cx, value).string();
            let start_v = emit_expr(cx, start).single();
            let end_v = emit_expr(cx, end).single();
            let new_ptr = cx.builder.ins().iadd(ptr, start_v);
            let new_len = cx.builder.ins().isub(end_v, start_v);
            ExprVal::String {
                ptr: new_ptr,
                len: new_len,
            }
        }
        StrictExpr::ConcatStrings { parts } => emit_concat(cx, parts),
    }
}

fn emit_literal(cx: &mut CodegenCtx<'_, '_>, lit: &StrictLiteral) -> ExprVal {
    match lit {
        StrictLiteral::U64(v) => {
            let val = cx.builder.ins().iconst(I64, *v as i64);
            ExprVal::Single(val)
        }
        StrictLiteral::Bool(b) => {
            let val = cx.builder.ins().iconst(I8, i64::from(*b));
            ExprVal::Single(val)
        }
        StrictLiteral::String(s) => {
            let bytes = s.as_bytes().to_vec().into_boxed_slice();
            let ptr_val = cx.builder.ins().iconst(I64, bytes.as_ptr() as i64);
            let len_val = cx.builder.ins().iconst(I64, bytes.len() as i64);
            cx.literal_storage.push(bytes);
            ExprVal::String {
                ptr: ptr_val,
                len: len_val,
            }
        }
    }
}

fn emit_binary(cx: &mut CodegenCtx<'_, '_>, op: StrictBinOp, lv: Value, rv: Value) -> ExprVal {
    let result = match op {
        StrictBinOp::Add => cx.builder.ins().iadd(lv, rv),
        StrictBinOp::Sub => cx.builder.ins().isub(lv, rv),
        StrictBinOp::Eq => {
            // icmp returns I8 in Cranelift.
            cx.builder.ins().icmp(IntCC::Equal, lv, rv)
        }
        StrictBinOp::Ne => cx.builder.ins().icmp(IntCC::NotEqual, lv, rv),
    };
    ExprVal::Single(result)
}

fn emit_concat(cx: &mut CodegenCtx<'_, '_>, parts: &[StrictExpr]) -> ExprVal {
    if parts.is_empty() {
        // Empty concat → static empty string.
        static EMPTY: [u8; 0] = [];
        let ptr = cx.builder.ins().iconst(I64, EMPTY.as_ptr() as i64);
        let len = cx.builder.ins().iconst(I64, 0);
        return ExprVal::String { ptr, len };
    }

    // Evaluate all parts first.
    let part_vals: Vec<(Value, Value)> = parts.iter().map(|p| emit_expr(cx, p).string()).collect();

    // Sum lengths.
    let mut total_len = part_vals[0].1;
    for &(_, len) in &part_vals[1..] {
        total_len = cx.builder.ins().iadd(total_len, len);
    }

    // Allocate buffer: __vx_alloc(total_len, 1).
    let align_one = cx.builder.ins().iconst(I64, 1);
    let alloc_call = cx.builder.ins().call(cx.alloc_ref, &[total_len, align_one]);
    let buf_ptr = cx.builder.inst_results(alloc_call)[0];

    // Memcpy each part, advancing the cursor.
    let mut cursor = buf_ptr;
    for &(src_ptr, src_len) in &part_vals {
        let memcpy_call = cx
            .builder
            .ins()
            .call(cx.memcpy_ref, &[cursor, src_ptr, src_len]);
        cursor = cx.builder.inst_results(memcpy_call)[0];
    }

    ExprVal::String {
        ptr: buf_ptr,
        len: total_len,
    }
}

fn block_is_terminated(builder: &FunctionBuilder<'_>) -> bool {
    let block = builder.current_block().unwrap();
    builder
        .func
        .layout
        .last_inst(block)
        .map(|inst| builder.func.dfg.insts[inst].opcode().is_terminator())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Runtime stubs registered with the JIT module
// ---------------------------------------------------------------------------

unsafe extern "C" fn vx_alloc(size: i64, align: i64) -> i64 {
    let layout = std::alloc::Layout::from_size_align(size as usize, align as usize).unwrap();
    unsafe { std::alloc::alloc(layout) as i64 }
}

unsafe extern "C" fn vx_memcpy_advance(dst: i64, src: i64, len: i64) -> i64 {
    unsafe {
        std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, len as usize);
    }
    dst + len
}
